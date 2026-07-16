use super::*;
use crate::model::{
    PIPELINE_DIAGNOSTICS_SCHEMA_VERSION, PipelineDiagnosticsV1, PipelineStageAggregateV1,
    PipelineStageSampleV1, PipelineStageV1,
};

impl WorkspaceStore {
    /// Aggregates pipeline samples in bounded memory. The aggregate is flushed
    /// at coordinated shutdown and whenever a diagnostic snapshot is read.
    pub fn record_pipeline_stages(
        &self,
        samples: &[PipelineStageSampleV1],
    ) -> Result<(), StoreError> {
        if samples.is_empty() {
            return Ok(());
        }
        let now = now_unix_ms();
        let mut aggregates = self
            .pipeline_metrics
            .lock()
            .expect("pipeline metrics lock poisoned");
        for sample in samples {
            let aggregate =
                aggregates
                    .entry(sample.stage)
                    .or_insert_with(|| PipelineStageAggregateV1 {
                        stage: sample.stage,
                        sample_count: 0,
                        total_duration_nano: 0,
                        max_duration_nano: 0,
                        item_count: 0,
                        byte_count: 0,
                        rows_scanned: 0,
                        rows_deserialized: 0,
                        updated_at_unix_ms: now,
                    });
            aggregate.sample_count = aggregate.sample_count.saturating_add(1);
            aggregate.total_duration_nano = aggregate
                .total_duration_nano
                .saturating_add(sample.duration_nano);
            aggregate.max_duration_nano = aggregate.max_duration_nano.max(sample.duration_nano);
            aggregate.item_count = aggregate.item_count.saturating_add(sample.item_count);
            aggregate.byte_count = aggregate.byte_count.saturating_add(sample.byte_count);
            aggregate.rows_scanned = aggregate.rows_scanned.saturating_add(sample.rows_scanned);
            aggregate.rows_deserialized = aggregate
                .rows_deserialized
                .saturating_add(sample.rows_deserialized);
            aggregate.updated_at_unix_ms = now;
        }
        Ok(())
    }

    pub fn flush_pipeline_stages(&self) -> Result<(), StoreError> {
        let pending = {
            let mut aggregates = self
                .pipeline_metrics
                .lock()
                .expect("pipeline metrics lock poisoned");
            std::mem::take(&mut *aggregates)
        };
        if pending.is_empty() {
            return Ok(());
        }
        let result = self.persist_pipeline_stages(pending.values());
        if result.is_err() {
            let mut aggregates = self
                .pipeline_metrics
                .lock()
                .expect("pipeline metrics lock poisoned");
            for (stage, pending) in pending {
                match aggregates.entry(stage) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(pending);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        merge_aggregate(entry.get_mut(), &pending);
                    }
                }
            }
        }
        result
    }

    fn persist_pipeline_stages<'a>(
        &self,
        stages: impl IntoIterator<Item = &'a PipelineStageAggregateV1>,
    ) -> Result<(), StoreError> {
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let mut statement = transaction.prepare(
            "INSERT INTO pipeline_stage_metrics(
                workspace_id, stage, sample_count, total_duration_nano, max_duration_nano,
                item_count, byte_count, rows_scanned, rows_deserialized, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(workspace_id, stage) DO UPDATE SET
                sample_count = sample_count + excluded.sample_count,
                total_duration_nano = total_duration_nano + excluded.total_duration_nano,
                max_duration_nano = MAX(max_duration_nano, excluded.max_duration_nano),
                item_count = item_count + excluded.item_count,
                byte_count = byte_count + excluded.byte_count,
                rows_scanned = rows_scanned + excluded.rows_scanned,
                rows_deserialized = rows_deserialized + excluded.rows_deserialized,
                updated_at_unix_ms = MAX(updated_at_unix_ms, excluded.updated_at_unix_ms)",
        )?;
        for stage in stages {
            statement.execute(params![
                self.workspace_id,
                stage.stage.as_str(),
                bounded_i64(stage.sample_count),
                bounded_i64(stage.total_duration_nano),
                bounded_i64(stage.max_duration_nano),
                bounded_i64(stage.item_count),
                bounded_i64(stage.byte_count),
                bounded_i64(stage.rows_scanned),
                bounded_i64(stage.rows_deserialized),
                stage.updated_at_unix_ms,
            ])?;
        }
        drop(statement);
        transaction.commit()?;
        Ok(())
    }

    pub fn pipeline_diagnostics(&self) -> Result<PipelineDiagnosticsV1, StoreError> {
        self.flush_pipeline_stages()?;
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT stage, sample_count, total_duration_nano, max_duration_nano,
                    item_count, byte_count, rows_scanned, rows_deserialized, updated_at_unix_ms
             FROM pipeline_stage_metrics WHERE workspace_id = ?1 ORDER BY stage",
        )?;
        let stages = statement
            .query_map(params![self.workspace_id], |row| {
                let stage: String = row.get(0)?;
                Ok((
                    stage,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })?
            .map(|row| {
                let (stage, samples, total, maximum, items, bytes, scanned, deserialized, updated) =
                    row?;
                Ok(PipelineStageAggregateV1 {
                    stage: stage
                        .parse::<PipelineStageV1>()
                        .map_err(StoreError::Invalid)?,
                    sample_count: nonnegative_u64(samples),
                    total_duration_nano: nonnegative_u64(total),
                    max_duration_nano: nonnegative_u64(maximum),
                    item_count: nonnegative_u64(items),
                    byte_count: nonnegative_u64(bytes),
                    rows_scanned: nonnegative_u64(scanned),
                    rows_deserialized: nonnegative_u64(deserialized),
                    updated_at_unix_ms: updated,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        drop(statement);

        let now = now_unix_ms();
        let (journal_backlog_rows, journal_backlog_oldest_age_ms) = control.query_row(
            "SELECT COUNT(*), COALESCE(MAX(0, ?1 - MIN(received_at_unix_ms)), 0)
             FROM ingest_journal WHERE workspace_id = ?2 AND projected = 0",
            params![now, self.workspace_id],
            |row| Ok((nonnegative_u64(row.get(0)?), nonnegative_u64(row.get(1)?))),
        )?;
        let (analysis_backlog_rows, analysis_backlog_oldest_age_ms) = control.query_row(
            "SELECT COUNT(*), COALESCE(MAX(0, ?1 - MIN(last_committed_unix_ms)), 0)
             FROM logical_traces WHERE workspace_id = ?2 AND lifecycle = 'finalized'
               AND analysis_status IN ('pending', 'reanalyzing')",
            params![now, self.workspace_id],
            |row| Ok((nonnegative_u64(row.get(0)?), nonnegative_u64(row.get(1)?))),
        )?;
        let feature_similarity_models_built =
            control.query_row("SELECT COUNT(*) FROM semantic_cluster_models", [], |row| {
                row.get::<_, i64>(0).map(nonnegative_u64)
            })?;
        let feature_similarity_assignments_written = control.query_row(
            "SELECT COUNT(*) FROM semantic_cluster_assignments",
            [],
            |row| row.get::<_, i64>(0).map(nonnegative_u64),
        )?;
        Ok(PipelineDiagnosticsV1 {
            schema_version: PIPELINE_DIAGNOSTICS_SCHEMA_VERSION.into(),
            stages,
            journal_backlog_rows,
            journal_backlog_oldest_age_ms,
            analysis_backlog_rows,
            analysis_backlog_oldest_age_ms,
            feature_similarity_models_built,
            feature_similarity_assignments_written,
        })
    }
}

fn merge_aggregate(target: &mut PipelineStageAggregateV1, source: &PipelineStageAggregateV1) {
    target.sample_count = target.sample_count.saturating_add(source.sample_count);
    target.total_duration_nano = target
        .total_duration_nano
        .saturating_add(source.total_duration_nano);
    target.max_duration_nano = target.max_duration_nano.max(source.max_duration_nano);
    target.item_count = target.item_count.saturating_add(source.item_count);
    target.byte_count = target.byte_count.saturating_add(source.byte_count);
    target.rows_scanned = target.rows_scanned.saturating_add(source.rows_scanned);
    target.rows_deserialized = target
        .rows_deserialized
        .saturating_add(source.rows_deserialized);
    target.updated_at_unix_ms = target.updated_at_unix_ms.max(source.updated_at_unix_ms);
}

fn bounded_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn nonnegative_u64(value: i64) -> u64 {
    value.max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::WorkspaceStoreLayout;

    #[test]
    fn stage_metrics_are_bounded_aggregates_with_backlog_diagnostics() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let store = WorkspaceStore::open(
            &WorkspaceStoreLayout::new(temporary.path()),
            "diagnostics-workspace",
        )
        .expect("open store");
        let mut first = PipelineStageSampleV1::new(PipelineStageV1::Decode, 10);
        first.item_count = 2;
        let mut second = PipelineStageSampleV1::new(PipelineStageV1::Decode, 30);
        second.item_count = 3;

        store
            .record_pipeline_stages(&[first, second])
            .expect("record stages");
        let diagnostics = store.pipeline_diagnostics().expect("diagnostics");

        assert_eq!(diagnostics.stages.len(), 1);
        assert_eq!(diagnostics.stages[0].stage, PipelineStageV1::Decode);
        assert_eq!(diagnostics.stages[0].sample_count, 2);
        assert_eq!(diagnostics.stages[0].total_duration_nano, 40);
        assert_eq!(diagnostics.stages[0].max_duration_nano, 30);
        assert_eq!(diagnostics.stages[0].item_count, 5);
        assert_eq!(diagnostics.journal_backlog_rows, 0);
        assert_eq!(diagnostics.analysis_backlog_rows, 0);

        drop(store);
        let reopened = WorkspaceStore::open(
            &WorkspaceStoreLayout::new(temporary.path()),
            "diagnostics-workspace",
        )
        .expect("reopen store");
        let persisted = reopened
            .pipeline_diagnostics()
            .expect("persisted diagnostics");
        assert_eq!(persisted.stages[0].sample_count, 2);
        assert_eq!(persisted.stages[0].total_duration_nano, 40);
    }
}

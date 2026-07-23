use duckdb::params as duck_params;

use super::{StoreError, WorkspaceStore, persisted_json, topology::has_persisted_topology};
use crate::model::{SpanRow, SpanTreePageV1};

impl WorkspaceStore {
    /// Returns one bounded page from the persisted topology. `None` requests roots; a span id
    /// requests its direct children. Live traces without a finalized projection use external
    /// parent identities and keep missing-parent spans visible as roots.
    pub fn span_tree_page(
        &self,
        logical_trace_id: &str,
        revision: u64,
        parent_span_id: Option<&str>,
        offset: u64,
        limit: u32,
    ) -> Result<SpanTreePageV1, StoreError> {
        let analytics = self.analytics_reads.connection();
        let persisted = has_persisted_topology(&analytics, logical_trace_id, revision)?;
        let relation = match (parent_span_id, persisted) {
            (Some(_), true) => "span.parent_span_id = ?3 AND COALESCE(span.topology_depth, 0) > 0",
            (Some(_), false) => "span.parent_span_id = ?3",
            (None, true) => "COALESCE(span.topology_depth, 0) = 0 AND ?3 = ?3",
            (None, false) => {
                "(span.parent_span_id IS NULL OR span.parent_span_id = '' OR NOT EXISTS (
                    SELECT 1 FROM spans AS parent
                    WHERE parent.logical_trace_id = span.logical_trace_id
                      AND parent.revision = span.revision
                      AND parent.span_id = span.parent_span_id
                      AND parent.is_current = TRUE
                )) AND ?3 = ?3"
            }
        };
        let parent = parent_span_id.unwrap_or("");
        let base = format!(
            " FROM spans AS span
              WHERE span.logical_trace_id = ?1 AND span.revision = ?2
                AND span.is_current = TRUE AND {relation}"
        );
        let total = analytics.query_row(
            &format!("SELECT COUNT(*){base}"),
            duck_params![logical_trace_id, revision as i64, parent],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let query = format!(
            "SELECT span.logical_trace_id, span.revision, span.span_id, span.parent_span_id,
                    span.name, span.category, span.start_time_unix_nano, span.duration_nano,
                    span.status_code, span.status_message, span.attributes_json,
                    span.payload_refs_json, span.topology_depth, span.topology_has_children
             {base}
             ORDER BY span.topology_order NULLS LAST, span.start_time_unix_nano, span.span_id
             LIMIT ?4 OFFSET ?5"
        );
        let mut statement = analytics.prepare(&query)?;
        let mut rows = statement
            .query_map(
                duck_params![
                    logical_trace_id,
                    revision as i64,
                    parent,
                    limit as i64,
                    offset as i64
                ],
                map_span_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if !persisted {
            let span_ids = rows
                .iter()
                .map(|span| span.span_id.clone())
                .collect::<Vec<_>>();
            let annotations =
                self.live_topology_annotations(&analytics, logical_trace_id, revision, &span_ids)?;
            for (span, (depth, has_children)) in rows.iter_mut().zip(annotations) {
                span.depth = depth;
                span.has_children = has_children;
            }
        }
        Ok(SpanTreePageV1 {
            parent_span_id: parent_span_id.map(str::to_owned),
            offset,
            total,
            rows,
        })
    }
}

fn map_span_row(row: &duckdb::Row<'_>) -> duckdb::Result<SpanRow> {
    let attributes_json: String = row.get(10)?;
    let payload_refs_json: String = row.get(11)?;
    Ok(SpanRow {
        logical_trace_id: row.get(0)?,
        revision: row.get::<_, i64>(1)? as u64,
        span_id: row.get(2)?,
        parent_span_id: row.get(3)?,
        name: row.get(4)?,
        category: row.get(5)?,
        start_time_unix_nano: row.get::<_, i64>(6)? as u64,
        duration_nano: row.get::<_, i64>(7)? as u64,
        status_code: row.get(8)?,
        status_message: row.get(9)?,
        attributes: persisted_json::decode_json_column(&attributes_json, 10, "span attributes")?,
        payload_refs: persisted_json::decode_json_column(
            &payload_refs_json,
            11,
            "span payload references",
        )?,
        depth: row
            .get::<_, Option<i64>>(12)?
            .map(|depth| depth as u32)
            .unwrap_or_default(),
        has_children: row.get::<_, Option<bool>>(13)?.unwrap_or(false),
        events: Vec::new(),
        links: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::layout::WorkspaceStoreLayout;
    use serde_json::Value;

    use crate::model::{
        SPAN_UPSERT_SCHEMA_VERSION, SpanUpsertBatchV1, SpanUpsertV1, TraceLifecycle,
    };

    use super::*;

    fn span(id: &str, parent: Option<&str>, start: u64) -> SpanUpsertV1 {
        SpanUpsertV1 {
            schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
            source_id: "source".into(),
            external_trace_id: "trace".into(),
            external_span_id: id.into(),
            external_parent_span_id: parent.map(str::to_owned),
            logical_trace_id: "trace".into(),
            content_hash: String::new(),
            observed_at_unix_nano: start,
            name: id.into(),
            category: "agent".into(),
            span_kind: 0,
            start_time_unix_nano: start,
            end_time_unix_nano: start + 1,
            status_code: 0,
            status_message: String::new(),
            trace_state: String::new(),
            flags: 0,
            dropped_attributes_count: 0,
            dropped_events_count: 0,
            dropped_links_count: 0,
            resource: BTreeMap::from([
                ("service.name".into(), Value::String("tree-test".into())),
                (
                    "perseval.project.id".into(),
                    Value::String("project".into()),
                ),
            ]),
            scope: BTreeMap::new(),
            attributes: BTreeMap::new(),
            payload_refs: BTreeMap::new(),
            payload_identities: BTreeMap::new(),
            events: Vec::new(),
            links: Vec::new(),
            decoder_version: "test".into(),
            semantic_mapping_version: "test".into(),
        }
    }

    #[test]
    fn pages_roots_and_direct_children_without_flattening_the_trace() {
        let path = std::env::temp_dir().join(format!(
            "perseval-span-tree-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let layout = WorkspaceStoreLayout::new(path);
        let store = WorkspaceStore::open(&layout, "workspace").unwrap();
        let mut batch = SpanUpsertBatchV1 {
            schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
            source_id: "source".into(),
            received_at_unix_ms: 1,
            spans: vec![
                span("root", None, 1),
                span("child-a", Some("root"), 2),
                span("grandchild", Some("child-a"), 3),
                span("child-b", Some("root"), 4),
            ],
            rejected_spans: 0,
            rejection_message: None,
        };
        let receipt = store
            .journal_batch(&mut batch, b"span-tree", "test", 4_096)
            .unwrap();
        store.project_journal(receipt.journal_sequence).unwrap();
        let run = store.list_runs(0, 1).unwrap().remove(0);

        let roots = store
            .span_tree_page(&run.logical_trace_id, run.revision, None, 0, 500)
            .unwrap();
        assert_eq!(roots.total, 1);
        assert_eq!(roots.rows[0].span_id, "root");

        let children = store
            .span_tree_page(&run.logical_trace_id, run.revision, Some("root"), 0, 1)
            .unwrap();
        assert_eq!(children.total, 2);
        assert_eq!(children.rows.len(), 1);
        assert_eq!(children.rows[0].span_id, "child-a");

        let mut orphan_batch = SpanUpsertBatchV1 {
            schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
            source_id: "source".into(),
            received_at_unix_ms: 2,
            spans: vec![span("late-child", Some("late-root"), 5)],
            rejected_spans: 0,
            rejection_message: None,
        };
        let orphan = store
            .journal_batch(&mut orphan_batch, b"span-tree-orphan", "test", 4_096)
            .unwrap();
        store.project_journal(orphan.journal_sequence).unwrap();
        assert_eq!(
            store
                .get_span(&run.logical_trace_id, run.revision, "late-child")
                .unwrap()
                .unwrap()
                .depth,
            0
        );

        let mut parent_batch = SpanUpsertBatchV1 {
            schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
            source_id: "source".into(),
            received_at_unix_ms: 3,
            spans: vec![span("late-root", None, 6)],
            rejected_spans: 0,
            rejection_message: None,
        };
        let parent = store
            .journal_batch(&mut parent_batch, b"span-tree-parent", "test", 4_096)
            .unwrap();
        store.project_journal(parent.journal_sequence).unwrap();
        let repaired_child = store
            .get_span(&run.logical_trace_id, run.revision, "late-child")
            .unwrap()
            .unwrap();
        let repaired_parent = store
            .get_span(&run.logical_trace_id, run.revision, "late-root")
            .unwrap()
            .unwrap();
        assert_eq!(repaired_child.depth, 1);
        assert!(repaired_parent.has_children);
        assert_eq!(run.lifecycle, TraceLifecycle::Live);
    }
}

use super::*;

struct DispositionTarget {
    project_id: String,
    logical_trace_id: String,
    analysis_id: String,
    detector_id: String,
    detector_version: String,
}

impl WorkspaceStore {
    pub fn set_finding_disposition(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
        finding_id: &str,
        state: FindingDispositionStateV1,
    ) -> Result<FindingDispositionV1, StoreError> {
        scope.validate().map_err(StoreError::Invalid)?;
        let mut control = self.control.lock().expect("control store lock poisoned");
        let target = disposition_target(&control, &self.workspace_id, scope, group_id, finding_id)?;
        let updated_at_unix_ms = now_unix_ms();
        let transaction = control.transaction()?;
        transaction.execute(
            "INSERT INTO finding_disposition_events(
                workspace_id, finding_id, project_id, group_id, analysis_id,
                detector_id, detector_version, state, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                self.workspace_id,
                finding_id,
                target.project_id,
                group_id,
                target.analysis_id,
                target.detector_id,
                target.detector_version,
                disposition_state_name(state),
                updated_at_unix_ms,
            ],
        )?;
        transaction.execute(
            "INSERT INTO finding_dispositions(
                workspace_id, finding_id, project_id, group_id, analysis_id,
                detector_id, detector_version, state, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(workspace_id, finding_id) DO UPDATE SET
                project_id = excluded.project_id,
                group_id = excluded.group_id,
                analysis_id = excluded.analysis_id,
                detector_id = excluded.detector_id,
                detector_version = excluded.detector_version,
                state = excluded.state,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                self.workspace_id,
                finding_id,
                target.project_id,
                group_id,
                target.analysis_id,
                target.detector_id,
                target.detector_version,
                disposition_state_name(state),
                updated_at_unix_ms,
            ],
        )?;
        super::super::refresh_failure_membership_dispositions(
            &transaction,
            &target.logical_trace_id,
            group_id,
        )?;
        transaction.commit()?;
        Ok(FindingDispositionV1 {
            project_id: target.project_id,
            group_id: group_id.to_string(),
            finding_id: finding_id.to_string(),
            analysis_id: target.analysis_id,
            detector_id: target.detector_id,
            detector_version: target.detector_version,
            state,
            updated_at_unix_ms: updated_at_unix_ms.max(0) as u64,
        })
    }

    pub fn undo_finding_disposition(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
        finding_id: &str,
    ) -> Result<bool, StoreError> {
        scope.validate().map_err(StoreError::Invalid)?;
        let mut control = self.control.lock().expect("control store lock poisoned");
        let target = disposition_target(&control, &self.workspace_id, scope, group_id, finding_id)?;
        let transaction = control.transaction()?;
        let removed = transaction.execute(
            "DELETE FROM finding_dispositions
              WHERE workspace_id = ?1 AND finding_id = ?2 AND project_id = ?3",
            params![self.workspace_id, finding_id, target.project_id],
        )?;
        if removed > 0 {
            transaction.execute(
                "INSERT INTO finding_disposition_events(
                    workspace_id, finding_id, project_id, group_id, analysis_id,
                    detector_id, detector_version, state, created_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'unreviewed', ?8)",
                params![
                    self.workspace_id,
                    finding_id,
                    target.project_id,
                    group_id,
                    target.analysis_id,
                    target.detector_id,
                    target.detector_version,
                    now_unix_ms(),
                ],
            )?;
            super::super::refresh_failure_membership_dispositions(
                &transaction,
                &target.logical_trace_id,
                group_id,
            )?;
        }
        transaction.commit()?;
        Ok(removed > 0)
    }
}

fn disposition_target(
    control: &SqliteConnection,
    workspace_id: &str,
    scope: &QueryScopeV1,
    group_id: &str,
    finding_id: &str,
) -> Result<DispositionTarget, StoreError> {
    let criteria = &scope.criteria;
    if criteria
        .project_id
        .as_deref()
        .is_none_or(|project_id| project_id == UNASSIGNED_PROJECT_ID)
    {
        return Err(StoreError::Invalid(
            "finding review requires one explicit assigned project".into(),
        ));
    }
    control
        .query_row(
            "SELECT t.project_id, f.logical_trace_id, f.analysis_id, f.detector_id,
                    f.detector_version
               FROM active_failure_findings f
               JOIN logical_traces t ON t.logical_trace_id = f.logical_trace_id
              WHERE t.workspace_id = ?1
                AND f.group_id = ?2 AND f.finding_id = ?3
                AND t.project_id = ?4
                AND (?5 IS NULL OR t.service_name = ?5)
                AND (?6 IS NULL OR t.environment = ?6)
                AND (?7 IS NULL OR t.build_id = ?7)
                AND (?8 IS NULL OR t.session_id = ?8)
                AND (?9 IS NULL OR t.start_time_unix_nano >= ?9)
                AND (?10 IS NULL OR t.start_time_unix_nano <= ?10)",
            params![
                workspace_id,
                group_id,
                finding_id,
                criteria.project_id,
                criteria.service_name,
                criteria.environment,
                criteria.build_id,
                criteria.session_id,
                criteria.started_after_unix_nano.map(|value| value as i64),
                criteria.started_before_unix_nano.map(|value| value as i64),
            ],
            |row| {
                Ok(DispositionTarget {
                    project_id: row.get(0)?,
                    logical_trace_id: row.get(1)?,
                    analysis_id: row.get(2)?,
                    detector_id: row.get(3)?,
                    detector_version: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::Invalid("finding is not active in the immutable scope".into()))
}

pub(super) fn disposition_state_name(state: FindingDispositionStateV1) -> &'static str {
    match state {
        FindingDispositionStateV1::Confirmed => "confirmed",
        FindingDispositionStateV1::Dismissed => "dismissed",
        FindingDispositionStateV1::NeedsContext => "needs_context",
    }
}

pub(super) fn parse_disposition_state(
    value: &str,
) -> Result<FindingDispositionStateV1, StoreError> {
    match value {
        "confirmed" => Ok(FindingDispositionStateV1::Confirmed),
        "dismissed" => Ok(FindingDispositionStateV1::Dismissed),
        "needs_context" => Ok(FindingDispositionStateV1::NeedsContext),
        _ => Err(StoreError::Invalid(format!(
            "invalid finding disposition state {value}"
        ))),
    }
}

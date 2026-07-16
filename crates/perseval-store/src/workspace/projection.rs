use super::*;

pub(super) fn ensure_logical_trace(
    transaction: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    span: &crate::model::SpanUpsertV1,
    now: i64,
) -> Result<(u64, bool), StoreError> {
    let current = transaction
        .query_row(
            "SELECT revision, lifecycle FROM logical_traces WHERE logical_trace_id = ?1",
            params![span.logical_trace_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let service = string_attr(&span.resource, "service.name");
    let project =
        string_attr(&span.resource, "perseval.project.id").filter(|value| !value.trim().is_empty());
    let environment = string_attr(&span.resource, "deployment.environment.name")
        .or_else(|| string_attr(&span.resource, "deployment.environment"));
    let session_id = string_attr(&span.resource, "gen_ai.conversation.id")
        .or_else(|| string_attr(&span.attributes, "gen_ai.conversation.id"))
        .or_else(|| string_attr(&span.resource, "session.id"))
        .or_else(|| string_attr(&span.attributes, "session.id"))
        .or_else(|| string_attr(&span.resource, "openinference.session.id"))
        .or_else(|| string_attr(&span.attributes, "openinference.session.id"));
    let build_id = string_attr(&span.resource, "service.version")
        .or_else(|| string_attr(&span.resource, "deployment.version"))
        .or_else(|| string_attr(&span.resource, "agent.version"));
    let agent_id = string_attr(&span.resource, "gen_ai.agent.id")
        .or_else(|| string_attr(&span.attributes, "gen_ai.agent.id"))
        .or_else(|| string_attr(&span.resource, "agent.id"))
        .or_else(|| string_attr(&span.attributes, "agent.id"));
    let identity_quality = if project.is_some() {
        IdentityQualityV1::Explicit
    } else {
        IdentityQualityV1::Unknown
    };
    if let Some((revision, lifecycle)) = current {
        if lifecycle == "finalized" {
            let next = revision + 1;
            transaction.execute(
                "UPDATE logical_traces SET revision = ?1, lifecycle = 'reopened', last_committed_unix_ms = ?2,
                    title = ?3, service_name = COALESCE(?4, service_name), environment = COALESCE(?5, environment),
                    project_id = CASE WHEN project_id = 'unassigned' AND ?6 IS NOT NULL THEN ?6 ELSE project_id END,
                    session_id = COALESCE(?7, session_id), build_id = COALESCE(?8, build_id),
                    agent_id = COALESCE(?9, agent_id),
                    identity_quality = CASE WHEN ?10 = 'explicit' THEN 'explicit' ELSE identity_quality END,
                    start_time_unix_nano = ?11, end_time_unix_nano = ?12, span_count = 0, error_count = 0,
                    analysis_status = 'reanalyzing'
                 WHERE logical_trace_id = ?13",
                params![next, now, span.name, service, environment, project, session_id, build_id, agent_id, identity_quality.as_str(), span.start_time_unix_nano as i64, span.end_time_unix_nano as i64, span.logical_trace_id],
            )?;
            transaction.execute(
                "INSERT INTO trace_revisions (
                    logical_trace_id, revision, lifecycle, created_at_unix_ms,
                    finalized_at_unix_ms
                 ) VALUES (?1, ?2, 'reopened', ?3, NULL)",
                params![span.logical_trace_id, next, now],
            )?;
            return Ok((next as u64, true));
        }
        transaction.execute(
            "UPDATE logical_traces SET lifecycle = CASE WHEN lifecycle = 'quiescent' THEN 'live' ELSE lifecycle END,
                    project_id = CASE WHEN project_id = 'unassigned' AND ?1 IS NOT NULL THEN ?1 ELSE project_id END,
                    session_id = COALESCE(?2, session_id), build_id = COALESCE(?3, build_id),
                    agent_id = COALESCE(?4, agent_id),
                    identity_quality = CASE WHEN ?5 = 'explicit' THEN 'explicit' ELSE identity_quality END,
                    last_committed_unix_ms = ?6 WHERE logical_trace_id = ?7",
            params![project, session_id, build_id, agent_id, identity_quality.as_str(), now, span.logical_trace_id],
        )?;
        return Ok((revision as u64, lifecycle == "reopened"));
    }
    transaction.execute(
        "INSERT INTO logical_traces (
            workspace_id, project_id, logical_trace_id, source_id, external_trace_id, revision, lifecycle,
            title, service_name, environment, session_id, build_id, agent_id, identity_quality,
            start_time_unix_nano, end_time_unix_nano, last_committed_unix_ms,
            span_count, error_count, analysis_status, finding_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'live', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, 0, 'not_ready', 0)",
        params![
            workspace_id,
            project.unwrap_or_else(|| UNASSIGNED_PROJECT_ID.into()),
            span.logical_trace_id,
            span.source_id,
            span.external_trace_id,
            span.name,
            service,
            environment,
            session_id,
            build_id,
            agent_id,
            identity_quality.as_str(),
            span.start_time_unix_nano as i64,
            span.end_time_unix_nano as i64,
            now,
        ],
    )?;
    transaction.execute(
        "INSERT INTO trace_revisions (
            logical_trace_id, revision, lifecycle, created_at_unix_ms,
            finalized_at_unix_ms
         ) VALUES (?1, 1, 'live', ?2, NULL)",
        params![span.logical_trace_id, now],
    )?;
    Ok((1, false))
}

pub(super) fn insert_delta_transaction(
    transaction: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    summary: RunSummary,
    change: TraceChangeKind,
    changed_span_ids: Vec<String>,
) -> Result<TraceDeltaV1, StoreError> {
    transaction.execute(
        "INSERT INTO trace_delta_outbox (workspace_id, logical_trace_id, delta_json, created_at_unix_ms)
         VALUES (?1, ?2, '', ?3)",
        params![workspace_id, summary.logical_trace_id, now_unix_ms()],
    )?;
    let sequence = transaction.last_insert_rowid() as u64;
    let delta = TraceDeltaV1 {
        schema_version: TRACE_DELTA_SCHEMA_VERSION.into(),
        workspace_id: workspace_id.into(),
        commit_sequence: sequence,
        logical_trace_id: summary.logical_trace_id.clone(),
        revision: summary.revision,
        change,
        changed_span_ids,
        summary,
    };
    transaction.execute(
        "UPDATE trace_delta_outbox SET delta_json = ?1 WHERE commit_sequence = ?2",
        params![serde_json::to_string(&delta)?, sequence as i64],
    )?;
    Ok(delta)
}

pub(super) fn insert_delta_locked(
    control: &SqliteConnection,
    workspace_id: &str,
    summary: RunSummary,
    change: TraceChangeKind,
    changed_span_ids: Vec<String>,
) -> Result<TraceDeltaV1, StoreError> {
    control.execute(
        "INSERT INTO trace_delta_outbox (workspace_id, logical_trace_id, delta_json, created_at_unix_ms)
         VALUES (?1, ?2, '', ?3)",
        params![workspace_id, summary.logical_trace_id, now_unix_ms()],
    )?;
    let sequence = control.last_insert_rowid() as u64;
    let delta = TraceDeltaV1 {
        schema_version: TRACE_DELTA_SCHEMA_VERSION.into(),
        workspace_id: workspace_id.into(),
        commit_sequence: sequence,
        logical_trace_id: summary.logical_trace_id.clone(),
        revision: summary.revision,
        change,
        changed_span_ids,
        summary,
    };
    control.execute(
        "UPDATE trace_delta_outbox SET delta_json = ?1 WHERE commit_sequence = ?2",
        params![serde_json::to_string(&delta)?, sequence as i64],
    )?;
    Ok(delta)
}

pub(super) fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunSummary> {
    let lifecycle: String = row.get(4)?;
    let identity_quality: String = row.get(11)?;
    let analysis_status: String = row.get(17)?;
    Ok(RunSummary {
        project_id: row.get(0)?,
        logical_trace_id: row.get(1)?,
        external_trace_id: row.get(2)?,
        revision: row.get::<_, i64>(3)? as u64,
        lifecycle: TraceLifecycle::from_str(&lifecycle).unwrap_or(TraceLifecycle::Live),
        title: row.get(5)?,
        service_name: row.get(6)?,
        environment: row.get(7)?,
        session_id: row.get(8)?,
        build_id: row.get(9)?,
        agent_id: row.get(10)?,
        identity_quality: match identity_quality.as_str() {
            "explicit" => IdentityQualityV1::Explicit,
            "inferred" => IdentityQualityV1::Inferred,
            _ => IdentityQualityV1::Unknown,
        },
        start_time_unix_nano: row.get::<_, i64>(12)? as u64,
        end_time_unix_nano: row.get::<_, i64>(13)? as u64,
        last_committed_unix_ms: row.get(14)?,
        span_count: row.get::<_, i64>(15)? as u64,
        error_count: row.get::<_, i64>(16)? as u64,
        analysis_status: AnalysisStatus::from_str(&analysis_status)
            .unwrap_or(AnalysisStatus::NotReady),
        finding_count: row.get::<_, i64>(18)? as u64,
    })
}

pub(super) fn query_run_locked(
    control: &SqliteConnection,
    workspace_id: &str,
    trace_id: &str,
) -> Result<Option<RunSummary>, StoreError> {
    control
        .query_row(
            "SELECT project_id, logical_trace_id, external_trace_id, revision, lifecycle, title,
                    service_name, environment, session_id, build_id, agent_id, identity_quality,
                    start_time_unix_nano, end_time_unix_nano, last_committed_unix_ms,
                    span_count, error_count, analysis_status, finding_count
             FROM logical_traces WHERE workspace_id = ?1 AND logical_trace_id = ?2",
            params![workspace_id, trace_id],
            map_run,
        )
        .optional()
        .map_err(StoreError::from)
}

pub(super) fn query_run_transaction(
    transaction: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    trace_id: &str,
) -> Result<Option<RunSummary>, StoreError> {
    transaction
        .query_row(
            "SELECT project_id, logical_trace_id, external_trace_id, revision, lifecycle, title,
                    service_name, environment, session_id, build_id, agent_id, identity_quality,
                    start_time_unix_nano, end_time_unix_nano, last_committed_unix_ms,
                    span_count, error_count, analysis_status, finding_count
             FROM logical_traces WHERE workspace_id = ?1 AND logical_trace_id = ?2",
            params![workspace_id, trace_id],
            map_run,
        )
        .optional()
        .map_err(StoreError::from)
}

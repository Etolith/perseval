mod dispositions;
mod investigation;
mod queries;

use super::*;
use dispositions::parse_disposition_state;
use traces_to_evals::FindingPresentationV1;

struct ActiveFailureRow {
    finding: BehaviorFinding,
    logical_trace_id: String,
    revision: u64,
    project_id: String,
    run_title: String,
    service_name: Option<String>,
    analysis_status: AnalysisStatus,
    telemetry_gaps: Vec<String>,
    presentation: Option<FindingPresentationV1>,
    analysis_id: String,
    disposition: Option<FindingDispositionV1>,
    disposition_stale: bool,
}

struct MaterializedFailureGroupRow {
    project_id: String,
    group_id: String,
    failure_signature: String,
    detector_ids: Vec<String>,
    subject: Option<String>,
    operation: Option<String>,
    presentation: Option<FindingPresentationV1>,
    severity: FindingSeverity,
    occurrence_count: u64,
    recovered_count: u64,
    unrecovered_count: u64,
    unknown_recovery_count: u64,
    affected_run_count: u64,
    affected_build_count: u64,
    affected_environment_count: u64,
    confirmed_count: u64,
    dismissed_count: u64,
    needs_context_count: u64,
    unreviewed_count: u64,
    stale_disposition_count: u64,
    first_seen_at: String,
    last_seen_at: String,
    telemetry_gap_count: u64,
    reanalyzing: bool,
}

struct FailureRecurrenceWindow {
    started_at_unix_nano: u64,
    ended_at_unix_nano: u64,
    bucket_width_nano: u64,
    eligible_run_counts: Vec<u64>,
}

struct FailureGroupMetadata {
    detector_versions: Vec<String>,
    adapter_versions: Vec<String>,
    telemetry_gaps: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn materialized_disposition(
    project_id: &str,
    group_id: &str,
    finding_id: &str,
    analysis_id: Option<String>,
    detector_id: Option<String>,
    detector_version: Option<String>,
    state: Option<String>,
    updated_at_unix_ms: Option<i64>,
    stale: bool,
) -> Result<(Option<FindingDispositionV1>, bool), StoreError> {
    let Some(state) = state else {
        return Ok((None, false));
    };
    let disposition = FindingDispositionV1 {
        project_id: project_id.to_string(),
        group_id: group_id.to_string(),
        finding_id: finding_id.to_string(),
        analysis_id: analysis_id.ok_or_else(|| {
            StoreError::Invalid("materialized disposition is missing analysis identity".into())
        })?,
        detector_id: detector_id.ok_or_else(|| {
            StoreError::Invalid("materialized disposition is missing detector identity".into())
        })?,
        detector_version: detector_version.ok_or_else(|| {
            StoreError::Invalid("materialized disposition is missing detector version".into())
        })?,
        state: parse_disposition_state(&state)?,
        updated_at_unix_ms: updated_at_unix_ms.unwrap_or_default().max(0) as u64,
    };
    Ok((Some(disposition), stale))
}

fn normalized_failure_search(search: Option<&str>) -> Option<String> {
    search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_ascii_lowercase()))
}

fn failure_group_scope(scope: &QueryScopeV1, project_id: &str) -> QueryScopeV1 {
    if scope.criteria.project_id.as_deref() == Some(project_id) {
        return scope.clone();
    }
    let mut criteria = scope.criteria.clone();
    criteria.project_id = Some(project_id.to_string());
    QueryScopeV1::new(criteria)
}

fn recurrence_rate_basis_points(affected: u64, eligible: u64) -> Option<u16> {
    if eligible == 0 {
        return None;
    }
    let rounded = affected.saturating_mul(10_000).saturating_add(eligible / 2) / eligible;
    Some(rounded.min(10_000) as u16)
}

fn query_distinct_strings(
    connection: &SqliteConnection,
    query: &str,
    workspace_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(query)?;
    statement
        .query_map(params![workspace_id], |row| row.get(0))?
        .map(|row| row.map_err(StoreError::from))
        .collect()
}

fn failure_occurrence_from_row(
    scope: &QueryScopeV1,
    group_id: &str,
    row: ActiveFailureRow,
) -> FailureOccurrence {
    FailureOccurrence {
        scope: scope.clone(),
        project_id: row.project_id,
        group_id: group_id.to_string(),
        logical_trace_id: row.logical_trace_id,
        revision: row.revision,
        run_title: row.run_title,
        service_name: row.service_name,
        analysis_status: row.analysis_status,
        finding: row.finding,
        disposition: row.disposition,
        disposition_stale: row.disposition_stale,
        telemetry_gaps: row.telemetry_gaps,
    }
}

fn query_scope_for_project(project_id: &str) -> QueryScopeV1 {
    QueryScopeV1::new(QueryScopeCriteriaV1 {
        project_id: Some(project_id.to_string()),
        ..QueryScopeCriteriaV1::default()
    })
}

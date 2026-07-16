use std::collections::BTreeMap;

use duckdb::params as duck_params;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use traces_to_evals::{
    ExecutionStep, SpanKind, StructuralTraceAligner, TraceAlignmentOptions, TraceComparison,
    TraceComparisonInput,
};

use super::{StoreError, WorkspaceStore, now_unix_ms};
use crate::model::{RunComparisonRequestV1, RunFiltersV1, TraceLifecycle};

impl WorkspaceStore {
    pub fn compare_runs(
        &self,
        request: &RunComparisonRequestV1,
        maximum_input_steps: usize,
        options: TraceAlignmentOptions,
    ) -> Result<TraceComparison, StoreError> {
        self.compare_runs_cancellable(request, maximum_input_steps, options, || false)
    }

    pub fn compare_runs_cancellable(
        &self,
        request: &RunComparisonRequestV1,
        maximum_input_steps: usize,
        options: TraceAlignmentOptions,
        cancelled: impl Fn() -> bool,
    ) -> Result<TraceComparison, StoreError> {
        let comparison = self.build_run_comparison_cancellable(
            request,
            maximum_input_steps,
            options,
            cancelled,
        )?;
        self.commit_trace_comparison(request, &comparison)?;
        Ok(comparison)
    }

    pub fn build_run_comparison_cancellable(
        &self,
        request: &RunComparisonRequestV1,
        maximum_input_steps: usize,
        options: TraceAlignmentOptions,
        cancelled: impl Fn() -> bool,
    ) -> Result<TraceComparison, StoreError> {
        validate_request(request)?;
        let project_id = request
            .scope
            .criteria
            .project_id
            .as_deref()
            .ok_or_else(|| {
                StoreError::Invalid(
                    "Choose one project before comparing runs; All Projects is read-only.".into(),
                )
            })?;
        ensure_not_cancelled(&cancelled)?;
        let baseline = self.get_run(&request.baseline_trace_id)?.ok_or_else(|| {
            StoreError::Invalid("Baseline run no longer exists in this workspace.".into())
        })?;
        let candidate = self.get_run(&request.candidate_trace_id)?.ok_or_else(|| {
            StoreError::Invalid("Candidate run no longer exists in this workspace.".into())
        })?;
        let scoped_run_ids = self
            .list_runs_filtered(
                &RunFiltersV1 {
                    scope: request.scope.clone(),
                    ..RunFiltersV1::default()
                },
                0,
                u32::MAX,
            )?
            .into_iter()
            .map(|run| run.logical_trace_id)
            .collect::<std::collections::BTreeSet<_>>();
        if baseline.project_id != project_id
            || candidate.project_id != project_id
            || !scoped_run_ids.contains(&request.baseline_trace_id)
            || !scoped_run_ids.contains(&request.candidate_trace_id)
        {
            return Err(StoreError::Invalid(
                "Runs must belong to the immutable active scope; cross-scope identities are never merged."
                    .into(),
            ));
        }
        if baseline.revision != request.baseline_revision
            || candidate.revision != request.candidate_revision
        {
            return Err(StoreError::Invalid(
                "One selected run has a newer revision. Review and select the current revisions."
                    .into(),
            ));
        }
        if !matches!(baseline.lifecycle, TraceLifecycle::Finalized)
            || !matches!(candidate.lifecycle, TraceLifecycle::Finalized)
        {
            return Err(StoreError::Invalid(
                "Structural comparison requires two finalized revisions; a selected run is still live or reopened."
                    .into(),
            ));
        }
        let baseline_steps = self.load_comparison_steps(
            &request.baseline_trace_id,
            request.baseline_revision,
            maximum_input_steps,
            &cancelled,
        )?;
        let candidate_steps = self.load_comparison_steps(
            &request.candidate_trace_id,
            request.candidate_revision,
            maximum_input_steps,
            &cancelled,
        )?;
        let comparison = StructuralTraceAligner
            .compare_cancellable(
                &TraceComparisonInput {
                    project_id: project_id.to_string(),
                    logical_trace_id: request.baseline_trace_id.clone(),
                    revision: request.baseline_revision,
                    build_id: baseline.build_id,
                    agent_id: baseline.agent_id,
                    steps: baseline_steps,
                },
                &TraceComparisonInput {
                    project_id: project_id.to_string(),
                    logical_trace_id: request.candidate_trace_id.clone(),
                    revision: request.candidate_revision,
                    build_id: candidate.build_id,
                    agent_id: candidate.agent_id,
                    steps: candidate_steps,
                },
                options,
                &cancelled,
            )
            .ok_or(StoreError::Cancelled)?;
        ensure_not_cancelled(&cancelled)?;
        Ok(comparison)
    }

    pub fn commit_trace_comparison(
        &self,
        request: &RunComparisonRequestV1,
        comparison: &TraceComparison,
    ) -> Result<(), StoreError> {
        validate_request(request)?;
        let project_id = request
            .scope
            .criteria
            .project_id
            .as_deref()
            .ok_or_else(|| {
                StoreError::Invalid(
                    "Choose one project before comparing runs; All Projects is read-only.".into(),
                )
            })?;
        if comparison.project_id != project_id
            || comparison.baseline_trace_id != request.baseline_trace_id
            || comparison.baseline_revision != request.baseline_revision
            || comparison.candidate_trace_id != request.candidate_trace_id
            || comparison.candidate_revision != request.candidate_revision
        {
            return Err(StoreError::Invalid(
                "comparison result does not match its immutable request scope".into(),
            ));
        }
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "INSERT INTO trace_comparisons(
                comparison_id, project_id, baseline_trace_id, baseline_revision,
                candidate_trace_id, candidate_revision, result_json, created_at_unix_ms,
                scope_id, scope_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(comparison_id) DO UPDATE SET
                result_json = excluded.result_json,
                scope_id = excluded.scope_id,
                scope_json = excluded.scope_json",
            params![
                comparison.comparison_id,
                comparison.project_id,
                comparison.baseline_trace_id,
                comparison.baseline_revision as i64,
                comparison.candidate_trace_id,
                comparison.candidate_revision as i64,
                serde_json::to_string(&comparison)?,
                now_unix_ms(),
                request.scope.scope_id,
                serde_json::to_string(&request.scope)?,
            ],
        )?;
        Ok(())
    }

    pub fn get_trace_comparison(
        &self,
        comparison_id: &str,
    ) -> Result<Option<TraceComparison>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let json = control
            .query_row(
                "SELECT result_json FROM trace_comparisons WHERE comparison_id = ?1",
                params![comparison_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    fn load_comparison_steps(
        &self,
        logical_trace_id: &str,
        revision: u64,
        maximum_input_steps: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Vec<ExecutionStep>, StoreError> {
        // A comparison is read-only and may scan a large finalized projection. Use a separate
        // connection so the single workspace writer remains free to journal/project live input.
        let analytics = self.analytics_reads.connection();
        let mut statement = analytics.prepare(
            "SELECT span_id, parent_span_id, name, category, status_code, duration_nano,
                    attributes_json
             FROM spans WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE
             ORDER BY topology_order NULLS LAST, start_time_unix_nano, span_id
             LIMIT ?3",
        )?;
        let mapped = statement.query_map(
            duck_params![
                logical_trace_id,
                revision as i64,
                maximum_input_steps.saturating_add(1) as i64
            ],
            |row| {
                let attributes_json: String = row.get(6)?;
                let attributes = serde_json::from_str::<BTreeMap<String, Value>>(&attributes_json)
                    .unwrap_or_default();
                Ok(ExecutionStep {
                    span_id: row.get(0)?,
                    parent_span_id: row.get(1)?,
                    name: row.get(2)?,
                    kind: span_kind(&row.get::<_, String>(3)?),
                    status_code: row.get(4)?,
                    duration_nano: row.get::<_, i64>(5)? as u64,
                    agent_ref: string_attribute(
                        &attributes,
                        &["gen_ai.agent.id", "agent.id", "agent.name"],
                    ),
                    operation: string_attribute(
                        &attributes,
                        &["gen_ai.operation.name", "tool.name", "operation.name"],
                    ),
                    facts: comparison_facts(&attributes),
                })
            },
        )?;
        let mut rows = Vec::with_capacity(maximum_input_steps.min(4_096));
        for (index, row) in mapped.enumerate() {
            if index % 256 == 0 {
                ensure_not_cancelled(cancelled)?;
            }
            rows.push(row?);
        }
        if rows.len() > maximum_input_steps {
            return Err(StoreError::Invalid(format!(
                "Run contains more than {maximum_input_steps} comparable steps; narrow the run or increase the workspace comparison limit."
            )));
        }
        Ok(rows)
    }
}

fn ensure_not_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), StoreError> {
    if cancelled() {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn validate_request(request: &RunComparisonRequestV1) -> Result<(), StoreError> {
    request.scope.validate().map_err(StoreError::Invalid)?;
    if request.baseline_trace_id == request.candidate_trace_id
        && request.baseline_revision == request.candidate_revision
    {
        return Err(StoreError::Invalid(
            "Choose two distinct run revisions to compare.".into(),
        ));
    }
    Ok(())
}

fn span_kind(category: &str) -> SpanKind {
    match category.to_ascii_lowercase().as_str() {
        "llm" => SpanKind::Llm,
        "agent" => SpanKind::Agent,
        "tool" => SpanKind::Tool,
        "chain" => SpanKind::Chain,
        "retriever" => SpanKind::Retriever,
        "reranker" => SpanKind::Reranker,
        "embedding" => SpanKind::Embedding,
        "guardrail" => SpanKind::Guardrail,
        "evaluator" => SpanKind::Evaluator,
        "prompt" => SpanKind::Prompt,
        _ => SpanKind::Other,
    }
}

fn string_attribute(attributes: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        attributes
            .get(*key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn comparison_facts(attributes: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    const SAFE_FACTS: &[&str] = &[
        "error.type",
        "gen_ai.response.finish_reasons",
        "perseval.final_outcome",
        "perseval.claimed_outcome",
        "perseval.policy.outcome",
        "retry.count",
        "tool.status",
    ];
    SAFE_FACTS
        .iter()
        .filter_map(|key| {
            attributes
                .get(*key)
                .cloned()
                .map(|value| ((*key).to_owned(), value))
        })
        .collect()
}

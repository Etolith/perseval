use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use traces_to_evals::{
    CompactTaskCompletionProjectionV1, TaskCompletionTraceFactV1, TraceFactKindV1,
    TraceFactStatusV1, canonical_content_id,
};

const FEATURE_RECORD_SCHEMA_VERSION: &str = "perseval.task_completion_evidence_feature_record.v1";
pub const FEATURE_SET_VERSION: &str = "perseval.task_completion_structured_evidence.v1";
const SMOLLM_PROMPT_VERSION: &str = "perseval.binary-task-completion-ab-v1";

pub const FEATURE_NAMES: [&str; 40] = [
    "smollm_incomplete_logit",
    "included_fact_count_log1p",
    "omitted_fact_count_log1p",
    "evidence_coverage",
    "projected_token_ratio",
    "compression_ratio",
    "final_response_present",
    "recovery_chain_count_log1p",
    "recovered_failure_fraction",
    "failed_fact_fraction",
    "succeeded_fact_fraction",
    "unfinished_fact_fraction",
    "verification_succeeded_count_log1p",
    "verification_failed_count_log1p",
    "verification_missing",
    "mutation_succeeded_count_log1p",
    "mutation_failed_count_log1p",
    "external_succeeded_count_log1p",
    "external_failed_count_log1p",
    "tool_succeeded_count_log1p",
    "tool_failed_count_log1p",
    "child_succeeded_count_log1p",
    "child_failed_count_log1p",
    "unfinished_fact_count_log1p",
    "failed_fact_count_log1p",
    "succeeded_fact_count_log1p",
    "last_fact_failed",
    "last_fact_succeeded",
    "failure_recency",
    "successes_after_last_failure_log1p",
    "failures_after_last_success_log1p",
    "distinct_tool_count_log1p",
    "mandatory_fact_fraction",
    "goal_relevant_fact_fraction",
    "final_response_token_ratio",
    "recovery_token_ratio",
    "user_amendment_count_log1p",
    "requested_verification_count_log1p",
    "requested_side_effect_count_log1p",
    "constraint_count_log1p",
];

/// Label-free, revision-bound measurements of projected trace evidence.
///
/// Source, model, benchmark reward, environment success, and gold-label fields
/// are intentionally absent. These values describe evidence; the calibrated
/// model remains responsible for the completion decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletionEvidenceFeatureRecordV1 {
    pub schema_version: String,
    pub feature_set_version: String,
    pub feature_record_id: String,
    pub target_key: String,
    pub target_revision: String,
    pub trace_context_binding_id: String,
    pub projection_hash: String,
    pub projector_version: String,
    pub inference_model_id: String,
    pub inference_prompt_version: String,
    pub feature_names: Vec<String>,
    pub feature_values: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct RunRecord {
    target_key: String,
    #[serde(default)]
    mandatory_facts_omitted: u32,
    decision: DecisionRecord,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionRecord {
    target_key: String,
    target_revision: String,
    trace_context_binding_id: String,
    raw_logit_difference: Option<f64>,
    inference: InferenceRecord,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceRecord {
    model_id: String,
    prompt_version: Option<String>,
}

pub fn extract(projections_path: &Path, results_path: &Path, output_path: &Path) -> Result<usize> {
    let projections = load_projections(projections_path)?;
    let results = load_results(results_path)?;
    let projection_keys = projections.keys().cloned().collect::<BTreeSet<_>>();
    let result_keys = results.keys().cloned().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        projection_keys == result_keys,
        "projection and result target sets differ (missing results {}, extra results {})",
        projection_keys.difference(&result_keys).count(),
        result_keys.difference(&projection_keys).count()
    );

    let identities = results
        .values()
        .map(|record| {
            (
                record.decision.inference.model_id.as_str(),
                record.decision.inference.prompt_version.as_deref(),
            )
        })
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        identities.len() == 1,
        "structured feature extraction requires one inference model and prompt identity"
    );
    let (_, prompt_version) = identities
        .iter()
        .next()
        .context("structured feature extraction requires results")?;
    anyhow::ensure!(
        *prompt_version == Some(SMOLLM_PROMPT_VERSION),
        "structured features require SmolLM prompt version {SMOLLM_PROMPT_VERSION}"
    );

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = File::create(output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::new(file);
    for (target_key, projection) in &projections {
        let result = &results[target_key];
        let record = feature_record(projection, result)?;
        serde_json::to_writer(&mut writer, &record)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(projections.len())
}

pub fn load_feature_records(
    path: &Path,
) -> Result<BTreeMap<String, TaskCompletionEvidenceFeatureRecordV1>> {
    let mut records = BTreeMap::new();
    for (line_number, line) in lines(path)?.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: TaskCompletionEvidenceFeatureRecordV1 = serde_json::from_str(&line)
            .with_context(|| {
                format!("invalid feature at {}:{}", path.display(), line_number + 1)
            })?;
        validate_feature_record(&record)?;
        let key = record.target_key.clone();
        anyhow::ensure!(
            records.insert(key.clone(), record).is_none(),
            "duplicate {key}"
        );
    }
    anyhow::ensure!(
        !records.is_empty(),
        "{} contains no features",
        path.display()
    );
    Ok(records)
}

fn feature_record(
    projection: &CompactTaskCompletionProjectionV1,
    result: &RunRecord,
) -> Result<TaskCompletionEvidenceFeatureRecordV1> {
    anyhow::ensure!(
        projection.target_key == result.target_key
            && result.target_key == result.decision.target_key,
        "target key mismatch for {}",
        projection.target_key
    );
    anyhow::ensure!(
        projection.target_revision == result.decision.target_revision,
        "target revision mismatch for {}",
        projection.target_key
    );
    anyhow::ensure!(
        projection.trace_context_binding_id == result.decision.trace_context_binding_id,
        "trace context binding mismatch for {}",
        projection.target_key
    );
    anyhow::ensure!(
        projection.stats.mandatory_facts_omitted == 0 && result.mandatory_facts_omitted == 0,
        "mandatory evidence was omitted for {}",
        projection.target_key
    );
    let logit = result
        .decision
        .raw_logit_difference
        .with_context(|| format!("missing SmolLM logit for {}", projection.target_key))?;
    anyhow::ensure!(logit.is_finite(), "non-finite SmolLM logit");
    let prompt_version = result
        .decision
        .inference
        .prompt_version
        .as_deref()
        .context("missing inference prompt version")?;
    anyhow::ensure!(
        prompt_version == SMOLLM_PROMPT_VERSION,
        "unexpected inference prompt version {prompt_version}"
    );

    let values = extract_values(projection, -logit);
    anyhow::ensure!(
        values.len() == FEATURE_NAMES.len() && values.iter().all(|value| value.is_finite()),
        "invalid structured feature vector for {}",
        projection.target_key
    );
    let names = FEATURE_NAMES.map(String::from).to_vec();
    let feature_record_id = canonical_content_id(
        FEATURE_RECORD_SCHEMA_VERSION,
        &serde_json::json!({
            "feature_set_version": FEATURE_SET_VERSION,
            "target_key": projection.target_key,
            "target_revision": projection.target_revision,
            "trace_context_binding_id": projection.trace_context_binding_id,
            "projection_hash": projection.projection_hash,
            "projector_version": projection.projector_version,
            "inference_model_id": result.decision.inference.model_id,
            "inference_prompt_version": prompt_version,
            "feature_names": names,
            "feature_values": values,
        }),
    )?;
    Ok(TaskCompletionEvidenceFeatureRecordV1 {
        schema_version: FEATURE_RECORD_SCHEMA_VERSION.into(),
        feature_set_version: FEATURE_SET_VERSION.into(),
        feature_record_id,
        target_key: projection.target_key.clone(),
        target_revision: projection.target_revision.clone(),
        trace_context_binding_id: projection.trace_context_binding_id.clone(),
        projection_hash: projection.projection_hash.clone(),
        projector_version: projection.projector_version.clone(),
        inference_model_id: result.decision.inference.model_id.clone(),
        inference_prompt_version: prompt_version.into(),
        feature_names: names,
        feature_values: values,
    })
}

fn extract_values(
    projection: &CompactTaskCompletionProjectionV1,
    incomplete_logit: f64,
) -> Vec<f64> {
    let facts = &projection.facts;
    let fact_count = facts.len() as f64;
    let failed = status_count(facts, TraceFactStatusV1::Failed);
    let succeeded = status_count(facts, TraceFactStatusV1::Succeeded);
    let unfinished = facts
        .iter()
        .filter(|fact| {
            matches!(
                fact.status,
                TraceFactStatusV1::Unknown
                    | TraceFactStatusV1::Running
                    | TraceFactStatusV1::Cancelled
            )
        })
        .count() as f64;
    let last = facts.iter().max_by_key(|fact| fact.sequence);
    let last_failure_sequence = facts
        .iter()
        .filter(|fact| fact.status == TraceFactStatusV1::Failed)
        .map(|fact| fact.sequence)
        .max();
    let last_success_sequence = facts
        .iter()
        .filter(|fact| fact.status == TraceFactStatusV1::Succeeded)
        .map(|fact| fact.sequence)
        .max();
    let max_sequence = facts.iter().map(|fact| fact.sequence).max().unwrap_or(0);
    let successes_after_last_failure = last_failure_sequence.map_or(0, |sequence| {
        facts
            .iter()
            .filter(|fact| fact.sequence > sequence && fact.status == TraceFactStatusV1::Succeeded)
            .count()
    });
    let failures_after_last_success = last_success_sequence.map_or(0, |sequence| {
        facts
            .iter()
            .filter(|fact| fact.sequence > sequence && fact.status == TraceFactStatusV1::Failed)
            .count()
    });
    let distinct_tools = facts
        .iter()
        .filter_map(|fact| fact.tool_name.as_deref())
        .collect::<BTreeSet<_>>()
        .len();
    let mandatory = facts.iter().filter(|fact| fact.mandatory).count() as f64;
    let goal_relevant = facts.iter().filter(|fact| !fact.mandatory).count() as f64;
    let included = projection.stats.included_facts as f64;
    let omitted = projection.stats.omitted_facts as f64;
    let projected_tokens = projection.token_budget.projected_tokens as f64;

    vec![
        incomplete_logit,
        log_count(included as usize),
        log_count(omitted as usize),
        ratio(included, included + omitted),
        ratio(
            projected_tokens,
            projection.token_budget.max_input_tokens as f64,
        ),
        ratio(
            projected_tokens,
            projection.token_budget.original_tokens as f64,
        ),
        binary(facts.iter().any(|fact| {
            fact.kind == TraceFactKindV1::AssistantMessage
                && matches!(
                    fact.lane,
                    traces_to_evals::TaskCompletionEvidenceLaneV1::FinalResponse
                )
        })),
        log_count(projection.recovery_chains.len()),
        ratio(projection.recovery_chains.len() as f64, failed),
        ratio(failed, fact_count),
        ratio(succeeded, fact_count),
        ratio(unfinished, fact_count),
        log_kind_status(
            facts,
            TraceFactKindV1::Verification,
            TraceFactStatusV1::Succeeded,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::Verification,
            TraceFactStatusV1::Failed,
        ),
        binary(
            !facts
                .iter()
                .any(|fact| fact.kind == TraceFactKindV1::Verification),
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ArtifactMutation,
            TraceFactStatusV1::Succeeded,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ArtifactMutation,
            TraceFactStatusV1::Failed,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ExternalAction,
            TraceFactStatusV1::Succeeded,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ExternalAction,
            TraceFactStatusV1::Failed,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ToolResult,
            TraceFactStatusV1::Succeeded,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ToolResult,
            TraceFactStatusV1::Failed,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ChildAgentResult,
            TraceFactStatusV1::Succeeded,
        ),
        log_kind_status(
            facts,
            TraceFactKindV1::ChildAgentResult,
            TraceFactStatusV1::Failed,
        ),
        log_count(unfinished as usize),
        log_count(failed as usize),
        log_count(succeeded as usize),
        binary(last.is_some_and(|fact| fact.status == TraceFactStatusV1::Failed)),
        binary(last.is_some_and(|fact| fact.status == TraceFactStatusV1::Succeeded)),
        ratio(
            last_failure_sequence.unwrap_or(0) as f64,
            max_sequence as f64,
        ),
        log_count(successes_after_last_failure),
        log_count(failures_after_last_success),
        log_count(distinct_tools),
        ratio(mandatory, fact_count),
        ratio(goal_relevant, fact_count),
        ratio(
            projection.token_budget.final_response_tokens as f64,
            projected_tokens,
        ),
        ratio(
            projection.token_budget.recovery_tokens as f64,
            projected_tokens,
        ),
        log_count(projection.goal.amendments.len()),
        log_count(projection.goal.requested_verification.len()),
        log_count(projection.goal.requested_side_effects.len()),
        log_count(projection.goal.constraints.len()),
    ]
}

fn status_count(facts: &[TaskCompletionTraceFactV1], status: TraceFactStatusV1) -> f64 {
    facts.iter().filter(|fact| fact.status == status).count() as f64
}

fn log_kind_status(
    facts: &[TaskCompletionTraceFactV1],
    kind: TraceFactKindV1,
    status: TraceFactStatusV1,
) -> f64 {
    log_count(
        facts
            .iter()
            .filter(|fact| fact.kind == kind && fact.status == status)
            .count(),
    )
}

fn log_count(value: usize) -> f64 {
    (value as f64).ln_1p()
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator <= 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

fn binary(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

fn validate_feature_record(record: &TaskCompletionEvidenceFeatureRecordV1) -> Result<()> {
    anyhow::ensure!(
        record.schema_version == FEATURE_RECORD_SCHEMA_VERSION,
        "unsupported feature record schema"
    );
    anyhow::ensure!(
        record.feature_set_version == FEATURE_SET_VERSION,
        "unsupported feature set version"
    );
    anyhow::ensure!(
        record.feature_names == FEATURE_NAMES.map(String::from),
        "feature names or ordering differ from {FEATURE_SET_VERSION}"
    );
    anyhow::ensure!(
        record.feature_values.len() == FEATURE_NAMES.len()
            && record.feature_values.iter().all(|value| value.is_finite()),
        "invalid feature vector for {}",
        record.target_key
    );
    anyhow::ensure!(
        record.inference_prompt_version == SMOLLM_PROMPT_VERSION,
        "unexpected prompt version for {}",
        record.target_key
    );
    let expected_id = canonical_content_id(
        FEATURE_RECORD_SCHEMA_VERSION,
        &serde_json::json!({
            "feature_set_version": record.feature_set_version,
            "target_key": record.target_key,
            "target_revision": record.target_revision,
            "trace_context_binding_id": record.trace_context_binding_id,
            "projection_hash": record.projection_hash,
            "projector_version": record.projector_version,
            "inference_model_id": record.inference_model_id,
            "inference_prompt_version": record.inference_prompt_version,
            "feature_names": record.feature_names,
            "feature_values": record.feature_values,
        }),
    )?;
    anyhow::ensure!(
        record.feature_record_id == expected_id,
        "feature record identity does not match its content for {}",
        record.target_key
    );
    Ok(())
}

fn load_projections(path: &Path) -> Result<BTreeMap<String, CompactTaskCompletionProjectionV1>> {
    let mut projections = BTreeMap::new();
    for (line_number, line) in lines(path)?.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let projection: CompactTaskCompletionProjectionV1 = serde_json::from_str(&line)
            .with_context(|| {
                format!(
                    "invalid projection at {}:{}",
                    path.display(),
                    line_number + 1
                )
            })?;
        projection
            .validate()
            .with_context(|| format!("invalid projection for {}", projection.target_key))?;
        let key = projection.target_key.clone();
        anyhow::ensure!(
            projections.insert(key.clone(), projection).is_none(),
            "duplicate {key}"
        );
    }
    anyhow::ensure!(
        !projections.is_empty(),
        "{} contains no projections",
        path.display()
    );
    Ok(projections)
}

fn load_results(path: &Path) -> Result<BTreeMap<String, RunRecord>> {
    let mut results = BTreeMap::new();
    for (line_number, line) in lines(path)?.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: RunRecord = serde_json::from_str(&line)
            .with_context(|| format!("invalid result at {}:{}", path.display(), line_number + 1))?;
        let key = record.target_key.clone();
        anyhow::ensure!(
            results.insert(key.clone(), record).is_none(),
            "duplicate {key}"
        );
    }
    anyhow::ensure!(
        !results.is_empty(),
        "{} contains no results",
        path.display()
    );
    Ok(results)
}

fn lines(path: &Path) -> Result<impl Iterator<Item = std::io::Result<String>>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    Ok(BufReader::new(file).lines())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_handles_absent_evidence() {
        assert_eq!(ratio(0.0, 0.0), 0.0);
        assert_eq!(ratio(1.0, 4.0), 0.25);
    }

    #[test]
    fn feature_schema_has_unique_names() {
        assert_eq!(
            FEATURE_NAMES.iter().copied().collect::<BTreeSet<_>>().len(),
            FEATURE_NAMES.len()
        );
    }

    #[test]
    fn feature_record_validation_rejects_tampering() {
        let names = FEATURE_NAMES.map(String::from).to_vec();
        let values = vec![0.0; FEATURE_NAMES.len()];
        let mut record = TaskCompletionEvidenceFeatureRecordV1 {
            schema_version: FEATURE_RECORD_SCHEMA_VERSION.into(),
            feature_set_version: FEATURE_SET_VERSION.into(),
            feature_record_id: String::new(),
            target_key: "trace-1".into(),
            target_revision: "revision-1".into(),
            trace_context_binding_id: "sha256:binding".into(),
            projection_hash: "sha256:projection".into(),
            projector_version: "projector-v1".into(),
            inference_model_id: "model-v1".into(),
            inference_prompt_version: SMOLLM_PROMPT_VERSION.into(),
            feature_names: names,
            feature_values: values,
        };
        record.feature_record_id = canonical_content_id(
            FEATURE_RECORD_SCHEMA_VERSION,
            &serde_json::json!({
                "feature_set_version": record.feature_set_version,
                "target_key": record.target_key,
                "target_revision": record.target_revision,
                "trace_context_binding_id": record.trace_context_binding_id,
                "projection_hash": record.projection_hash,
                "projector_version": record.projector_version,
                "inference_model_id": record.inference_model_id,
                "inference_prompt_version": record.inference_prompt_version,
                "feature_names": record.feature_names,
                "feature_values": record.feature_values,
            }),
        )
        .unwrap();
        assert!(validate_feature_record(&record).is_ok());
        record.feature_values[0] = 1.0;
        assert!(validate_feature_record(&record).is_err());
    }
}

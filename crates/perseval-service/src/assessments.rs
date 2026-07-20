use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use perseval_store::{
    AssessmentCommitV1, AssessmentItemStatusV1, ClaimedAssessmentItemV1, WorkspaceStore,
};
use traces_to_evals::{
    EvaluationEvidenceCatalogV1, LEARNED_EVALUATION_SCHEMA_VERSION, LearnedAbstentionReasonV1,
    LearnedEvaluationV1, LearnedVerdictV1,
};

use crate::config::AssessmentConfig;

/// Execution seam implemented by task-specific vertical slices. The worker owns
/// durable scheduling only; evaluator algorithms remain in `traces-to-evals`.
pub trait LearnedAssessmentExecutor: Send + Sync + 'static {
    fn estimated_cost_micros(&self, claim: Option<&ClaimedAssessmentItemV1>) -> u64;
    fn execute(&self, claim: &ClaimedAssessmentItemV1) -> AssessmentCommitV1;
}

/// PV-01's safe production executor. It never calls a provider. It records why a
/// task-specific evaluator cannot run; PV-02 installs the first real executor.
#[derive(Debug, Default)]
pub struct FoundationAssessmentExecutor;

impl LearnedAssessmentExecutor for FoundationAssessmentExecutor {
    fn estimated_cost_micros(&self, _claim: Option<&ClaimedAssessmentItemV1>) -> u64 {
        0
    }

    fn execute(&self, claim: &ClaimedAssessmentItemV1) -> AssessmentCommitV1 {
        if let Some(status @ AssessmentItemStatusV1::BudgetBlocked) = claim.preflight_status {
            return non_executable_commit(status, "Project daily assessment budget is exhausted.");
        }
        if claim.context_release_id.is_none() {
            return abstention_commit(
                claim,
                AssessmentItemStatusV1::Abstained,
                LearnedAbstentionReasonV1::ContextUnresolved,
                "No reviewed agent specification resolves to this exact trace revision.",
            );
        }
        abstention_commit(
            claim,
            AssessmentItemStatusV1::ProviderUnavailable,
            LearnedAbstentionReasonV1::ProviderUnavailable,
            "The assessment runtime is ready, but no task-specific learned evaluator is installed.",
        )
    }
}

pub(crate) struct AssessmentWorker {
    pub(crate) thread: thread::JoinHandle<()>,
}

pub(crate) fn spawn_assessment_worker(
    store: Arc<WorkspaceStore>,
    config: AssessmentConfig,
    executor: Arc<dyn LearnedAssessmentExecutor>,
    shutting_down: Arc<AtomicBool>,
) -> std::io::Result<AssessmentWorker> {
    let thread = thread::Builder::new()
        .name("perseval-learned-assessments".into())
        .spawn(move || {
            if !config.enabled {
                return;
            }
            let lease_owner = format!("assessment-worker-{}", std::process::id());
            while !shutting_down.load(Ordering::Acquire) {
                let estimate = config
                    .estimated_attempt_cost_micros
                    .max(executor.estimated_cost_micros(None));
                match store.claim_next_assessment(&lease_owner, estimate) {
                    Ok(Some(claim)) => {
                        // No SQLite guard exists here. Provider/model work is intentionally
                        // outside the durable writer transaction.
                        let commit = executor.execute(&claim);
                        let _ = store.commit_assessment_attempt(&claim, &commit);
                    }
                    Ok(None) | Err(_) => {
                        thread::sleep(Duration::from_millis(config.poll_interval_ms));
                    }
                }
            }
        })?;
    Ok(AssessmentWorker { thread })
}

fn abstention_commit(
    claim: &ClaimedAssessmentItemV1,
    status: AssessmentItemStatusV1,
    reason: LearnedAbstentionReasonV1,
    explanation: &str,
) -> AssessmentCommitV1 {
    let catalog = EvaluationEvidenceCatalogV1 {
        target_key: claim.logical_trace_id.clone(),
        target_revision: claim.revision.to_string(),
        projection_hash: claim.projection_hash.clone(),
        entries: Default::default(),
    };
    let evaluation = LearnedEvaluationV1 {
        schema_version: LEARNED_EVALUATION_SCHEMA_VERSION.into(),
        evaluator_release_id: claim.evaluator_release_id.clone(),
        target_key: claim.logical_trace_id.clone(),
        target_revision: claim.revision.to_string(),
        trace_context_binding_id: claim.context_binding_id.clone(),
        projection_hash: claim.projection_hash.clone(),
        verdict: LearnedVerdictV1::Abstain,
        label: None,
        score: None,
        model_reported_confidence: None,
        explanation: explanation.into(),
        evidence: Vec::new(),
        criteria: Vec::new(),
        abstention_reason: Some(reason),
    };
    AssessmentCommitV1 {
        status,
        evaluation: Some(evaluation),
        evidence_catalog: Some(catalog),
        provider_response: None,
        provider_failure: None,
        charged_cost_micros: 0,
        latency_ms: 0,
        retryable: false,
        error_code: None,
        error_message: None,
    }
}

fn non_executable_commit(status: AssessmentItemStatusV1, explanation: &str) -> AssessmentCommitV1 {
    AssessmentCommitV1 {
        status,
        evaluation: None,
        evidence_catalog: None,
        provider_response: None,
        provider_failure: None,
        charged_cost_micros: 0,
        latency_ms: 0,
        retryable: false,
        error_code: Some(
            match status {
                AssessmentItemStatusV1::BudgetBlocked => "budget_blocked",
                AssessmentItemStatusV1::PrivacyBlocked => "privacy_blocked",
                _ => "not_executable",
            }
            .into(),
        ),
        error_message: Some(explanation.into()),
    }
}

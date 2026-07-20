use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use perseval_store::{
    AssessmentCommitV1, AssessmentItemStatusV1, ClaimedAssessmentItemV1,
    TaskCompletionReleaseConfigV1, WorkspaceStore,
};
use traces_to_evals::{
    EvaluationEvidenceCatalogV1, EvaluatorReleaseSpecV1, LEARNED_EVALUATION_SCHEMA_VERSION,
    LearnedAbstentionReasonV1, LearnedEvaluationV1, LearnedVerdictV1,
    OpenAiTaskCompletionEvaluator, ProviderExecutionFailureV1, ProviderExecutionStageV1,
    ProviderResponseEnvelopeV1, TaskCompletionExecutionV1, TaskCompletionProjectionV1,
    TraceContextBindingV1,
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

pub trait TaskCompletionEvaluationRunner: Send + Sync + 'static {
    fn evaluate(
        &self,
        evaluator_release: EvaluatorReleaseSpecV1,
        config: &TaskCompletionReleaseConfigV1,
        projection: &TaskCompletionProjectionV1,
        binding: &TraceContextBindingV1,
    ) -> anyhow::Result<TaskCompletionExecutionV1>;
}

pub struct OpenAiTaskCompletionRunner {
    runtime: tokio::runtime::Runtime,
}

impl OpenAiTaskCompletionRunner {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name("perseval-task-completion-provider")
                .build()?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI provider credentials are not configured")]
struct MissingOpenAiApiKey;

impl TaskCompletionEvaluationRunner for OpenAiTaskCompletionRunner {
    fn evaluate(
        &self,
        evaluator_release: EvaluatorReleaseSpecV1,
        config: &TaskCompletionReleaseConfigV1,
        projection: &TaskCompletionProjectionV1,
        binding: &TraceContextBindingV1,
    ) -> anyhow::Result<TaskCompletionExecutionV1> {
        if std::env::var("OPENAI_API_KEY")
            .ok()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(MissingOpenAiApiKey.into());
        }
        let evaluator = OpenAiTaskCompletionEvaluator::from_env(
            config.requested_model.clone(),
            evaluator_release,
        )?;
        self.runtime
            .block_on(evaluator.evaluate(projection, binding))
    }
}

/// PV-02's provider-backed task-completion executor. All durable inputs are
/// loaded by exact project-scoped identity before the provider is considered.
pub struct TaskCompletionAssessmentExecutor {
    store: Arc<WorkspaceStore>,
    runner: Arc<dyn TaskCompletionEvaluationRunner>,
}

impl TaskCompletionAssessmentExecutor {
    pub fn openai(store: Arc<WorkspaceStore>) -> std::io::Result<Self> {
        Ok(Self {
            store,
            runner: Arc::new(OpenAiTaskCompletionRunner::new()?),
        })
    }

    pub fn with_runner(
        store: Arc<WorkspaceStore>,
        runner: Arc<dyn TaskCompletionEvaluationRunner>,
    ) -> Self {
        Self { store, runner }
    }

    fn load_inputs(
        &self,
        claim: &ClaimedAssessmentItemV1,
    ) -> Result<
        (
            EvaluatorReleaseSpecV1,
            TaskCompletionReleaseConfigV1,
            TaskCompletionProjectionV1,
            TraceContextBindingV1,
        ),
        String,
    > {
        let evaluator = self
            .store
            .evaluator_release(&claim.project_id, &claim.evaluator_release_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "exact evaluator release is unavailable".to_string())?;
        let config = self
            .store
            .task_completion_release_config(&claim.project_id, &claim.evaluator_release_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "task-completion execution configuration is unavailable".to_string())?;
        let projection = self
            .store
            .load_task_completion_projection(&claim.project_id, &claim.projection_hash)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "exact task-completion projection is unavailable".to_string())?;
        let binding = self
            .store
            .trace_context_binding(&claim.project_id, &claim.context_binding_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "exact trace-context binding is unavailable".to_string())?;
        if config.evaluator_release_id != claim.evaluator_release_id
            || projection.target_key != claim.logical_trace_id
            || projection.target_revision != claim.revision.to_string()
            || projection.projection_hash != claim.projection_hash
            || projection.trace_context_binding_id != claim.context_binding_id
        {
            return Err("assessment execution inputs do not match the claimed exact target".into());
        }
        Ok((evaluator, config, projection, binding))
    }
}

impl LearnedAssessmentExecutor for TaskCompletionAssessmentExecutor {
    fn estimated_cost_micros(&self, _claim: Option<&ClaimedAssessmentItemV1>) -> u64 {
        // PV-02 persists the exact preview high estimate on each job item. The
        // store uses that value before this executor is invoked.
        0
    }

    fn execute(&self, claim: &ClaimedAssessmentItemV1) -> AssessmentCommitV1 {
        if let Some(status @ AssessmentItemStatusV1::BudgetBlocked) = claim.preflight_status {
            return non_executable_commit(status, "Project daily assessment budget is exhausted.");
        }
        match self
            .store
            .task_completion_release_config(&claim.project_id, &claim.evaluator_release_id)
        {
            Ok(None) => return FoundationAssessmentExecutor.execute(claim),
            Err(error) => {
                return execution_failure_commit(
                    AssessmentItemStatusV1::Failed,
                    "assessment_configuration_unavailable",
                    &error.to_string(),
                    false,
                    None,
                    0,
                    0,
                );
            }
            Ok(Some(_)) => {}
        }
        let (evaluator, config, projection, binding) = match self.load_inputs(claim) {
            Ok(inputs) => inputs,
            Err(error) => {
                return execution_failure_commit(
                    AssessmentItemStatusV1::Failed,
                    "assessment_input_unavailable",
                    &error,
                    false,
                    None,
                    0,
                    0,
                );
            }
        };
        if let Some(status @ AssessmentItemStatusV1::ProviderUnavailable) = claim.preflight_status {
            return abstention_commit_with_catalog(
                claim,
                status,
                LearnedAbstentionReasonV1::ProviderUnavailable,
                "Hosted assessment execution is disabled by the project provider policy.",
                projection.evidence_catalog,
            );
        }

        let started = Instant::now();
        match self
            .runner
            .evaluate(evaluator, &config, &projection, &binding)
        {
            Ok(execution) => task_completion_execution_commit(
                execution,
                projection.evidence_catalog,
                &config,
                started,
            ),
            Err(error) => task_completion_error_commit(claim, &projection, &config, error, started),
        }
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
    abstention_commit_with_catalog(claim, status, reason, explanation, catalog)
}

fn abstention_commit_with_catalog(
    claim: &ClaimedAssessmentItemV1,
    status: AssessmentItemStatusV1,
    reason: LearnedAbstentionReasonV1,
    explanation: &str,
    catalog: EvaluationEvidenceCatalogV1,
) -> AssessmentCommitV1 {
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

fn task_completion_execution_commit(
    execution: TaskCompletionExecutionV1,
    evidence_catalog: EvaluationEvidenceCatalogV1,
    config: &TaskCompletionReleaseConfigV1,
    started: Instant,
) -> AssessmentCommitV1 {
    let status = match execution.evaluation.verdict {
        LearnedVerdictV1::Pass | LearnedVerdictV1::Fail => AssessmentItemStatusV1::Succeeded,
        LearnedVerdictV1::Abstain => match execution.evaluation.abstention_reason {
            Some(LearnedAbstentionReasonV1::PrivacyBlocked) => {
                AssessmentItemStatusV1::PrivacyBlocked
            }
            Some(LearnedAbstentionReasonV1::ProviderUnavailable) => {
                AssessmentItemStatusV1::ProviderUnavailable
            }
            Some(LearnedAbstentionReasonV1::NotApplicable) => AssessmentItemStatusV1::NotApplicable,
            _ => AssessmentItemStatusV1::Abstained,
        },
    };
    let charged_cost_micros = execution
        .provider
        .as_ref()
        .map(|response| provider_cost_micros(response, config))
        .unwrap_or(0);
    let latency_ms = execution
        .provider
        .as_ref()
        .map(|response| response.latency_ms)
        .unwrap_or_else(|| elapsed_millis(started));
    AssessmentCommitV1 {
        status,
        evaluation: Some(execution.evaluation),
        evidence_catalog: Some(evidence_catalog),
        provider_response: execution.provider,
        provider_failure: None,
        charged_cost_micros,
        latency_ms,
        retryable: false,
        error_code: None,
        error_message: None,
    }
}

fn task_completion_error_commit(
    claim: &ClaimedAssessmentItemV1,
    projection: &TaskCompletionProjectionV1,
    config: &TaskCompletionReleaseConfigV1,
    error: anyhow::Error,
    started: Instant,
) -> AssessmentCommitV1 {
    if error.downcast_ref::<MissingOpenAiApiKey>().is_some() {
        return abstention_commit_with_catalog(
            claim,
            AssessmentItemStatusV1::ProviderUnavailable,
            LearnedAbstentionReasonV1::ProviderUnavailable,
            "OpenAI provider credentials are not configured for assessment execution.",
            projection.evidence_catalog.clone(),
        );
    }
    if let Some(failure) = error.downcast_ref::<ProviderExecutionFailureV1>() {
        let failure = failure.clone();
        let response = failure.provider_response.clone();
        let failure_message = failure.message.clone();
        let charged_cost_micros = response
            .as_ref()
            .map(|response| provider_cost_micros(response, config))
            .unwrap_or(0);
        let latency_ms = failure.latency_ms.max(elapsed_millis(started));
        return match failure.stage {
            ProviderExecutionStageV1::Transport => execution_failure_commit(
                AssessmentItemStatusV1::ProviderUnavailable,
                "provider_transport_failure",
                &failure_message,
                true,
                Some(failure),
                charged_cost_micros,
                latency_ms,
            ),
            ProviderExecutionStageV1::OutputParsing => {
                let mut commit = abstention_commit_with_catalog(
                    claim,
                    AssessmentItemStatusV1::Abstained,
                    LearnedAbstentionReasonV1::InvalidProviderOutput,
                    "The provider response could not be validated as a task-completion judgment.",
                    projection.evidence_catalog.clone(),
                );
                commit.provider_response = response;
                commit.provider_failure = Some(failure);
                commit.charged_cost_micros = charged_cost_micros;
                commit.latency_ms = latency_ms;
                commit.error_code = Some("invalid_provider_output".into());
                commit.error_message = Some(
                    "The provider returned an invalid structured task-completion response.".into(),
                );
                commit
            }
            ProviderExecutionStageV1::ResponseValidation => execution_failure_commit(
                AssessmentItemStatusV1::Failed,
                "provider_response_invalid",
                &failure_message,
                false,
                Some(failure),
                charged_cost_micros,
                latency_ms,
            ),
        };
    }
    execution_failure_commit(
        AssessmentItemStatusV1::Failed,
        "task_completion_execution_failed",
        &error.to_string(),
        false,
        None,
        0,
        elapsed_millis(started),
    )
}

#[allow(clippy::too_many_arguments)]
fn execution_failure_commit(
    status: AssessmentItemStatusV1,
    error_code: &str,
    message: &str,
    retryable: bool,
    provider_failure: Option<ProviderExecutionFailureV1>,
    charged_cost_micros: u64,
    latency_ms: u64,
) -> AssessmentCommitV1 {
    let provider_response = provider_failure
        .as_ref()
        .and_then(|failure| failure.provider_response.clone());
    AssessmentCommitV1 {
        status,
        evaluation: None,
        evidence_catalog: None,
        provider_response,
        provider_failure,
        charged_cost_micros,
        latency_ms,
        retryable,
        error_code: Some(error_code.into()),
        error_message: Some(bounded_error_message(message)),
    }
}

fn provider_cost_micros(
    response: &ProviderResponseEnvelopeV1,
    config: &TaskCompletionReleaseConfigV1,
) -> u64 {
    let Some(usage) = &response.usage else {
        return 0;
    };
    let input_tokens = u64::from(usage.input_tokens.unwrap_or(0));
    let output_tokens = u64::from(usage.output_tokens.unwrap_or(0));
    input_tokens
        .saturating_mul(config.input_cost_micros_per_million_tokens)
        .saturating_add(output_tokens.saturating_mul(config.output_cost_micros_per_million_tokens))
        .saturating_add(999_999)
        / 1_000_000
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn bounded_error_message(message: &str) -> String {
    const MAX_ERROR_CHARS: usize = 2_048;
    let mut bounded = message.chars().take(MAX_ERROR_CHARS).collect::<String>();
    if message.chars().count() > MAX_ERROR_CHARS {
        bounded.push('…');
    }
    bounded
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

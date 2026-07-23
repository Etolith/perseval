use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use perseval_model_runtime::{TaskCompletionLabelV1, TaskCompletionOnnxRuntime};
use perseval_store::{
    AssessmentCommitV1, AssessmentItemStatusV1, ClaimedAssessmentItemV1,
    TaskCompletionReleaseConfigV1, WorkspaceStore,
};
use traces_to_evals::learned::{CompactTaskCompletionProjector, TaskCompletionTokenCounter};
use traces_to_evals::{
    CompactTaskCompletionVariantV1, EvaluationEvidenceCatalogV1, EvaluationEvidenceCitationV1,
    EvaluationImplementationV1, EvaluatorReleaseSpecV1, LEARNED_EVALUATION_SCHEMA_VERSION,
    LearnedAbstentionReasonV1, LearnedEvaluationV1, LearnedVerdictV1,
    OpenAiTaskCompletionEvaluator, ProviderExecutionFailureV1, ProviderExecutionStageV1,
    ProviderResponseEnvelopeV1, TaskCompletionExecutionV1, TaskCompletionProjectionV1,
    TraceContextBindingV1, TraceFactStatusV1,
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

struct RuntimeTokenCounter<'a>(&'a TaskCompletionOnnxRuntime);

impl TaskCompletionTokenCounter for RuntimeTokenCounter<'_> {
    fn tokenizer_id(&self) -> &str {
        self.0
            .manifest()
            .tokenizer_file
            .as_ref()
            .map_or("missing-tokenizer", |file| file.sha256.as_str())
    }

    fn count_tokens(&self, text: &str) -> Result<u32, String> {
        self.0.count_tokens(text).map_err(|error| error.to_string())
    }
}

/// Local task-completion execution uses the same immutable release and
/// assessment commit path as the cloud judge. Only inference is different.
pub struct LocalOnnxTaskCompletionRunner {
    runtime: Mutex<TaskCompletionOnnxRuntime>,
    projector: CompactTaskCompletionProjector,
}

impl LocalOnnxTaskCompletionRunner {
    pub fn load(artifact_dir: &Path) -> anyhow::Result<Self> {
        let runtime = TaskCompletionOnnxRuntime::load(artifact_dir)?;
        if runtime.manifest().tokenizer_file.is_none() {
            anyhow::bail!(
                "the local task-completion artifact must include a tokenizer for compact projection"
            );
        }
        Ok(Self {
            runtime: Mutex::new(runtime),
            projector: CompactTaskCompletionProjector::default(),
        })
    }
}

impl TaskCompletionEvaluationRunner for LocalOnnxTaskCompletionRunner {
    fn evaluate(
        &self,
        evaluator_release: EvaluatorReleaseSpecV1,
        _config: &TaskCompletionReleaseConfigV1,
        projection: &TaskCompletionProjectionV1,
        binding: &TraceContextBindingV1,
    ) -> anyhow::Result<TaskCompletionExecutionV1> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| anyhow::anyhow!("local task-completion runtime lock is poisoned"))?;
        runtime.bind_to_release(&evaluator_release)?;
        let compact = self.projector.project(
            projection,
            CompactTaskCompletionVariantV1::Complete,
            &RuntimeTokenCounter(&runtime),
        )?;
        if compact.projector_version != runtime.manifest().lineage.projector_version {
            anyhow::bail!(
                "verified model artifact was trained for projector {}, but runtime produced {}",
                runtime.manifest().lineage.projector_version,
                compact.projector_version
            );
        }
        let decision = runtime.decide_projection(&compact)?;
        let probability = f64::from(decision.calibrated_probability_complete);
        let (verdict, label, explanation) = match decision.label {
            TaskCompletionLabelV1::Complete => (
                LearnedVerdictV1::Pass,
                "complete",
                "The local learned judge found sufficient evidence that the requested task was completed.",
            ),
            TaskCompletionLabelV1::Incomplete => (
                LearnedVerdictV1::Fail,
                "incomplete",
                "The local learned judge found that completion was unsupported or incomplete.",
            ),
        };
        let evidence =
            if verdict == LearnedVerdictV1::Fail {
                compact
                    .facts
                    .iter()
                    .filter(|fact| fact.mandatory && fact.status != TraceFactStatusV1::Succeeded)
                    .chain(compact.facts.iter().filter(|fact| {
                        fact.mandatory && fact.status == TraceFactStatusV1::Succeeded
                    }))
                    .chain(compact.facts.iter().filter(|fact| !fact.mandatory))
                    .find_map(|fact| {
                        projection
                            .evidence_catalog
                            .entries
                            .get(&fact.evidence_key)
                            .map(|record| EvaluationEvidenceCitationV1 {
                                evidence_key: fact.evidence_key.clone(),
                                evidence_kind: record.evidence_kind,
                                location: record.location.clone(),
                                criterion_id: None,
                            })
                    })
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
        let evaluation = LearnedEvaluationV1 {
            schema_version: LEARNED_EVALUATION_SCHEMA_VERSION.into(),
            evaluator_release_id: evaluator_release.release_id()?,
            target_key: projection.target_key.clone(),
            target_revision: projection.target_revision.clone(),
            trace_context_binding_id: binding.binding_id()?,
            projection_hash: projection.projection_hash.clone(),
            verdict,
            label: Some(label.into()),
            score: Some(probability),
            model_reported_confidence: Some(probability.max(1.0 - probability)),
            explanation: explanation.into(),
            evidence,
            criteria: Vec::new(),
            abstention_reason: None,
        };
        evaluation.validate_against(&projection.evidence_catalog)?;
        Ok(TaskCompletionExecutionV1 {
            evaluation,
            provider: None,
        })
    }
}

pub struct ConfiguredTaskCompletionRunner {
    cloud: OpenAiTaskCompletionRunner,
    local: Option<LocalOnnxTaskCompletionRunner>,
}

impl ConfiguredTaskCompletionRunner {
    pub fn new(local_artifact_dir: Option<&Path>) -> anyhow::Result<Self> {
        Ok(Self {
            cloud: OpenAiTaskCompletionRunner::new()?,
            local: local_artifact_dir
                .map(LocalOnnxTaskCompletionRunner::load)
                .transpose()?,
        })
    }
}

impl TaskCompletionEvaluationRunner for ConfiguredTaskCompletionRunner {
    fn evaluate(
        &self,
        evaluator_release: EvaluatorReleaseSpecV1,
        config: &TaskCompletionReleaseConfigV1,
        projection: &TaskCompletionProjectionV1,
        binding: &TraceContextBindingV1,
    ) -> anyhow::Result<TaskCompletionExecutionV1> {
        match &evaluator_release.implementation {
            EvaluationImplementationV1::PromptJudge { .. } => {
                self.cloud
                    .evaluate(evaluator_release, config, projection, binding)
            }
            EvaluationImplementationV1::LocalClassifier { .. } => self
                .local
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no local task-completion artifact is configured"))?
                .evaluate(evaluator_release, config, projection, binding),
            _ => anyhow::bail!("unsupported task-completion evaluator implementation"),
        }
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

    pub fn configured(
        store: Arc<WorkspaceStore>,
        local_artifact_dir: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            store,
            runner: Arc::new(ConfiguredTaskCompletionRunner::new(local_artifact_dir)?),
        })
    }

    pub fn with_runner(
        store: Arc<WorkspaceStore>,
        runner: Arc<dyn TaskCompletionEvaluationRunner>,
    ) -> Self {
        Self { store, runner }
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
                    None,
                    0,
                    0,
                );
            }
            Ok(Some(_)) => {}
        }
        let inputs = match self.store.assessments().execution_inputs(claim) {
            Ok(inputs) => inputs,
            Err(error) => {
                return execution_failure_commit(
                    AssessmentItemStatusV1::Failed,
                    "assessment_input_unavailable",
                    &error.to_string(),
                    false,
                    None,
                    None,
                    0,
                    0,
                );
            }
        };
        let perseval_store::AssessmentExecutionInputsV1 {
            evaluator,
            config,
            projection,
            binding,
        } = inputs;
        if let Some(status @ AssessmentItemStatusV1::ProviderUnavailable) = claim.preflight_status
            && matches!(
                &evaluator.implementation,
                EvaluationImplementationV1::PromptJudge { .. }
            )
        {
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
                match store.assessments().claim_next(&lease_owner, estimate) {
                    Ok(Some(claim)) => {
                        // No SQLite guard exists here. Provider/model work is intentionally
                        // outside the durable writer transaction.
                        let commit = executor.execute(&claim);
                        let _ = store.assessments().commit_attempt(&claim, &commit);
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
                Some(projection.evidence_catalog.clone()),
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
                Some(projection.evidence_catalog.clone()),
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
        Some(projection.evidence_catalog.clone()),
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
    evidence_catalog: Option<EvaluationEvidenceCatalogV1>,
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
        evidence_catalog,
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

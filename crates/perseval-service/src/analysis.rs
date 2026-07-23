use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use perseval_store::{
    ANALYSIS_DEFINITION_SCHEMA_VERSION, ANALYSIS_IDENTITY_SCHEMA_VERSION,
    ANALYSIS_RESULT_SCHEMA_VERSION, AnalysisDefinitionV1, AnalysisIdentityV1, AnalysisResultV1,
    DEFAULT_ANALYSIS_GROUPING_VERSION, DEFAULT_ANALYSIS_RISK_MODEL_VERSION, PipelineStageSampleV1,
    PipelineStageV1, UNASSIGNED_PROJECT_ID, WorkspaceStore,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use traces_to_evals::{
    AgentBehaviorNormalizer, BEHAVIOR_INPUT_SCHEMA_VERSION, BehaviorFinding, CaseEmbedding,
    ClusterDiscovery, ClusterDiscoveryInput, ClusterDiscoveryOptions, ClusterLabeler,
    ClusterModelAssigner, ClusterQualityEvaluation, ClusterTextProjector,
    DefaultClusterTextProjector, DeterministicDetectorSet, DistanceMetric,
    EmbeddingClusterAssigner, EmbeddingProvider, FindingProjection, KMeansClusterDiscovery,
    OPENAI_SEMANTIC_BEHAVIOR_EVALUATOR_VERSION, OpenAiClusterLabeler, OpenAiEmbeddingProvider,
    OpenAiSemanticBehaviorEvaluator, OpenInferenceBehaviorNormalizer, ProjectName,
    SAFE_BEHAVIOR_PROJECTION_VERSION, SafeFindingProjector, SemanticBehaviorDetector,
    SemanticBehaviorPolicy, SemanticBehaviorProjector, SemanticContentPolicy,
    finding_projection_cases,
};

use crate::config::AnalysisConfig;
use crate::live::WorkspaceWriterHandle;

const FEATURE_SIMILARITY_SCOPE_ID: &str = "all-time-all-builds";

#[derive(Debug)]
struct TraceAnalysisJob {
    request: perseval_store::AnalysisRequestV1,
}

#[derive(Debug)]
struct CohortAssignmentJob {
    logical_trace_id: String,
}

#[derive(Debug)]
struct CohortRebuildJob {
    project_id: Option<String>,
}

#[derive(Debug)]
enum CohortJob {
    Assignment(CohortAssignmentJob),
    Rebuild(CohortRebuildJob),
}

pub(crate) struct AnalysisWorker {
    pub(crate) threads: Vec<thread::JoinHandle<()>>,
    pub(crate) cohort_control: Option<CohortControlHandle>,
    pub(crate) openai_health: OpenAiHealthHandle,
}

#[derive(Clone)]
pub(crate) struct OpenAiHealthHandle {
    state: Arc<OpenAiHealthState>,
}

struct OpenAiHealthState {
    enabled: bool,
    configured: bool,
    running_jobs: AtomicUsize,
    successful_jobs: AtomicU64,
    failed_jobs: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl OpenAiHealthHandle {
    fn new(enabled: bool, configured: bool) -> Self {
        Self {
            state: Arc::new(OpenAiHealthState {
                enabled,
                configured,
                running_jobs: AtomicUsize::new(0),
                successful_jobs: AtomicU64::new(0),
                failed_jobs: AtomicU64::new(0),
                last_error: Mutex::new(
                    (enabled && !configured)
                        .then(|| "OPENAI_API_KEY is not available to the Perseval process".into()),
                ),
            }),
        }
    }

    fn begin_job(&self) {
        self.state.running_jobs.fetch_add(1, Ordering::AcqRel);
    }

    fn finish_success(&self) {
        self.state.running_jobs.fetch_sub(1, Ordering::AcqRel);
        self.state.successful_jobs.fetch_add(1, Ordering::AcqRel);
        *self
            .state
            .last_error
            .lock()
            .expect("OpenAI health lock poisoned") = None;
    }

    fn finish_failure(&self, error: &str) {
        self.state.running_jobs.fetch_sub(1, Ordering::AcqRel);
        self.state.failed_jobs.fetch_add(1, Ordering::AcqRel);
        *self
            .state
            .last_error
            .lock()
            .expect("OpenAI health lock poisoned") = Some(provider_error_summary(error));
    }

    pub(crate) fn snapshot(&self) -> perseval_store::OpenAiProviderHealthV1 {
        let last_error = self
            .state
            .last_error
            .lock()
            .expect("OpenAI health lock poisoned")
            .clone();
        perseval_store::OpenAiProviderHealthV1 {
            enabled: self.state.enabled,
            configured: self.state.configured,
            running_jobs: self.state.running_jobs.load(Ordering::Acquire),
            successful_jobs: self.state.successful_jobs.load(Ordering::Acquire),
            failed_jobs: self.state.failed_jobs.load(Ordering::Acquire),
            degraded: self.state.enabled && last_error.is_some(),
            last_error,
        }
    }
}

fn provider_error_summary(error: &str) -> String {
    let error = error.to_ascii_lowercase();
    if error.contains("api key") || error.contains("auth") || error.contains("401") {
        "OpenAI authentication failed; check OPENAI_API_KEY".into()
    } else if error.contains("rate") || error.contains("quota") || error.contains("429") {
        "OpenAI rate limit or quota prevented analysis".into()
    } else if error.contains("model") || error.contains("404") {
        "The configured OpenAI model is unavailable".into()
    } else if error.contains("timeout") || error.contains("connect") || error.contains("network") {
        "OpenAI could not be reached from this process".into()
    } else {
        "OpenAI analysis failed; deterministic analysis remains available".into()
    }
}

fn openai_key_available() -> bool {
    std::env::var_os("OPENAI_API_KEY").is_some_and(|value| !value.is_empty())
}

#[derive(Clone)]
pub(crate) struct CohortControlHandle {
    sender: SyncSender<CohortJob>,
    rebuild_all: Arc<AtomicBool>,
    health: Arc<CohortHealthState>,
}

#[derive(Default)]
struct CohortHealthState {
    queued_assignments: std::sync::atomic::AtomicUsize,
    queued_rebuilds: std::sync::atomic::AtomicUsize,
    dirty_projects: std::sync::atomic::AtomicUsize,
    running: AtomicBool,
}

pub(crate) struct CohortHealthSnapshot {
    pub(crate) assignment_pending: usize,
    pub(crate) rebuild_pending: usize,
    pub(crate) running: bool,
}

impl CohortControlHandle {
    fn submit(&self, job: CohortJob) -> Result<(), String> {
        let assignment = matches!(&job, CohortJob::Assignment(_));
        let counter = if assignment {
            &self.health.queued_assignments
        } else {
            &self.health.queued_rebuilds
        };
        counter.fetch_add(1, Ordering::AcqRel);
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                counter.fetch_sub(1, Ordering::AcqRel);
                self.rebuild_all.store(true, Ordering::Release);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => {
                counter.fetch_sub(1, Ordering::AcqRel);
                Err("feature cohort worker is unavailable".into())
            }
        }
    }

    fn assign_trace(&self, logical_trace_id: String) {
        let _ = self.submit(CohortJob::Assignment(CohortAssignmentJob {
            logical_trace_id,
        }));
    }

    pub(crate) fn request_rebuild(&self, project_id: Option<String>) -> Result<(), String> {
        self.submit(CohortJob::Rebuild(CohortRebuildJob { project_id }))
    }

    pub(crate) fn health(&self) -> CohortHealthSnapshot {
        CohortHealthSnapshot {
            assignment_pending: self.health.queued_assignments.load(Ordering::Acquire),
            rebuild_pending: self
                .health
                .queued_rebuilds
                .load(Ordering::Acquire)
                .saturating_add(self.health.dirty_projects.load(Ordering::Acquire))
                .saturating_add(usize::from(self.rebuild_all.load(Ordering::Acquire))),
            running: self.health.running.load(Ordering::Acquire),
        }
    }
}

#[derive(Debug, Clone)]
struct CohortFeatureCacheEntry {
    projection: FindingProjection,
    embedding: CaseEmbedding,
}

#[derive(Debug)]
struct CohortFeatureCache {
    entries: HashMap<String, CohortFeatureCacheEntry>,
    insertion_order: VecDeque<String>,
    capacity: usize,
}

impl CohortFeatureCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(4_096)),
            insertion_order: VecDeque::with_capacity(capacity.min(4_096)),
            capacity,
        }
    }

    fn get(&self, key: &str) -> Option<CohortFeatureCacheEntry> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: String, entry: CohortFeatureCacheEntry) {
        if let Some(existing) = self.entries.get_mut(&key) {
            *existing = entry;
            return;
        }
        while self.entries.len() >= self.capacity {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.insertion_order.push_back(key.clone());
        self.entries.insert(key, entry);
    }
}

pub(crate) fn spawn_analysis_worker(
    store: Arc<WorkspaceStore>,
    writer: WorkspaceWriterHandle,
    config: AnalysisConfig,
    shutting_down: Arc<AtomicBool>,
) -> std::io::Result<AnalysisWorker> {
    let openai_enabled = config.openai.enabled
        && (config.openai.embeddings_enabled
            || config.openai.cluster_labels_enabled
            || config.openai.semantic_judge_enabled);
    let openai_configured = openai_key_available();
    if openai_enabled && !openai_configured {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "OpenAI analysis is enabled but OPENAI_API_KEY is unavailable",
        ));
    }
    let openai_health = OpenAiHealthHandle::new(openai_enabled, openai_configured);
    let cohort_available = config.feature_similarity_enabled
        && (!config.openai.embeddings_enabled || openai_configured);
    let (cohort_control, cohort_receiver) = if cohort_available {
        let (sender, receiver) = std::sync::mpsc::sync_channel(config.cohort_job_queue);
        (
            Some(CohortControlHandle {
                sender,
                rebuild_all: Arc::new(AtomicBool::new(false)),
                health: Arc::new(CohortHealthState::default()),
            }),
            Some(receiver),
        )
    } else {
        (None, None)
    };
    let normalizer = OpenInferenceBehaviorNormalizer::default();
    let detectors = DeterministicDetectorSet::default();
    let mut detector_versions = detectors.detector_versions();
    if config.openai.enabled && config.openai.semantic_judge_enabled {
        detector_versions.insert(
            format!(
                "semantic_behavior_judge/openai/{}",
                config.openai.chat_model
            ),
            OPENAI_SEMANTIC_BEHAVIOR_EVALUATOR_VERSION.into(),
        );
    }
    let expected = AnalysisDefinitionV1 {
        schema_version: ANALYSIS_DEFINITION_SCHEMA_VERSION.into(),
        input_schema_version: BEHAVIOR_INPUT_SCHEMA_VERSION.into(),
        projection_version: SAFE_BEHAVIOR_PROJECTION_VERSION.into(),
        adapter_id: normalizer.adapter().adapter_id.clone(),
        adapter_version: normalizer.adapter().adapter_version.clone(),
        detector_profile_id: detectors.profile().profile_id.clone(),
        detector_profile_version: detectors.profile().profile_version.clone(),
        detector_versions,
        grouping_version: DEFAULT_ANALYSIS_GROUPING_VERSION.into(),
        risk_model_version: DEFAULT_ANALYSIS_RISK_MODEL_VERSION.into(),
    };
    let semantic_runtime =
        (config.openai.enabled && config.openai.semantic_judge_enabled && openai_configured)
            .then(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
            })
            .transpose()?;
    let cohort_runtime = (cohort_available
        && config.openai.enabled
        && (config.openai.embeddings_enabled || config.openai.cluster_labels_enabled))
        .then(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
        })
        .transpose()?;
    let definition_id = analysis_definition_id(&expected);
    let worker_cohort_control = cohort_control.clone();
    let worker_openai_health = openai_health.clone();
    let cohort_thread = if let (Some(control), Some(receiver)) =
        (worker_cohort_control.as_ref(), cohort_receiver)
    {
        let cohort_store = Arc::clone(&store);
        let cohort_writer = writer.clone();
        let cohort_config = config.clone();
        let cohort_shutdown = Arc::clone(&shutting_down);
        let cohort_rebuild_all = Arc::clone(&control.rebuild_all);
        let cohort_health = Arc::clone(&control.health);
        let cohort_openai_health = worker_openai_health.clone();
        Some(
            thread::Builder::new()
                .name("perseval-feature-cohort".into())
                .spawn(move || {
                    run_cohort_worker(CohortWorkerContext {
                        store: cohort_store,
                        writer: cohort_writer,
                        config: cohort_config,
                        analysis_definition_id: definition_id,
                        shutting_down: cohort_shutdown,
                        receiver,
                        rebuild_all: cohort_rebuild_all,
                        health: cohort_health,
                        openai_health: cohort_openai_health,
                        openai_runtime: cohort_runtime,
                    });
                })?,
        )
    } else {
        None
    };
    let analysis_shutdown = Arc::clone(&shutting_down);
    let thread = thread::Builder::new()
        .name("perseval-finalized-analysis".into())
        .spawn(move || {
            while !analysis_shutdown.load(Ordering::Acquire) {
                match writer.enqueue_stale_analyses(expected.clone()) {
                    Ok(()) => break,
                    Err(_) => thread::sleep(Duration::from_millis(100)),
                }
            }
            if let Some(control) = &worker_cohort_control {
                let _ = control.request_rebuild(None);
            }
            while !analysis_shutdown.load(Ordering::Acquire) {
                let requests = match store.pending_analysis_requests(8) {
                    Ok(requests) => requests,
                    Err(_) => {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                };
                if requests.is_empty() {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                for request in requests {
                    let job = TraceAnalysisJob { request };
                    let request = job.request;
                    if analysis_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    match writer.mark_analysis_started(request.clone()) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(_) => continue,
                    }
                    let mut stage_samples = Vec::with_capacity(4);
                    let analyzed = (|| {
                        let projection_started = Instant::now();
                        let behavior_input = store
                            .load_behavior_input(&request.logical_trace_id, request.revision)?;
                        let mut projection =
                            stage_sample(PipelineStageV1::AnalysisProjection, projection_started);
                        projection.item_count = behavior_input.trace.spans.len() as u64;
                        projection.rows_scanned = behavior_input.trace.spans.len() as u64;
                        projection.rows_deserialized = behavior_input.trace.spans.len() as u64;
                        stage_samples.push(projection);

                        let normalization_started = Instant::now();
                        let behavior =
                            normalizer
                                .normalize_input(&behavior_input)
                                .map_err(|error| {
                                    perseval_store::StoreError::Invalid(error.to_string())
                                })?;
                        let behavior_fact_count = behavior
                            .turns
                            .len()
                            .saturating_add(behavior.tool_calls.len())
                            .saturating_add(behavior.policy_decisions.len());
                        let mut normalization =
                            stage_sample(PipelineStageV1::Normalization, normalization_started);
                        normalization.item_count = behavior_fact_count as u64;
                        stage_samples.push(normalization);

                        let detection_started = Instant::now();
                        let detection_report = detectors.detect_report(&behavior);
                        let mut findings = detection_report.findings.clone();
                        let mut semantic_succeeded = false;
                        if let Some(runtime) = semantic_runtime.as_ref() {
                            let evaluator = OpenAiSemanticBehaviorEvaluator::from_env(
                                config.openai.chat_model.clone(),
                            );
                            let detector = SemanticBehaviorDetector::new()
                                .with_projector(
                                    SemanticBehaviorProjector::new()
                                        .with_content_policy(SemanticContentPolicy::StructuredOnly),
                                )
                                .with_policy(SemanticBehaviorPolicy {
                                    minimum_failure_confidence: config
                                        .openai
                                        .minimum_failure_confidence_milli
                                        as f32
                                        / 1_000.0,
                                    emit_abstentions: config.openai.emit_abstentions,
                                    ..SemanticBehaviorPolicy::default()
                                });
                            worker_openai_health.begin_job();
                            match runtime.block_on(
                                detector.detect_traces(std::slice::from_ref(&behavior), &evaluator),
                            ) {
                                Ok(semantic) => {
                                    findings.extend(semantic.findings);
                                    semantic_succeeded = true;
                                    worker_openai_health.finish_success();
                                }
                                Err(error) => {
                                    worker_openai_health.finish_failure(&error.to_string());
                                }
                            }
                        }
                        let mut detection =
                            stage_sample(PipelineStageV1::Detection, detection_started);
                        detection.item_count = findings.len() as u64;
                        detection.rows_scanned = behavior_fact_count as u64;
                        stage_samples.push(detection);

                        let identity = AnalysisIdentityV1 {
                            schema_version: ANALYSIS_IDENTITY_SCHEMA_VERSION.into(),
                            logical_trace_id: request.logical_trace_id.clone(),
                            revision: request.revision,
                            input_schema_version: behavior_input.schema_version.clone(),
                            projection_version: behavior_input
                                .provenance
                                .projection_version
                                .clone(),
                            adapter_id: normalizer.adapter().adapter_id.clone(),
                            adapter_version: normalizer.adapter().adapter_version.clone(),
                            detector_profile_id: detection_report.profile.profile_id.clone(),
                            detector_profile_version: detection_report
                                .profile
                                .profile_version
                                .clone(),
                            detector_versions: if semantic_succeeded {
                                expected.detector_versions.clone()
                            } else {
                                detection_report.detector_versions.clone()
                            },
                            grouping_version: DEFAULT_ANALYSIS_GROUPING_VERSION.into(),
                            risk_model_version: DEFAULT_ANALYSIS_RISK_MODEL_VERSION.into(),
                        };
                        let result = AnalysisResultV1 {
                            schema_version: ANALYSIS_RESULT_SCHEMA_VERSION.into(),
                            analysis_id: identity.analysis_id(),
                            identity,
                            logical_trace_id: request.logical_trace_id.clone(),
                            revision: request.revision,
                            adapter_id: normalizer.adapter().adapter_id.clone(),
                            adapter_version: normalizer.adapter().adapter_version.clone(),
                            behavior,
                            detection_report,
                            findings,
                        };
                        Ok::<_, perseval_store::StoreError>(result)
                    })();
                    match analyzed {
                        Ok(result) => {
                            let logical_trace_id = result.logical_trace_id.clone();
                            if writer.commit_analysis(result, stage_samples).is_ok()
                                && let Some(control) = &worker_cohort_control
                            {
                                control.assign_trace(logical_trace_id);
                            }
                        }
                        Err(error) => {
                            let _ = writer.fail_analysis(request, error.to_string(), stage_samples);
                        }
                    }
                }
            }
        });
    let thread = match thread {
        Ok(thread) => thread,
        Err(error) => {
            shutting_down.store(true, Ordering::Release);
            if let Some(thread) = cohort_thread {
                let _ = thread.join();
            }
            return Err(error);
        }
    };
    let mut threads = vec![thread];
    threads.extend(cohort_thread);
    Ok(AnalysisWorker {
        threads,
        cohort_control,
        openai_health,
    })
}

struct CohortWorkerContext {
    store: Arc<WorkspaceStore>,
    writer: WorkspaceWriterHandle,
    config: AnalysisConfig,
    analysis_definition_id: String,
    shutting_down: Arc<AtomicBool>,
    receiver: Receiver<CohortJob>,
    rebuild_all: Arc<AtomicBool>,
    health: Arc<CohortHealthState>,
    openai_health: OpenAiHealthHandle,
    openai_runtime: Option<tokio::runtime::Runtime>,
}

fn run_cohort_worker(context: CohortWorkerContext) {
    let CohortWorkerContext {
        store,
        writer,
        config,
        analysis_definition_id,
        shutting_down,
        receiver,
        rebuild_all,
        health,
        openai_health,
        openai_runtime,
    } = context;
    let debounce = Duration::from_millis(config.cohort_rebuild_debounce_ms);
    let mut feature_cache = CohortFeatureCache::new(config.cohort_feature_cache_entries);
    let mut dirty_projects = BTreeSet::new();
    let mut last_change = Instant::now();
    let mut retry_not_before = None;
    let mut retry_delay = Duration::from_secs(5);
    while !shutting_down.load(Ordering::Acquire) {
        if rebuild_all.swap(false, Ordering::AcqRel) {
            if let Ok(projects) = store.active_finding_projects() {
                dirty_projects.extend(projects);
            }
            last_change = Instant::now();
            health
                .dirty_projects
                .store(dirty_projects.len(), Ordering::Release);
        }
        let timeout = if dirty_projects.is_empty() {
            Duration::from_millis(100)
        } else {
            let debounce_wait = debounce
                .saturating_sub(last_change.elapsed())
                .max(Duration::from_millis(1));
            let retry_wait = retry_not_before
                .map(|deadline: Instant| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_default();
            debounce_wait.max(retry_wait)
        };
        match receiver.recv_timeout(timeout) {
            Ok(CohortJob::Rebuild(job)) => {
                health.queued_rebuilds.fetch_sub(1, Ordering::AcqRel);
                if let Some(project_id) = job.project_id {
                    dirty_projects.insert(project_id);
                } else if let Ok(projects) = store.active_finding_projects() {
                    dirty_projects.extend(projects);
                }
                last_change = Instant::now();
                health
                    .dirty_projects
                    .store(dirty_projects.len(), Ordering::Release);
            }
            Ok(CohortJob::Assignment(CohortAssignmentJob { logical_trace_id })) => {
                health.queued_assignments.fetch_sub(1, Ordering::AcqRel);
                health.running.store(true, Ordering::Release);
                match assign_trace_to_active_cohort(
                    &ActiveCohortAssignmentContext {
                        store: &store,
                        writer: &writer,
                        config: &config,
                        analysis_definition_id: &analysis_definition_id,
                        openai_runtime: openai_runtime.as_ref(),
                        openai_health: &openai_health,
                    },
                    &logical_trace_id,
                    &mut feature_cache,
                ) {
                    Ok(Some(project_id)) => {
                        dirty_projects.insert(project_id);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        rebuild_all.store(true, Ordering::Release);
                        retry_not_before = Some(Instant::now() + retry_delay);
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
                    }
                }
                health.running.store(false, Ordering::Release);
                last_change = Instant::now();
                health
                    .dirty_projects
                    .store(dirty_projects.len(), Ordering::Release);
            }
            Err(RecvTimeoutError::Timeout)
                if !dirty_projects.is_empty()
                    && last_change.elapsed() >= debounce
                    && retry_not_before.is_none_or(|deadline| Instant::now() >= deadline)
                    && store
                        .analysis_counts()
                        .is_ok_and(|(pending, running)| pending == 0 && running == 0) =>
            {
                health.running.store(true, Ordering::Release);
                let pending = std::mem::take(&mut dirty_projects);
                let mut failed = false;
                for project_id in pending {
                    if !store
                        .project_cohort_input_settled(&project_id)
                        .unwrap_or(false)
                    {
                        dirty_projects.insert(project_id);
                        continue;
                    }
                    if refresh_feature_similarity_cohorts(
                        &FeatureSimilarityRebuildContext {
                            store: &store,
                            writer: &writer,
                            config: &config,
                            analysis_definition_id: &analysis_definition_id,
                            shutting_down: &shutting_down,
                            openai_runtime: openai_runtime.as_ref(),
                            openai_health: &openai_health,
                        },
                        &project_id,
                        FEATURE_SIMILARITY_SCOPE_ID,
                        &mut feature_cache,
                    )
                    .is_err()
                    {
                        dirty_projects.insert(project_id);
                        failed = true;
                    }
                }
                if failed {
                    retry_not_before = Some(Instant::now() + retry_delay);
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
                } else {
                    retry_not_before = None;
                    retry_delay = Duration::from_secs(5);
                }
                health.running.store(false, Ordering::Release);
                health
                    .dirty_projects
                    .store(dirty_projects.len(), Ordering::Release);
                last_change = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

struct ActiveCohortAssignmentContext<'a> {
    store: &'a WorkspaceStore,
    writer: &'a WorkspaceWriterHandle,
    config: &'a AnalysisConfig,
    analysis_definition_id: &'a str,
    openai_runtime: Option<&'a tokio::runtime::Runtime>,
    openai_health: &'a OpenAiHealthHandle,
}

fn assign_trace_to_active_cohort(
    context: &ActiveCohortAssignmentContext<'_>,
    logical_trace_id: &str,
    feature_cache: &mut CohortFeatureCache,
) -> Result<Option<String>, perseval_store::StoreError> {
    let store = context.store;
    let writer = context.writer;
    let config = context.config;
    let analysis_definition_id = context.analysis_definition_id;
    let Some(project_id) = store.project_for_trace(logical_trace_id)? else {
        return Ok(None);
    };
    if project_id == UNASSIGNED_PROJECT_ID {
        return Ok(None);
    }
    let findings = store.active_findings_for_trace(logical_trace_id)?;
    if findings.is_empty() {
        return Ok(Some(project_id));
    }
    let Some(model) = store.active_feature_similarity_model_for_scope(
        &project_id,
        analysis_definition_id,
        FEATURE_SIMILARITY_SCOPE_ID,
    )?
    else {
        return Ok(Some(project_id));
    };
    if !model_matches_scope(
        &model,
        &project_id,
        analysis_definition_id,
        FEATURE_SIMILARITY_SCOPE_ID,
        config,
    ) {
        return Ok(Some(project_id));
    }

    let features = project_and_embed_findings(
        &findings,
        config,
        feature_cache,
        None,
        context.openai_runtime,
        context.openai_health,
    )?;
    let cases = finding_projection_cases(&features.projections);
    let mut projection_sample =
        PipelineStageSampleV1::new(PipelineStageV1::CohortProjection, features.projection_nanos);
    projection_sample.item_count = cases.len() as u64;
    projection_sample.rows_scanned = findings.len() as u64;
    projection_sample.rows_deserialized = features.cache_misses as u64;

    let mut embedding_sample =
        PipelineStageSampleV1::new(PipelineStageV1::CohortEmbedding, features.embedding_nanos);
    embedding_sample.item_count = features.embeddings.len() as u64;
    embedding_sample.rows_deserialized = features.cache_misses as u64;
    let mut assigner = ClusterModelAssigner::new(model)
        .with_distance_metric(DistanceMetric::Cosine)
        .with_novelty_distance_threshold(config.novelty_distance_milli as f32 / 1_000.0);
    let assignments = assigner
        .assign_case_embeddings(&cases, &features.embeddings)
        .map_err(|error| perseval_store::StoreError::Invalid(error.to_string()))?;
    writer
        .append_cohort_assignments(
            project_id.clone(),
            analysis_definition_id.to_string(),
            FEATURE_SIMILARITY_SCOPE_ID.to_string(),
            assignments,
            vec![projection_sample, embedding_sample],
        )
        .map_err(perseval_store::StoreError::Invalid)?;

    let model = store.active_feature_similarity_model_for_scope(
        &project_id,
        analysis_definition_id,
        FEATURE_SIMILARITY_SCOPE_ID,
    )?;
    Ok(model
        .filter(|model| cohort_growth_requires_rebuild(model, config))
        .map(|_| project_id))
}

fn cohort_growth_requires_rebuild(
    model: &traces_to_evals::ClusterModel,
    config: &AnalysisConfig,
) -> bool {
    let baseline = model.source.case_count.max(1);
    let percentage_threshold = baseline
        .saturating_mul(config.cohort_rebuild_new_percent as usize)
        .div_ceil(100)
        .max(1);
    let threshold = percentage_threshold.min(config.cohort_rebuild_new_cases);
    let new_cases = model
        .metadata
        .get("perseval_incremental_assignment_count")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_else(|| {
            model
                .assignments
                .len()
                .saturating_sub(model.source.case_count)
        });
    let novel_cases = model
        .metadata
        .get("perseval_incremental_novelty_count")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0);
    let drift_exceeded = new_cases >= config.minimum_findings
        && novel_cases.saturating_mul(100)
            >= new_cases.saturating_mul(config.cohort_rebuild_novelty_percent as usize);
    new_cases >= threshold || drift_exceeded
}

struct FeatureSimilarityRebuildContext<'a> {
    store: &'a WorkspaceStore,
    writer: &'a WorkspaceWriterHandle,
    config: &'a AnalysisConfig,
    analysis_definition_id: &'a str,
    shutting_down: &'a AtomicBool,
    openai_runtime: Option<&'a tokio::runtime::Runtime>,
    openai_health: &'a OpenAiHealthHandle,
}

fn refresh_feature_similarity_cohorts(
    context: &FeatureSimilarityRebuildContext<'_>,
    project_id: &str,
    scope_id: &str,
    feature_cache: &mut CohortFeatureCache,
) -> Result<(), perseval_store::StoreError> {
    let store = context.store;
    let writer = context.writer;
    let config = context.config;
    let analysis_definition_id = context.analysis_definition_id;
    let shutting_down = context.shutting_down;
    if shutting_down.load(Ordering::Acquire) {
        return Err(perseval_store::StoreError::Cancelled);
    }
    let findings =
        store.active_findings_for_project_bounded(project_id, config.cohort_maximum_cases)?;
    if findings.len() < config.minimum_findings || config.embedding_dimensions == 0 {
        return Ok(());
    }

    let features = project_and_embed_findings(
        &findings,
        config,
        feature_cache,
        Some(shutting_down),
        context.openai_runtime,
        context.openai_health,
    )?;
    let cases = finding_projection_cases(&features.projections);
    let mut projection_sample =
        PipelineStageSampleV1::new(PipelineStageV1::CohortProjection, features.projection_nanos);
    projection_sample.item_count = findings.len() as u64;
    projection_sample.rows_scanned = findings.len() as u64;
    projection_sample.rows_deserialized = features.cache_misses as u64;

    let mut embedding_sample =
        PipelineStageSampleV1::new(PipelineStageV1::CohortEmbedding, features.embedding_nanos);
    embedding_sample.item_count = features.embeddings.len() as u64;
    embedding_sample.rows_deserialized = features.cache_misses as u64;

    let mut identity_material = features
        .projections
        .iter()
        .map(|projection| projection.text_hash.as_str())
        .collect::<Vec<_>>();
    identity_material.sort_unstable();
    let identity = feature_similarity_model_id(
        project_id,
        analysis_definition_id,
        scope_id,
        feature_embedding_provider(config),
        feature_embedding_model(config),
        config.embedding_dimensions,
        &identity_material,
    );
    let cluster_count = (findings.len() as f64).sqrt().ceil() as usize;
    let cluster_count = cluster_count
        .clamp(2, config.maximum_clusters.max(2))
        .min(findings.len());
    let mut options = ClusterDiscoveryOptions {
        model_id: Some(identity),
        distance_metric: DistanceMetric::Cosine,
        representative_count: 5.min(findings.len()),
        random_seed: 42,
        project_name: feature_similarity_project_name(project_id),
        quality_evaluation: ClusterQualityEvaluation::Sampled {
            maximum_cases: config.cohort_quality_sample_size,
        },
        novelty_distance_threshold: Some(config.novelty_distance_milli as f32 / 1_000.0),
        ..ClusterDiscoveryOptions::default()
    };
    options.metadata.insert(
        "perseval_grouping_role".into(),
        Value::String("secondary_feature_similarity_cohort".into()),
    );
    options.metadata.insert(
        "perseval_project_id".into(),
        Value::String(project_id.to_string()),
    );
    options.metadata.insert(
        "perseval_analysis_definition_id".into(),
        Value::String(analysis_definition_id.to_string()),
    );
    options.metadata.insert(
        "perseval_scope_id".into(),
        Value::String(scope_id.to_string()),
    );
    options.metadata.insert(
        "perseval_maximum_feature_cases".into(),
        Value::from(config.cohort_maximum_cases as u64),
    );
    options.metadata.insert(
        "perseval_embedding_provider".into(),
        Value::String(feature_embedding_provider(config).into()),
    );
    options.metadata.insert(
        "perseval_embedding_model".into(),
        Value::String(feature_embedding_model(config).into()),
    );
    options.metadata.insert(
        "perseval_embedding_dimensions".into(),
        Value::from(config.embedding_dimensions as u64),
    );
    let discovery = KMeansClusterDiscovery {
        k: cluster_count,
        max_iterations: 100,
        tolerance: 0.0001,
        random_seed: options.random_seed,
    };
    let previous = store
        .active_feature_similarity_model_for_scope(project_id, analysis_definition_id, scope_id)?
        .filter(|model| {
            model_matches_scope(model, project_id, analysis_definition_id, scope_id, config)
        });
    let fit_started = Instant::now();
    if shutting_down.load(Ordering::Acquire) {
        return Err(perseval_store::StoreError::Cancelled);
    }
    let mut model = discovery
        .fit(ClusterDiscoveryInput {
            cases: &cases,
            embeddings: Some(&features.embeddings),
            human_ratings: None,
            previous_results: None,
            options: &options,
        })
        .map_err(|error| perseval_store::StoreError::Invalid(error.to_string()))?;
    if shutting_down.load(Ordering::Acquire) {
        return Err(perseval_store::StoreError::Cancelled);
    }
    let mut fit_sample = stage_sample(PipelineStageV1::CohortFit, fit_started);
    fit_sample.item_count = model.assignments.len() as u64;

    let assignment_started = Instant::now();
    if let Some(previous) = previous {
        let mut assigner = ClusterModelAssigner::new(previous)
            .with_distance_metric(DistanceMetric::Cosine)
            .with_novelty_distance_threshold(config.novelty_distance_milli as f32 / 1_000.0);
        let prior_assignments = assigner
            .assign_case_embeddings(&cases, &features.embeddings)
            .map_err(|error| perseval_store::StoreError::Invalid(error.to_string()))?;
        let novelty = prior_assignments
            .into_iter()
            .map(|assignment| (assignment.case_id.clone(), assignment))
            .collect::<std::collections::HashMap<_, _>>();
        for assignment in &mut model.assignments {
            if let Some(prior) = novelty.get(&assignment.case_id) {
                assignment.novelty = prior.novelty;
                if prior.novelty {
                    assignment.metadata.insert(
                        "nearest_previous_cluster".into(),
                        prior
                            .metadata
                            .get("nearest_cluster_id")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                }
            }
        }
    }
    if config.openai.enabled
        && config.openai.cluster_labels_enabled
        && let Some(runtime) = context.openai_runtime
    {
        let labeler = OpenAiClusterLabeler::from_env(config.openai.chat_model.clone());
        context.openai_health.begin_job();
        match runtime.block_on(labeler.label_model(model.clone(), &cases)) {
            Ok(labeled) => {
                model = labeled;
                context.openai_health.finish_success();
            }
            Err(error) => context.openai_health.finish_failure(&error.to_string()),
        }
    }
    let mut assignment_sample = stage_sample(PipelineStageV1::CohortAssignment, assignment_started);
    assignment_sample.item_count = model.assignments.len() as u64;

    writer
        .commit_cohort(
            model,
            project_id.to_string(),
            analysis_definition_id.to_string(),
            scope_id.to_string(),
            config.cohort_model_history,
            vec![
                projection_sample,
                embedding_sample,
                fit_sample,
                assignment_sample,
            ],
        )
        .map_err(perseval_store::StoreError::Invalid)?;
    Ok(())
}

fn analysis_definition_id(definition: &AnalysisDefinitionV1) -> String {
    let encoded = serde_json::to_vec(definition).expect("analysis definition is serializable");
    format!("analysis-{:x}", Sha256::digest(encoded))
}

fn feature_similarity_model_id(
    project_id: &str,
    analysis_definition_id: &str,
    scope_id: &str,
    embedding_provider: &str,
    embedding_model: &str,
    embedding_dimensions: usize,
    projection_hashes: &[&str],
) -> String {
    let mut digest = Sha256::new();
    for value in [
        project_id,
        analysis_definition_id,
        scope_id,
        embedding_provider,
        embedding_model,
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    digest.update(embedding_dimensions.to_le_bytes());
    digest.update([0]);
    for hash in projection_hashes {
        digest.update(hash.as_bytes());
        digest.update([b'\n']);
    }
    format!("feature-similarity-{:x}", digest.finalize())
}

fn feature_similarity_project_name(project_id: &str) -> ProjectName {
    let digest = format!("{:x}", Sha256::digest(project_id.as_bytes()));
    ProjectName::new(format!("perseval-{}", &digest[..16]))
        .expect("hashed project namespace is always valid")
}

fn model_matches_scope(
    model: &traces_to_evals::ClusterModel,
    project_id: &str,
    analysis_definition_id: &str,
    scope_id: &str,
    config: &AnalysisConfig,
) -> bool {
    [
        ("perseval_project_id", project_id),
        ("perseval_analysis_definition_id", analysis_definition_id),
        ("perseval_scope_id", scope_id),
    ]
    .into_iter()
    .all(|(key, expected)| model.metadata.get(key).and_then(Value::as_str) == Some(expected))
        && model
            .metadata
            .get("perseval_embedding_provider")
            .and_then(Value::as_str)
            == Some(feature_embedding_provider(config))
        && model
            .metadata
            .get("perseval_embedding_model")
            .and_then(Value::as_str)
            == Some(feature_embedding_model(config))
        && model
            .metadata
            .get("perseval_embedding_dimensions")
            .and_then(Value::as_u64)
            == Some(config.embedding_dimensions as u64)
}

fn feature_embedding_provider(config: &AnalysisConfig) -> &str {
    if config.openai.enabled && config.openai.embeddings_enabled {
        "openai"
    } else {
        "perseval-local"
    }
}

fn feature_embedding_model(config: &AnalysisConfig) -> &str {
    if config.openai.enabled && config.openai.embeddings_enabled {
        &config.openai.embedding_model
    } else {
        "signed-feature-hash-v1"
    }
}

struct CohortFeatureBatch {
    projections: Vec<FindingProjection>,
    embeddings: Vec<CaseEmbedding>,
    cache_misses: usize,
    projection_nanos: u64,
    embedding_nanos: u64,
}

fn project_and_embed_findings(
    findings: &[BehaviorFinding],
    config: &AnalysisConfig,
    cache: &mut CohortFeatureCache,
    shutting_down: Option<&AtomicBool>,
    openai_runtime: Option<&tokio::runtime::Runtime>,
    openai_health: &OpenAiHealthHandle,
) -> Result<CohortFeatureBatch, perseval_store::StoreError> {
    const LOCAL_PROVIDER: &str = "perseval-local";
    const LOCAL_MODEL: &str = "signed-feature-hash-v1";
    let finding_projector = SafeFindingProjector::default();
    let text_projector = DefaultClusterTextProjector::default();
    let text_projection_version = text_projector.projection_version();
    let (provider, model) = if config.openai.enabled && config.openai.embeddings_enabled {
        ("openai", config.openai.embedding_model.as_str())
    } else {
        (LOCAL_PROVIDER, LOCAL_MODEL)
    };
    let mut entries = vec![None; findings.len()];
    let mut pending = Vec::new();
    let mut cache_misses = 0usize;
    let mut projection_nanos = 0u64;

    for (index, finding) in findings.iter().enumerate() {
        if shutting_down.is_some_and(|state| state.load(Ordering::Acquire)) {
            return Err(perseval_store::StoreError::Cancelled);
        }
        let key = cohort_feature_cache_key(
            finding,
            finding_projector.projection_version(),
            &text_projection_version,
            provider,
            model,
            config.embedding_dimensions,
        );
        if let Some(entry) = cache.get(&key) {
            entries[index] = Some(entry);
        } else {
            cache_misses = cache_misses.saturating_add(1);
            let started = Instant::now();
            let projection = finding_projector.project(finding);
            let projected = text_projector.project_case(&projection.to_eval_case());
            projection_nanos = projection_nanos.saturating_add(elapsed_nanos(started));
            pending.push((index, key, projection, projected));
        }
    }

    let started = Instant::now();
    let vectors = if pending.is_empty() {
        Vec::new()
    } else if config.openai.enabled && config.openai.embeddings_enabled {
        let runtime = openai_runtime.ok_or_else(|| {
            perseval_store::StoreError::Invalid("OpenAI runtime is unavailable".into())
        })?;
        let dimensions = u32::try_from(config.embedding_dimensions).map_err(|_| {
            perseval_store::StoreError::Invalid(
                "OpenAI embedding dimensions exceed the provider limit".into(),
            )
        })?;
        let provider = OpenAiEmbeddingProvider::from_env(config.openai.embedding_model.clone())
            .with_dimensions(dimensions)
            .with_batch_size(config.openai.embedding_batch_size);
        let texts = pending
            .iter()
            .map(|(_, _, _, projected)| projected.text.clone())
            .collect::<Vec<_>>();
        openai_health.begin_job();
        match runtime.block_on(provider.embed_texts(&texts)) {
            Ok(vectors) => {
                openai_health.finish_success();
                vectors
            }
            Err(error) => {
                openai_health.finish_failure(&error.to_string());
                return Err(perseval_store::StoreError::Invalid(
                    "OpenAI embedding request failed".into(),
                ));
            }
        }
    } else {
        pending
            .iter()
            .map(|(_, _, _, projected)| {
                feature_hash_embedding(&projected.text, config.embedding_dimensions)
            })
            .collect()
    };
    let embedding_nanos = elapsed_nanos(started);
    if vectors.len() != pending.len() {
        return Err(perseval_store::StoreError::Invalid(format!(
            "embedding provider returned {} vectors for {} findings",
            vectors.len(),
            pending.len()
        )));
    }
    for ((index, key, projection, projected), vector) in pending.into_iter().zip(vectors) {
        let embedding = CaseEmbedding::new(
            &projected,
            provider,
            model,
            vector,
            finding_projector.projection_version(),
        );
        let entry = CohortFeatureCacheEntry {
            projection,
            embedding,
        };
        cache.insert(key, entry.clone());
        entries[index] = Some(entry);
    }
    let entries = entries
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| perseval_store::StoreError::Invalid("missing cohort feature".into()))?;
    let projections = entries
        .iter()
        .map(|entry| entry.projection.clone())
        .collect();
    let embeddings = entries.into_iter().map(|entry| entry.embedding).collect();

    Ok(CohortFeatureBatch {
        projections,
        embeddings,
        cache_misses,
        projection_nanos,
        embedding_nanos,
    })
}

fn cohort_feature_cache_key(
    finding: &BehaviorFinding,
    finding_projection_version: &str,
    text_projection_version: &str,
    provider: &str,
    model: &str,
    dimensions: usize,
) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(finding).expect("behavior finding is serializable"));
    for value in [
        finding_projection_version,
        text_projection_version,
        provider,
        model,
    ] {
        digest.update([0]);
        digest.update(value.as_bytes());
    }
    digest.update([0]);
    digest.update(dimensions.to_le_bytes());
    format!("cohort-feature-{:x}", digest.finalize())
}

fn elapsed_nanos(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn stage_sample(stage: PipelineStageV1, started: Instant) -> PipelineStageSampleV1 {
    PipelineStageSampleV1::new(
        stage,
        started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
    )
}

fn feature_hash_embedding(text: &str, dimensions: usize) -> Vec<f32> {
    let tokens = text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut vector = vec![0.0_f32; dimensions];
    let mut add_feature = |feature: &str| {
        let digest = Sha256::digest(feature.as_bytes());
        let bucket = u64::from_le_bytes(digest[..8].try_into().expect("sha256 prefix")) as usize
            % dimensions;
        let sign = if digest[8] & 1 == 0 { 1.0 } else { -1.0 };
        vector[bucket] += sign;
    };
    for token in &tokens {
        add_feature(token);
    }
    for pair in tokens.windows(2) {
        add_feature(&format!("{}::{}", pair[0], pair[1]));
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

#[cfg(test)]
fn run_debounced_jobs(
    shutting_down: &AtomicBool,
    receiver: Receiver<()>,
    debounce: Duration,
    mut ready: impl FnMut() -> bool,
    mut run: impl FnMut() -> bool,
) {
    let mut dirty = false;
    while !shutting_down.load(Ordering::Acquire) {
        match receiver.recv_timeout(if dirty {
            debounce
        } else {
            Duration::from_millis(100)
        }) {
            Ok(()) => dirty = true,
            Err(RecvTimeoutError::Timeout) if dirty => {
                if ready() && run() {
                    dirty = false;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
#[path = "analysis/tests.rs"]
mod tests;

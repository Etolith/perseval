use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, path::Path};

use async_trait::async_trait;
use perseval_ingest::otlp::{
    OtlpAdmission, OtlpBatchSink, OtlpReceiverConfig, OtlpSubmission, OtlpSubmitError,
    prepare_otlp_submission,
};
use perseval_store::{
    AnalysisDefinitionV1, AnalysisRequestV1, AnalysisResultV1, BlobRefV1, CandidateGenerationJobV1,
    CreateProjectV1, EvalBatchPreviewV1, EvalBatchSelectionSpecV1, EvalCandidateRecordV1,
    EvalReviewDecisionV1, FindingDispositionStateV1, FindingDispositionV1, PipelineStageSampleV1,
    PipelineStageV1, ProjectV1, QueryScopeV1, ReviewEvalCandidateV1, RunComparisonRequestV1,
    RunFiltersV1, RunOrderV1, RunSummary, SourceHealth, SpanRow, SpanTreePageV1,
    TopologyProjectionJobV1, TopologyProjectionRowV1, TraceDeltaV1, WorkspaceStore,
    WorkspaceStoreLayout,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::analyzer::{CohortControlHandle, spawn_analysis_worker};
use crate::assessments::{FoundationAssessmentExecutor, spawn_assessment_worker};
use crate::config::PersevalConfigV1;
use crate::jobs::spawn_candidate_job_worker;
use crate::topology::spawn_topology_worker;
use traces_to_evals::{ClusterAssignment, ClusterModel, TraceAlignmentOptions, TraceComparison};

mod assessments;
mod local_import;
mod product;
mod writer;

use writer::writer_loop;

#[derive(Debug, Clone)]
pub struct TraceSnapshot {
    pub commit_sequence: u64,
    pub total_runs: u64,
    pub runs: Vec<RunSummary>,
    pub health: SourceHealth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobPreviewV1 {
    pub bytes: Vec<u8>,
    pub original_bytes: u64,
    pub revealed_bytes: u64,
    pub applied_limit_bytes: usize,
    pub truncated: bool,
    pub larger_local_reveal_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFileImportResultV1 {
    pub file_name: String,
    pub project_id: String,
    pub accepted_spans: u64,
    pub rejected_spans: u64,
    pub duplicate_request: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionError {
    ResyncRequired,
    Closed,
}

pub struct TraceSubscription {
    receiver: async_channel::Receiver<TraceDeltaV1>,
}

impl TraceSubscription {
    pub async fn recv(&self) -> Result<TraceDeltaV1, SubscriptionError> {
        self.receiver
            .recv()
            .await
            .map_err(|_| SubscriptionError::ResyncRequired)
    }

    pub fn try_recv(&self) -> Result<TraceDeltaV1, SubscriptionError> {
        self.receiver.try_recv().map_err(|error| match error {
            async_channel::TryRecvError::Empty => SubscriptionError::Closed,
            async_channel::TryRecvError::Closed => SubscriptionError::ResyncRequired,
        })
    }

    pub async fn recv_batch(&self, maximum: usize) -> Result<Vec<TraceDeltaV1>, SubscriptionError> {
        let mut deltas = vec![self.recv().await?];
        while deltas.len() < maximum {
            match self.receiver.try_recv() {
                Ok(delta) => deltas.push(delta),
                Err(async_channel::TryRecvError::Empty) => break,
                Err(async_channel::TryRecvError::Closed) => {
                    return Err(SubscriptionError::ResyncRequired);
                }
            }
        }
        Ok(deltas)
    }
}

#[derive(Debug, Error)]
pub enum LiveServiceError {
    #[error(transparent)]
    Store(#[from] perseval_store::StoreError),
    #[error("workspace writer is unavailable")]
    WriterUnavailable,
    #[error("workspace writer failed: {0}")]
    Writer(String),
    #[error("workspace payload policy denied this reveal: {0}")]
    PolicyDenied(String),
    #[error("trace file import failed: {0}")]
    InvalidImport(String),
}

#[derive(Debug, Clone, Default)]
pub struct ComparisonCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl ComparisonCancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct DeltaHub {
    subscribers: Mutex<Vec<async_channel::Sender<TraceDeltaV1>>>,
}

impl DeltaHub {
    fn subscribe(&self, capacity: usize) -> TraceSubscription {
        let (sender, receiver) = async_channel::bounded(capacity);
        self.subscribers
            .lock()
            .expect("subscriber lock poisoned")
            .push(sender);
        TraceSubscription { receiver }
    }

    fn publish(&self, delta: TraceDeltaV1) {
        let mut subscribers = self.subscribers.lock().expect("subscriber lock poisoned");
        subscribers.retain(|sender| match sender.try_send(delta.clone()) {
            Ok(()) => true,
            Err(async_channel::TrySendError::Full(_)) => {
                sender.close();
                false
            }
            Err(async_channel::TrySendError::Closed(_)) => false,
        });
    }
}

pub(crate) enum WriterCommand {
    Ingest {
        submission: OtlpSubmission,
        admitted_bytes: usize,
        response: mpsc::Sender<Result<OtlpAdmission, String>>,
    },
    CreateProject {
        request: CreateProjectV1,
        response: mpsc::Sender<Result<ProjectV1, String>>,
    },
    PreviewEvalBatch {
        project_id: String,
        selection_spec: EvalBatchSelectionSpecV1,
        response: mpsc::Sender<Result<EvalBatchPreviewV1, String>>,
    },
    CreateEvalBatch {
        project_id: String,
        preview_id: String,
        selection_hash: String,
        idempotency_key: String,
        response: mpsc::Sender<Result<CandidateGenerationJobV1, String>>,
    },
    CancelEvalBatch {
        job_id: String,
        response: mpsc::Sender<Result<CandidateGenerationJobV1, String>>,
    },
    RetryEvalBatch {
        job_id: String,
        response: mpsc::Sender<Result<CandidateGenerationJobV1, String>>,
    },
    ReviewEvalCandidate {
        request: ReviewEvalCandidateV1,
        response: mpsc::Sender<Result<EvalCandidateRecordV1, String>>,
    },
    SetFindingDisposition {
        scope: QueryScopeV1,
        group_id: String,
        finding_id: String,
        state: FindingDispositionStateV1,
        response: mpsc::Sender<Result<FindingDispositionV1, String>>,
    },
    UndoFindingDisposition {
        scope: QueryScopeV1,
        group_id: String,
        finding_id: String,
        response: mpsc::Sender<Result<bool, String>>,
    },
    CreateEvalCandidate {
        group_id: String,
        finding_id: String,
        response: mpsc::Sender<Result<Option<traces_to_evals::EvalCandidate>, String>>,
    },
    ExecuteCandidateJob {
        job_id: String,
        response: mpsc::Sender<Result<(), String>>,
    },
    CommitComparison {
        request: RunComparisonRequestV1,
        comparison: TraceComparison,
        response: mpsc::Sender<Result<(), String>>,
    },
    EnqueueStaleAnalyses {
        definition: AnalysisDefinitionV1,
        response: mpsc::Sender<Result<(), String>>,
    },
    MarkAnalysisStarted {
        request: AnalysisRequestV1,
        response: mpsc::Sender<Result<bool, String>>,
    },
    CommitAnalysis {
        result: Box<AnalysisResultV1>,
        stage_samples: Vec<PipelineStageSampleV1>,
        response: mpsc::Sender<Result<(), String>>,
    },
    FailAnalysis {
        request: AnalysisRequestV1,
        message: String,
        stage_samples: Vec<PipelineStageSampleV1>,
        response: mpsc::Sender<Result<(), String>>,
    },
    CommitCohort {
        model: ClusterModel,
        project_id: String,
        analysis_definition_id: String,
        scope_id: String,
        history_limit: usize,
        stage_samples: Vec<PipelineStageSampleV1>,
        response: mpsc::Sender<Result<bool, String>>,
    },
    AppendCohortAssignments {
        project_id: String,
        analysis_definition_id: String,
        scope_id: String,
        assignments: Vec<ClusterAssignment>,
        stage_samples: Vec<PipelineStageSampleV1>,
        response: mpsc::Sender<Result<u64, String>>,
    },
    ClaimTopology {
        response: mpsc::Sender<Result<Option<TopologyProjectionJobV1>, String>>,
    },
    CommitTopologyChunk {
        job: TopologyProjectionJobV1,
        rows: Vec<TopologyProjectionRowV1>,
        first: bool,
        last: bool,
        response: mpsc::Sender<Result<(), String>>,
    },
    FailTopology {
        job: TopologyProjectionJobV1,
        message: String,
        response: mpsc::Sender<Result<(), String>>,
    },
    Shutdown(mpsc::Sender<()>),
}

pub(crate) struct WriterShared {
    sender: mpsc::SyncSender<WriterCommand>,
    queued_batches: AtomicUsize,
    queued_bytes: AtomicUsize,
    shutting_down: AtomicBool,
    health: Mutex<SourceHealth>,
    queue_byte_capacity: usize,
}

#[derive(Clone)]
pub(crate) struct WorkspaceWriterHandle {
    sender: mpsc::SyncSender<WriterCommand>,
}

impl WorkspaceWriterHandle {
    fn request<T>(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<T, String>>) -> WriterCommand,
    ) -> Result<T, String> {
        let (response, receiver) = mpsc::channel();
        self.sender
            .send(command(response))
            .map_err(|_| "workspace writer is unavailable".to_string())?;
        receiver
            .recv()
            .map_err(|_| "workspace writer stopped before replying".to_string())?
    }

    pub(crate) fn enqueue_stale_analyses(
        &self,
        definition: AnalysisDefinitionV1,
    ) -> Result<(), String> {
        self.request(|response| WriterCommand::EnqueueStaleAnalyses {
            definition,
            response,
        })
    }

    pub(crate) fn mark_analysis_started(&self, request: AnalysisRequestV1) -> Result<bool, String> {
        self.request(|response| WriterCommand::MarkAnalysisStarted { request, response })
    }

    pub(crate) fn commit_analysis(
        &self,
        result: AnalysisResultV1,
        stage_samples: Vec<PipelineStageSampleV1>,
    ) -> Result<(), String> {
        self.request(|response| WriterCommand::CommitAnalysis {
            result: Box::new(result),
            stage_samples,
            response,
        })
    }

    pub(crate) fn fail_analysis(
        &self,
        request: AnalysisRequestV1,
        message: String,
        stage_samples: Vec<PipelineStageSampleV1>,
    ) -> Result<(), String> {
        self.request(|response| WriterCommand::FailAnalysis {
            request,
            message,
            stage_samples,
            response,
        })
    }

    pub(crate) fn commit_cohort(
        &self,
        model: ClusterModel,
        project_id: String,
        analysis_definition_id: String,
        scope_id: String,
        history_limit: usize,
        stage_samples: Vec<PipelineStageSampleV1>,
    ) -> Result<bool, String> {
        self.request(|response| WriterCommand::CommitCohort {
            model,
            project_id,
            analysis_definition_id,
            scope_id,
            history_limit,
            stage_samples,
            response,
        })
    }

    pub(crate) fn append_cohort_assignments(
        &self,
        project_id: String,
        analysis_definition_id: String,
        scope_id: String,
        assignments: Vec<ClusterAssignment>,
        stage_samples: Vec<PipelineStageSampleV1>,
    ) -> Result<u64, String> {
        self.request(|response| WriterCommand::AppendCohortAssignments {
            project_id,
            analysis_definition_id,
            scope_id,
            assignments,
            stage_samples,
            response,
        })
    }

    pub(crate) fn claim_topology(&self) -> Result<Option<TopologyProjectionJobV1>, String> {
        self.request(|response| WriterCommand::ClaimTopology { response })
    }

    pub(crate) fn commit_topology_chunk(
        &self,
        job: TopologyProjectionJobV1,
        rows: Vec<TopologyProjectionRowV1>,
        first: bool,
        last: bool,
    ) -> Result<(), String> {
        self.request(|response| WriterCommand::CommitTopologyChunk {
            job,
            rows,
            first,
            last,
            response,
        })
    }

    pub(crate) fn fail_topology(
        &self,
        job: TopologyProjectionJobV1,
        message: String,
    ) -> Result<(), String> {
        self.request(|response| WriterCommand::FailTopology {
            job,
            message,
            response,
        })
    }

    pub(crate) fn execute_candidate_job(&self, job_id: String) -> Result<(), String> {
        self.request(|response| WriterCommand::ExecuteCandidateJob { job_id, response })
    }

    fn commit_comparison(
        &self,
        request: RunComparisonRequestV1,
        comparison: TraceComparison,
    ) -> Result<(), String> {
        self.request(|response| WriterCommand::CommitComparison {
            request,
            comparison,
            response,
        })
    }
}

#[derive(Clone)]
pub struct LiveIngestHandle {
    shared: Arc<WriterShared>,
}

impl LiveIngestHandle {
    fn reserve_bytes(&self, bytes: usize) -> bool {
        let mut current = self.shared.queued_bytes.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > self.shared.queue_byte_capacity {
                return false;
            }
            match self.shared.queued_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, bytes: usize) {
        self.shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
        self.shared.queued_batches.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn health(&self) -> SourceHealth {
        let mut health = self
            .shared
            .health
            .lock()
            .expect("health lock poisoned")
            .clone();
        health.queue_batches = self.shared.queued_batches.load(Ordering::Relaxed);
        health.queue_bytes = self.shared.queued_bytes.load(Ordering::Relaxed);
        health.shutting_down = self.shared.shutting_down.load(Ordering::Relaxed);
        health.backpressured = health.queue_batches >= health.queue_batch_capacity
            || health.queue_bytes >= health.queue_byte_capacity;
        health
    }

    pub fn submit_blocking(
        &self,
        submission: OtlpSubmission,
    ) -> Result<OtlpAdmission, OtlpSubmitError> {
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(OtlpSubmitError::ShuttingDown);
        }
        let admitted_bytes = submission.raw_wire_payload.len().saturating_add(
            serde_json::to_vec(&submission.batch)
                .map(|value| value.len())
                .unwrap_or(0),
        );
        if !self.reserve_bytes(admitted_bytes) {
            let mut health = self.shared.health.lock().expect("health lock poisoned");
            health.rejected_spans = health
                .rejected_spans
                .saturating_add(submission.batch.spans.len() as u64);
            health.backpressured = true;
            return Err(OtlpSubmitError::Backpressured);
        }
        let (response_sender, response_receiver) = mpsc::channel();
        self.shared.queued_batches.fetch_add(1, Ordering::AcqRel);
        if self
            .shared
            .sender
            .try_send(WriterCommand::Ingest {
                submission,
                admitted_bytes,
                response: response_sender,
            })
            .is_err()
        {
            self.release(admitted_bytes);
            return Err(OtlpSubmitError::Backpressured);
        }
        response_receiver
            .recv()
            .map_err(|_| OtlpSubmitError::Unavailable("workspace writer stopped".into()))?
            .map_err(OtlpSubmitError::Unavailable)
    }
}

#[async_trait]
impl OtlpBatchSink for LiveIngestHandle {
    async fn submit(&self, submission: OtlpSubmission) -> Result<OtlpAdmission, OtlpSubmitError> {
        let handle = self.clone();
        tokio::task::spawn_blocking(move || handle.submit_blocking(submission))
            .await
            .map_err(|error| OtlpSubmitError::Unavailable(error.to_string()))?
    }
}

pub struct LiveTraceService {
    store: Arc<WorkspaceStore>,
    config: PersevalConfigV1,
    ingest: LiveIngestHandle,
    hub: Arc<DeltaHub>,
    writer_thread: Mutex<Option<thread::JoinHandle<()>>>,
    analysis_thread: Mutex<Option<thread::JoinHandle<()>>>,
    assessment_thread: Mutex<Option<thread::JoinHandle<()>>>,
    cohort_control: Option<CohortControlHandle>,
    openai_health: crate::analyzer::OpenAiHealthHandle,
    topology_thread: Mutex<Option<thread::JoinHandle<()>>>,
    analysis_shutdown: Arc<AtomicBool>,
    candidate_job_thread: Mutex<Option<thread::JoinHandle<()>>>,
    candidate_job_shutdown: Arc<AtomicBool>,
}

impl LiveTraceService {
    pub fn start(config: PersevalConfigV1) -> Result<Arc<Self>, LiveServiceError> {
        let layout = WorkspaceStoreLayout::new(&config.workspace_dir);
        let store = Arc::new(WorkspaceStore::open(&layout, &config.workspace_id)?);
        let (sender, receiver) = mpsc::sync_channel(config.stream.queue_batches);
        let hub = Arc::new(DeltaHub::default());
        let (accepted_spans, rejected_spans) = store.source_totals(&config.otlp.source_id)?;
        let health = SourceHealth {
            enabled: config.otlp.enabled,
            accepted_spans,
            rejected_spans,
            queue_batch_capacity: config.stream.queue_batches,
            queue_byte_capacity: config.stream.queue_bytes,
            ..SourceHealth::default()
        };
        let shared = Arc::new(WriterShared {
            sender,
            queued_batches: AtomicUsize::new(0),
            queued_bytes: AtomicUsize::new(0),
            shutting_down: AtomicBool::new(false),
            health: Mutex::new(health),
            queue_byte_capacity: config.stream.queue_bytes,
        });
        let ingest = LiveIngestHandle {
            shared: shared.clone(),
        };
        let workspace_writer = WorkspaceWriterHandle {
            sender: shared.sender.clone(),
        };
        let writer_store = store.clone();
        let writer_hub = hub.clone();
        let writer_config = config.clone();
        let writer_shared = shared.clone();
        let candidate_worker = spawn_candidate_job_worker(store.clone(), workspace_writer.clone())
            .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        let writer_candidate_job_sender = candidate_worker.sender.clone();
        let writer_thread = thread::Builder::new()
            .name("perseval-workspace-writer".into())
            .spawn(move || {
                writer_loop(
                    writer_store,
                    writer_hub,
                    writer_shared,
                    writer_config,
                    receiver,
                    writer_candidate_job_sender,
                )
            })
            .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        let analysis_shutdown = Arc::new(AtomicBool::new(false));
        let topology_thread = spawn_topology_worker(
            store.clone(),
            workspace_writer.clone(),
            config.stream.topology_chunk_rows,
            analysis_shutdown.clone(),
        )
        .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        let analysis_config = config.analysis.clone();
        let analysis_worker = spawn_analysis_worker(
            store.clone(),
            workspace_writer,
            analysis_config,
            analysis_shutdown.clone(),
        )
        .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        let assessment_worker = spawn_assessment_worker(
            store.clone(),
            config.assessments.clone(),
            Arc::new(FoundationAssessmentExecutor),
            analysis_shutdown.clone(),
        )
        .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        Ok(Arc::new(Self {
            store,
            config,
            ingest,
            hub,
            writer_thread: Mutex::new(Some(writer_thread)),
            analysis_thread: Mutex::new(Some(analysis_worker.thread)),
            assessment_thread: Mutex::new(Some(assessment_worker.thread)),
            cohort_control: analysis_worker.cohort_control,
            openai_health: analysis_worker.openai_health,
            topology_thread: Mutex::new(Some(topology_thread)),
            analysis_shutdown,
            candidate_job_thread: Mutex::new(Some(candidate_worker.thread)),
            candidate_job_shutdown: candidate_worker.shutdown,
        }))
    }

    pub fn ingest_handle(&self) -> LiveIngestHandle {
        self.ingest.clone()
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectV1>, LiveServiceError> {
        Ok(self.store.list_projects()?)
    }

    pub fn rebuild_feature_cohorts(
        &self,
        project_id: Option<&str>,
    ) -> Result<(), LiveServiceError> {
        let control = self.cohort_control.as_ref().ok_or_else(|| {
            LiveServiceError::Writer("feature-similarity cohorts are disabled".into())
        })?;
        control
            .request_rebuild(project_id.map(str::to_owned))
            .map_err(LiveServiceError::Writer)
    }

    pub fn create_project(&self, request: CreateProjectV1) -> Result<ProjectV1, LiveServiceError> {
        if self.ingest.shared.shutting_down.load(Ordering::Acquire) {
            return Err(LiveServiceError::WriterUnavailable);
        }
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::CreateProject { request, response })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv_timeout(Duration::from_millis(
                self.config.lifecycle.shutdown_drain_ms,
            ))
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn set_effective_address(&self, address: Option<String>) {
        self.ingest
            .shared
            .health
            .lock()
            .expect("health lock poisoned")
            .effective_address = address;
    }

    pub fn snapshot_and_subscribe(
        &self,
    ) -> Result<(TraceSnapshot, TraceSubscription), LiveServiceError> {
        let subscription = self.hub.subscribe(self.config.stream.subscriber_capacity);
        let sequence = self.store.latest_commit_sequence()?;
        let total_runs = self.store.run_count()?;
        let runs = self.store.list_runs(0, self.config.query.max_run_page)?;
        let snapshot = TraceSnapshot {
            commit_sequence: sequence,
            total_runs,
            runs,
            health: self.source_health()?,
        };
        Ok((snapshot, subscription))
    }

    pub fn source_health(&self) -> Result<SourceHealth, LiveServiceError> {
        let mut health = self.ingest.health();
        let (journal_lag, oldest_received_at) = self.store.projection_backlog()?;
        health.journal_lag = journal_lag;
        health.projection_lag = health.journal_lag;
        health.projection_backlog_age_ms = oldest_received_at
            .map(|received_at| now_unix_ms().saturating_sub(received_at).max(0) as u64)
            .unwrap_or(0);
        if health.last_error.is_none() {
            health.last_error.clone_from(&health.projection_last_error);
        }
        let (pending, running) = self.store.analysis_counts()?;
        health.analysis_pending = pending;
        health.analysis_running = running;
        if let Some(control) = &self.cohort_control {
            let cohort = control.health();
            health.cohort_assignment_pending = cohort.assignment_pending as u64;
            health.cohort_rebuild_pending = cohort.rebuild_pending as u64;
            health.cohort_running = cohort.running;
        }
        health.openai = self.openai_health.snapshot();
        let (topology_pending, topology_running) = self.store.topology_counts()?;
        health.topology_pending = topology_pending;
        health.topology_running = topology_running;
        let (live, quiescent, finalized, reopened) = self.store.lifecycle_counts()?;
        health.live_runs = live;
        health.quiescent_runs = quiescent;
        health.finalized_runs = finalized;
        health.reopened_runs = reopened;
        Ok(health)
    }

    pub fn list_runs(&self, offset: u64, limit: u32) -> Result<Vec<RunSummary>, LiveServiceError> {
        Ok(self
            .store
            .list_runs(offset, limit.min(self.config.query.max_run_page))?)
    }

    pub fn run_count(&self) -> Result<u64, LiveServiceError> {
        Ok(self.store.run_count()?)
    }

    pub fn commit_sequence(&self) -> Result<u64, LiveServiceError> {
        Ok(self.store.latest_commit_sequence()?)
    }

    pub fn list_runs_filtered(
        &self,
        filters: &RunFiltersV1,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<RunSummary>, LiveServiceError> {
        self.list_runs_filtered_ordered(filters, RunOrderV1::Newest, offset, limit)
    }

    pub fn list_runs_filtered_ordered(
        &self,
        filters: &RunFiltersV1,
        order: RunOrderV1,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<RunSummary>, LiveServiceError> {
        Ok(self.store.list_runs_filtered_ordered(
            filters,
            order,
            offset,
            limit.min(self.config.query.max_run_page),
        )?)
    }

    pub fn run_count_filtered(&self, filters: &RunFiltersV1) -> Result<u64, LiveServiceError> {
        Ok(self.store.run_count_filtered(filters)?)
    }

    pub fn compare_runs(
        &self,
        request: &RunComparisonRequestV1,
    ) -> Result<TraceComparison, LiveServiceError> {
        self.compare_runs_cancellable(request, &ComparisonCancellationToken::default())
    }

    pub fn compare_runs_cancellable(
        &self,
        request: &RunComparisonRequestV1,
        cancellation: &ComparisonCancellationToken,
    ) -> Result<TraceComparison, LiveServiceError> {
        let comparison = self.store.build_run_comparison_cancellable(
            request,
            self.config.query.comparison_max_input_steps,
            TraceAlignmentOptions {
                lookahead: self.config.query.comparison_lookahead,
                context_before: 12,
                maximum_rows: self.config.query.comparison_max_rows,
            },
            || cancellation.is_cancelled(),
        )?;
        WorkspaceWriterHandle {
            sender: self.ingest.shared.sender.clone(),
        }
        .commit_comparison(request.clone(), comparison.clone())
        .map_err(LiveServiceError::Writer)?;
        Ok(comparison)
    }

    pub fn get_trace_comparison(
        &self,
        comparison_id: &str,
    ) -> Result<Option<TraceComparison>, LiveServiceError> {
        Ok(self.store.get_trace_comparison(comparison_id)?)
    }

    pub fn get_run(&self, trace_id: &str) -> Result<Option<RunSummary>, LiveServiceError> {
        Ok(self.store.get_run(trace_id)?)
    }

    pub fn list_spans(
        &self,
        trace_id: &str,
        revision: u64,
        offset: u64,
        limit: u32,
        category: Option<&str>,
        errors_only: bool,
    ) -> Result<Vec<SpanRow>, LiveServiceError> {
        Ok(self.store.list_spans(
            trace_id,
            revision,
            offset,
            limit.min(self.config.query.max_span_page),
            category,
            errors_only,
        )?)
    }

    pub fn list_spans_timeline(
        &self,
        trace_id: &str,
        revision: u64,
        offset: u64,
        limit: u32,
        category: Option<&str>,
        errors_only: bool,
    ) -> Result<Vec<SpanRow>, LiveServiceError> {
        Ok(self.store.list_spans_timeline(
            trace_id,
            revision,
            offset,
            limit.min(self.config.query.max_span_page),
            category,
            errors_only,
        )?)
    }

    pub fn get_span(
        &self,
        trace_id: &str,
        revision: u64,
        span_id: &str,
    ) -> Result<Option<SpanRow>, LiveServiceError> {
        Ok(self.store.get_span(trace_id, revision, span_id)?)
    }

    pub fn span_tree_page(
        &self,
        trace_id: &str,
        revision: u64,
        parent_span_id: Option<&str>,
        offset: u64,
        limit: u32,
    ) -> Result<SpanTreePageV1, LiveServiceError> {
        Ok(self.store.span_tree_page(
            trace_id,
            revision,
            parent_span_id,
            offset,
            limit.min(self.config.query.max_span_page),
        )?)
    }

    pub fn span_count(
        &self,
        trace_id: &str,
        revision: u64,
        category: Option<&str>,
        errors_only: bool,
    ) -> Result<u64, LiveServiceError> {
        Ok(self
            .store
            .span_count(trace_id, revision, category, errors_only)?)
    }

    pub fn reveal_blob(&self, hash: &str, requested: usize) -> Result<Vec<u8>, LiveServiceError> {
        Ok(self
            .store
            .reveal_blob(hash, requested.min(self.config.query.blob_preview_bytes))?)
    }

    pub fn reveal_blob_preview(&self, blob: &BlobRefV1) -> Result<BlobPreviewV1, LiveServiceError> {
        self.reveal_blob_with_limit(blob, self.config.query.blob_preview_bytes)
    }

    pub fn reveal_blob_larger_local(
        &self,
        blob: &BlobRefV1,
    ) -> Result<BlobPreviewV1, LiveServiceError> {
        if !self.config.blobs.allow_larger_local_reveal {
            return Err(LiveServiceError::PolicyDenied(
                "enable blobs.allow_larger_local_reveal for this local workspace".into(),
            ));
        }
        self.reveal_blob_with_limit(blob, self.config.blobs.maximum_local_reveal_bytes)
    }

    fn reveal_blob_with_limit(
        &self,
        blob: &BlobRefV1,
        limit: usize,
    ) -> Result<BlobPreviewV1, LiveServiceError> {
        let limit = limit.max(1);
        let bytes = self.store.reveal_blob(&blob.sha256, limit)?;
        let revealed_bytes = bytes.len() as u64;
        Ok(BlobPreviewV1 {
            bytes,
            original_bytes: blob.original_bytes,
            revealed_bytes,
            applied_limit_bytes: limit,
            truncated: revealed_bytes < blob.original_bytes,
            larger_local_reveal_allowed: self.config.blobs.allow_larger_local_reveal,
        })
    }

    pub fn shutdown(&self) {
        if self
            .ingest
            .shared
            .shutting_down
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        self.analysis_shutdown.store(true, Ordering::Release);
        self.candidate_job_shutdown.store(true, Ordering::Release);
        if let Some(thread) = self
            .candidate_job_thread
            .lock()
            .expect("candidate job thread lock poisoned")
            .take()
        {
            let _ = thread.join();
        }
        if let Some(thread) = self
            .analysis_thread
            .lock()
            .expect("analysis thread lock poisoned")
            .take()
        {
            let _ = thread.join();
        }
        if let Some(thread) = self
            .assessment_thread
            .lock()
            .expect("assessment thread lock poisoned")
            .take()
        {
            let _ = thread.join();
        }
        if let Some(thread) = self
            .topology_thread
            .lock()
            .expect("topology thread lock poisoned")
            .take()
        {
            let _ = thread.join();
        }
        let (sender, receiver) = mpsc::channel();
        let _ = self
            .ingest
            .shared
            .sender
            .send(WriterCommand::Shutdown(sender));
        let _ = receiver.recv_timeout(Duration::from_millis(
            self.config.lifecycle.shutdown_drain_ms,
        ));
        if let Some(thread) = self
            .writer_thread
            .lock()
            .expect("writer thread lock poisoned")
            .take()
        {
            let _ = thread.join();
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

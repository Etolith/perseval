mod analysis;
mod assessments;
mod comparison;
mod context;
mod diagnostics;
mod ingest;
mod projection;
mod projects;
mod read;
mod schema;
mod span_tree;
mod taxonomy;
mod topology;

use projection::{
    ensure_logical_trace, insert_delta_locked, insert_delta_transaction, map_run, query_run_locked,
    query_run_transaction,
};
use schema::{migrate_analytics, migrate_control};
use topology::{has_persisted_topology, recover_topology_jobs};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use duckdb::{Connection as DuckConnection, OptionalExt as DuckOptionalExt, params as duck_params};
use rusqlite::{Connection as SqliteConnection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use traces_to_evals::{
    AgentBehaviorTrace, BehaviorFinding, BehaviorInputProvenanceV1, BehaviorInputV1,
    CandidateReview, CandidateReviewDecision, ClusterAssignment, ClusterModel, EvalCandidate,
    EvalCandidateStatus, EvidencePacketBuilder, FactQuality, FindingEvalCandidateGenerator,
    FindingPresenter, FindingSeverity, KnownSignatureGrouper, PayloadIdentity, RecoveryStatus,
    SAFE_BEHAVIOR_PROJECTION_VERSION, SourceSpanStatus, Span, SpanEvent, SpanKind, SpanLink,
    SpanProvenance, Trace,
};

use crate::blobs::FsBlobStore;
use crate::layout::WorkspaceStoreLayout;
use crate::model::{
    ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION, ANALYSIS_IDENTITY_SCHEMA_VERSION,
    ANALYSIS_RESULT_SCHEMA_VERSION, AnalysisDefinitionV1, AnalysisRequestV1, AnalysisResultV1,
    AnalysisStatus, BlobRefV1, CANDIDATE_GENERATION_JOB_SCHEMA_VERSION,
    CandidateGenerationItemOutcomeV1, CandidateGenerationJobStatusV1, CandidateGenerationJobV1,
    CandidateGenerationOutcomeKindV1, CreateProjectV1, EVAL_BATCH_PREVIEW_SCHEMA_VERSION,
    EVAL_CANDIDATE_RECORD_SCHEMA_VERSION, EvalBatchExclusionV1, EvalBatchItemPreviewV1,
    EvalBatchPreviewV1, EvalBatchSelectionSpecV1, EvalCandidatePreview, EvalCandidateRecordV1,
    EvalReviewDecisionV1, EvalReviewQueueStateV1, EvalSelectionReasonV1, FailureFiltersV1,
    FailureGroupDetail, FailureGroupPageV1, FailureGroupSummary, FailureOccurrence,
    FeatureSimilarityCohortSummary, FindingDispositionStateV1, FindingDispositionV1,
    FindingEvidence, IdentityQualityV1, PAYLOAD_IDENTITY_SCHEMA_VERSION, PROJECT_SCHEMA_VERSION,
    PayloadIdentityQualityV1, PayloadIdentityV1, PipelineStageAggregateV1, PipelineStageSampleV1,
    PipelineStageV1, ProjectV1, ReviewEvalCandidateV1, RunFiltersV1, RunOrderV1, RunSummary,
    SpanRow, SpanUpsertBatchV1, SpanUpsertV1, TRACE_DELTA_SCHEMA_VERSION, TopologyProjectionJobV1,
    TopologyProjectionRowV1, TraceChangeKind, TraceDeltaV1, TraceLifecycle, UNASSIGNED_PROJECT_ID,
};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("DuckDB error: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid persisted value: {0}")]
    Invalid(String),
    #[error("operation cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalReceipt {
    pub journal_sequence: u64,
    pub duplicate_request: bool,
    pub raw_blob: BlobRefV1,
    pub normalized_blob: BlobRefV1,
    pub stage_samples: Vec<crate::model::PipelineStageSampleV1>,
}

pub struct WorkspaceStore {
    workspace_id: String,
    control: Mutex<SqliteConnection>,
    analytics: Mutex<DuckConnection>,
    analytics_reads: AnalyticsReadPool,
    live_topologies: Mutex<topology::LiveTopologyCache>,
    blobs: FsBlobStore,
    pipeline_metrics: Mutex<BTreeMap<PipelineStageV1, PipelineStageAggregateV1>>,
}

const ANALYTICS_READ_POOL_SIZE: usize = 2;

struct AnalyticsReadPool {
    connections: Vec<Mutex<DuckConnection>>,
    next: AtomicUsize,
}

impl AnalyticsReadPool {
    fn clone_from(writer: &DuckConnection, size: usize) -> Result<Self, StoreError> {
        let connections = (0..size.max(1))
            .map(|_| writer.try_clone().map(Mutex::new))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            connections,
            next: AtomicUsize::new(0),
        })
    }

    fn connection(&self) -> MutexGuard<'_, DuckConnection> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.connections.len();
        self.connections[index]
            .lock()
            .expect("analytics read connection lock poisoned")
    }
}

impl WorkspaceStore {
    pub fn open(
        layout: &WorkspaceStoreLayout,
        workspace_id: impl Into<String>,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(layout.root())?;
        set_mode(layout.root(), 0o700)?;
        std::fs::create_dir_all(layout.analytics_directory())?;
        set_mode(&layout.analytics_directory(), 0o700)?;

        let control = SqliteConnection::open(layout.control_database())?;
        control.pragma_update(None, "journal_mode", "WAL")?;
        control.pragma_update(None, "synchronous", "FULL")?;
        control.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate_control(&control)?;
        set_mode(&layout.control_database(), 0o600)?;

        let analytics_path = layout.analytics_directory().join("traces.duckdb");
        let analytics = DuckConnection::open(&analytics_path)?;
        migrate_analytics(&analytics)?;
        recover_topology_jobs(&control)?;
        let analytics_reads = AnalyticsReadPool::clone_from(&analytics, ANALYTICS_READ_POOL_SIZE)?;
        set_mode(&analytics_path, 0o600)?;

        let store = Self {
            workspace_id: workspace_id.into(),
            control: Mutex::new(control),
            analytics: Mutex::new(analytics),
            analytics_reads,
            live_topologies: Mutex::new(topology::LiveTopologyCache::default()),
            blobs: FsBlobStore::open(layout.blob_directory())?,
            pipeline_metrics: Mutex::new(BTreeMap::new()),
        };
        store.backfill_active_failure_projection()?;
        Ok(store)
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

fn string_attr(map: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn is_payload_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if is_analysis_payload_key(&key) {
        return true;
    }
    [
        "input",
        "output",
        "message",
        "prompt",
        "reasoning",
        "tool.arguments",
        "tool.result",
        "tool.call.arguments",
        "tool.call.result",
        "source.code",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn is_analysis_payload_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "input"
            | "input.value"
            | "output"
            | "output.value"
            | "tool.arguments"
            | "tool.result"
            | "gen_ai.tool.call.arguments"
            | "gen_ai.tool.call.result"
            | "traceeval.tool.invocation"
            | "traceeval.tool.result"
    )
}

fn source_span_status(status_code: i32) -> SourceSpanStatus {
    match status_code {
        1 => SourceSpanStatus::Ok,
        2 => SourceSpanStatus::Error,
        _ => SourceSpanStatus::Unset,
    }
}

fn safe_analysis_attributes(attributes: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    attributes
        .iter()
        .filter(|(key, value)| is_safe_analysis_attribute_key(key) && is_bounded_safe_scalar(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_safe_analysis_attribute_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "openinference.span.kind"
            | "gen_ai.operation.name"
            | "gen_ai.tool.name"
            | "gen_ai.tool.call.id"
            | "gen_ai.tool.status"
            | "agent.operation"
            | "agent.operation.effect"
            | "agent.operation.retry_safety"
            | "agent.tool.requirement"
            | "agent.tool.attempt"
            | "agent.tool.status"
            | "agent.approval.required"
            | "agent.approval.outcome"
            | "agent.state.observation"
            | "agent.state.predicate"
            | "agent.state.artifact.id"
            | "agent.final.status"
            | "final.status"
            | "agent.escalation.status"
            | "final.escalation.status"
            | "agent.outcome.claim.status"
            | "agent.outcome.claim.operation"
            | "agent.outcome.claim.call_id"
            | "final.outcome.claim.status"
            | "final.outcome.claim.operation"
            | "final.outcome.claim.call_id"
            | "agent.role"
            | "agent.policy.id"
            | "agent.policy.action"
            | "agent.policy.outcome"
            | "agent.policy.reason_code"
            | "tool.name"
            | "tool.call.id"
            | "tool_call_id"
            | "tool.status"
            | "tool.result.success"
            | "tool.timeout"
            | "tool.cancelled"
            | "tool.operation"
            | "tool.effect"
            | "tool.retry_safety"
            | "tool.requirement"
            | "tool.approval.required"
            | "tool.approval.outcome"
            | "tool.state.observation"
            | "tool.state.predicate"
            | "tool.state.artifact.id"
            | "operation"
            | "operation.name"
            | "operation.effect"
            | "operation.retry_safety"
            | "operation.requirement"
            | "execution.status"
            | "execution.timeout"
            | "duration_ms"
            | "tool.duration_ms"
            | "gen_ai.tool.duration_ms"
            | "gen_ai.execute_tool.duration"
            | "tool.duration"
            | "execution.duration"
            | "error.type"
            | "error.code"
            | "error.retryable"
            | "tool.error.kind"
            | "tool.error.code"
            | "tool.error.retryable"
            | "exception.type"
            | "exception.escaped"
            | "exception.recorded"
            | "http.status_code"
            | "http.response.status_code"
            | "rpc.status_code"
            | "protocol.status_code"
            | "result.success"
            | "result.ok"
            | "policy.id"
            | "policy.version"
            | "policy.decision.id"
            | "policy.decision.outcome"
            | "policy.action"
            | "policy.outcome"
            | "policy.reason_code"
            | "guardrail.outcome"
            | "decision_id"
            | "reason_code"
    )
}

fn is_bounded_safe_scalar(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(value) => value.chars().count() <= 256,
        Value::Array(_) | Value::Object(_) => false,
    }
}

fn payload_fingerprint(value: &Value) -> Result<String, StoreError> {
    let canonical = serde_json::to_vec(&canonicalize_json(value))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(canonical))))
}

fn event_evidence_identity(
    trace_id: &str,
    revision: u64,
    span_id: &str,
    span_version: i64,
    index: usize,
) -> String {
    immutable_evidence_identity("event", trace_id, revision, span_id, span_version, index)
}

fn link_evidence_identity(
    trace_id: &str,
    revision: u64,
    span_id: &str,
    span_version: i64,
    index: usize,
) -> String {
    immutable_evidence_identity("link", trace_id, revision, span_id, span_version, index)
}

fn immutable_evidence_identity(
    kind: &str,
    trace_id: &str,
    revision: u64,
    span_id: &str,
    span_version: i64,
    index: usize,
) -> String {
    let material = format!("{kind}\0{trace_id}\0{revision}\0{span_id}\0{span_version}\0{index}");
    format!("{kind}:sha256:{}", hex::encode(Sha256::digest(material)))
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(values) => {
            let ordered = values
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(ordered.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        value => value.clone(),
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

fn elapsed_nano(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod topology_tests {
    use super::{WorkspaceStore, topology::topology_layout};
    use crate::layout::WorkspaceStoreLayout;

    #[test]
    fn lays_out_a_twenty_thousand_span_chain_without_rewalking_ancestors() {
        let topology = (0..20_000)
            .map(|index| {
                (
                    format!("span-{index}"),
                    (index > 0).then(|| format!("span-{}", index - 1)),
                )
            })
            .collect::<Vec<_>>();

        let (depths, parents_with_children) = topology_layout(&topology);

        assert_eq!(depths["span-0"], 0);
        assert_eq!(depths["span-19999"], 19_999);
        assert!(parents_with_children.contains("span-0"));
        assert!(!parents_with_children.contains("span-19999"));
    }

    #[test]
    fn treats_missing_parents_and_cycles_as_roots() {
        let topology = vec![
            ("orphan".into(), Some("missing".into())),
            ("a".into(), Some("b".into())),
            ("b".into(), Some("a".into())),
        ];

        let (depths, _) = topology_layout(&topology);

        assert_eq!(depths["orphan"], 0);
        assert_eq!(depths["a"], 0);
        assert_eq!(depths["b"], 0);
    }

    #[test]
    fn bounded_analytics_read_pool_never_borrows_the_writer_connection() {
        let directory = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "workspace")
            .unwrap();

        assert_eq!(store.analytics_reads.connections.len(), 2);
        let reader = store.analytics_reads.connection();
        let writer = store
            .analytics
            .try_lock()
            .expect("a reader must not hold the sole writer connection");
        let span_count = reader
            .query_row("SELECT COUNT(*) FROM spans", [], |row| row.get::<_, i64>(0))
            .unwrap();
        assert_eq!(span_count, 0);
        writer
            .execute_batch("CREATE TEMP TABLE writer_remains_available(value INTEGER);")
            .unwrap();
    }
}

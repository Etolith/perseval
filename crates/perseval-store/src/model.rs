use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use traces_to_evals::{
    AgentBehaviorTrace, BehaviorFinding, EvalCandidate, EvidencePacket, FindingPresentationV1,
    FindingSeverity, KNOWN_SIGNATURE_GROUP_SCHEMA_VERSION, RecoveryStatus,
};

pub const SPAN_UPSERT_SCHEMA_VERSION: &str = "perseval.span_upsert.v1";
pub const DEFAULT_INLINE_ATTRIBUTE_BYTES: usize = 4 * 1024;
pub const TRACE_DELTA_SCHEMA_VERSION: &str = "perseval.trace_delta.v1";
pub const ANALYSIS_RESULT_SCHEMA_VERSION: &str = "perseval.analysis_result.v2";
pub const ANALYSIS_IDENTITY_SCHEMA_VERSION: &str = "perseval.analysis_identity.v2";
pub const ANALYSIS_DEFINITION_SCHEMA_VERSION: &str = "perseval.analysis_definition.v2";
pub const DEFAULT_ANALYSIS_GROUPING_VERSION: &str = KNOWN_SIGNATURE_GROUP_SCHEMA_VERSION;
pub const DEFAULT_ANALYSIS_RISK_MODEL_VERSION: &str = "perseval.risk_model.none.v1";
pub const ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION: &str = "perseval.active_failure_projection.v3";
pub const PAYLOAD_IDENTITY_SCHEMA_VERSION: &str = "perseval.payload_identity.v1";
pub const PROJECT_SCHEMA_VERSION: &str = "perseval.project.v1";
pub const UNASSIGNED_PROJECT_ID: &str = "unassigned";
pub const EVAL_BATCH_PREVIEW_SCHEMA_VERSION: &str = "perseval.eval_batch_preview.v1";
pub const CANDIDATE_GENERATION_JOB_SCHEMA_VERSION: &str = "perseval.candidate_generation_job.v1";
pub const RUN_COMPARISON_REQUEST_SCHEMA_VERSION: &str = "perseval.run_comparison_request.v1";
pub const EVAL_CANDIDATE_RECORD_SCHEMA_VERSION: &str = "perseval.eval_candidate_record.v1";
pub const PIPELINE_DIAGNOSTICS_SCHEMA_VERSION: &str = "perseval.pipeline_diagnostics.v2";
pub const QUERY_SCOPE_SCHEMA_VERSION: &str = "perseval.query_scope.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStageV1 {
    Decode,
    JournalBuild,
    PayloadBlobDurability,
    RawBlobDurability,
    NormalizedBlobDurability,
    JournalCommit,
    DurableAcknowledgement,
    ProjectionDeserialization,
    Projection,
    Topology,
    AnalysisProjection,
    Normalization,
    Detection,
    AnalysisCommit,
    CohortProjection,
    CohortEmbedding,
    CohortFit,
    CohortAssignment,
    CohortCommit,
}

impl PipelineStageV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::JournalBuild => "journal_build",
            Self::PayloadBlobDurability => "payload_blob_durability",
            Self::RawBlobDurability => "raw_blob_durability",
            Self::NormalizedBlobDurability => "normalized_blob_durability",
            Self::JournalCommit => "journal_commit",
            Self::DurableAcknowledgement => "durable_acknowledgement",
            Self::ProjectionDeserialization => "projection_deserialization",
            Self::Projection => "projection",
            Self::Topology => "topology",
            Self::AnalysisProjection => "analysis_projection",
            Self::Normalization => "normalization",
            Self::Detection => "detection",
            Self::AnalysisCommit => "analysis_commit",
            Self::CohortProjection => "cohort_projection",
            Self::CohortEmbedding => "cohort_embedding",
            Self::CohortFit => "cohort_fit",
            Self::CohortAssignment => "cohort_assignment",
            Self::CohortCommit => "cohort_commit",
        }
    }
}

impl std::str::FromStr for PipelineStageV1 {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "decode" => Ok(Self::Decode),
            "journal_build" => Ok(Self::JournalBuild),
            "payload_blob_durability" => Ok(Self::PayloadBlobDurability),
            "raw_blob_durability" => Ok(Self::RawBlobDurability),
            "normalized_blob_durability" => Ok(Self::NormalizedBlobDurability),
            "journal_commit" => Ok(Self::JournalCommit),
            "durable_acknowledgement" => Ok(Self::DurableAcknowledgement),
            "projection_deserialization" => Ok(Self::ProjectionDeserialization),
            "projection" => Ok(Self::Projection),
            "topology" => Ok(Self::Topology),
            "analysis_projection" => Ok(Self::AnalysisProjection),
            "normalization" => Ok(Self::Normalization),
            "detection" => Ok(Self::Detection),
            "analysis_commit" => Ok(Self::AnalysisCommit),
            "cohort_projection" => Ok(Self::CohortProjection),
            "cohort_embedding" => Ok(Self::CohortEmbedding),
            "cohort_fit" => Ok(Self::CohortFit),
            "cohort_assignment" => Ok(Self::CohortAssignment),
            "cohort_commit" => Ok(Self::CohortCommit),
            _ => Err(format!("unknown pipeline stage {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineStageSampleV1 {
    pub stage: PipelineStageV1,
    pub duration_nano: u64,
    pub item_count: u64,
    pub byte_count: u64,
    pub rows_scanned: u64,
    pub rows_deserialized: u64,
}

impl PipelineStageSampleV1 {
    pub const fn new(stage: PipelineStageV1, duration_nano: u64) -> Self {
        Self {
            stage,
            duration_nano,
            item_count: 0,
            byte_count: 0,
            rows_scanned: 0,
            rows_deserialized: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineStageAggregateV1 {
    pub stage: PipelineStageV1,
    pub sample_count: u64,
    pub total_duration_nano: u64,
    pub max_duration_nano: u64,
    pub item_count: u64,
    pub byte_count: u64,
    pub rows_scanned: u64,
    pub rows_deserialized: u64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineDiagnosticsV1 {
    pub schema_version: String,
    pub stages: Vec<PipelineStageAggregateV1>,
    pub journal_backlog_rows: u64,
    pub journal_backlog_oldest_age_ms: u64,
    pub analysis_backlog_rows: u64,
    pub analysis_backlog_oldest_age_ms: u64,
    #[serde(alias = "semantic_models_built")]
    pub feature_similarity_models_built: u64,
    #[serde(alias = "semantic_assignments_written")]
    pub feature_similarity_assignments_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectV1 {
    pub schema_version: String,
    pub workspace_id: String,
    pub project_id: String,
    pub display_name: String,
    pub artifact_namespace: String,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectV1 {
    pub project_id: String,
    pub display_name: String,
    pub artifact_namespace: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceLifecycle {
    Live,
    Quiescent,
    Finalized,
    Reopened,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    #[default]
    NotReady,
    Pending,
    Analyzing,
    Ready,
    Reanalyzing,
    Failed,
}

impl AnalysisStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::Pending => "pending",
            Self::Analyzing => "analyzing",
            Self::Ready => "ready",
            Self::Reanalyzing => "reanalyzing",
            Self::Failed => "failed",
        }
    }
}

impl std::str::FromStr for AnalysisStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "not_ready" => Ok(Self::NotReady),
            "pending" => Ok(Self::Pending),
            "analyzing" => Ok(Self::Analyzing),
            "ready" => Ok(Self::Ready),
            "reanalyzing" => Ok(Self::Reanalyzing),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("unknown analysis status {value}")),
        }
    }
}

impl TraceLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Quiescent => "quiescent",
            Self::Finalized => "finalized",
            Self::Reopened => "reopened",
        }
    }
}

impl std::str::FromStr for TraceLifecycle {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "live" => Ok(Self::Live),
            "quiescent" => Ok(Self::Quiescent),
            "finalized" => Ok(Self::Finalized),
            "reopened" => Ok(Self::Reopened),
            _ => Err(format!("unknown trace lifecycle {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanEventV1 {
    pub name: String,
    pub timestamp_unix_nano: u64,
    pub attributes: BTreeMap<String, Value>,
    pub dropped_attributes_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanLinkV1 {
    pub trace_id: String,
    pub span_id: String,
    pub trace_state: String,
    pub attributes: BTreeMap<String, Value>,
    pub dropped_attributes_count: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRefV1 {
    pub sha256: String,
    pub original_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadIdentityQualityV1 {
    Explicit,
    Derived,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadIdentityV1 {
    pub schema_version: String,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobRefV1>,
    pub original_bytes: u64,
    #[serde(default)]
    pub quality: PayloadIdentityQualityV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanUpsertV1 {
    pub schema_version: String,
    pub source_id: String,
    pub external_trace_id: String,
    pub external_span_id: String,
    pub external_parent_span_id: Option<String>,
    pub logical_trace_id: String,
    pub content_hash: String,
    pub observed_at_unix_nano: u64,
    pub name: String,
    pub category: String,
    pub span_kind: i32,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub status_code: i32,
    pub status_message: String,
    pub trace_state: String,
    pub flags: u32,
    pub dropped_attributes_count: u32,
    pub dropped_events_count: u32,
    pub dropped_links_count: u32,
    pub resource: BTreeMap<String, Value>,
    pub scope: BTreeMap<String, Value>,
    pub attributes: BTreeMap<String, Value>,
    pub payload_refs: BTreeMap<String, BlobRefV1>,
    #[serde(default)]
    pub payload_identities: BTreeMap<String, PayloadIdentityV1>,
    pub events: Vec<SpanEventV1>,
    pub links: Vec<SpanLinkV1>,
    pub decoder_version: String,
    pub semantic_mapping_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanUpsertBatchV1 {
    pub schema_version: String,
    pub source_id: String,
    pub received_at_unix_ms: i64,
    pub spans: Vec<SpanUpsertV1>,
    pub rejected_spans: u64,
    pub rejection_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceChangeKind {
    Upserted,
    Quiescent,
    Finalized,
    Reopened,
    Analyzing,
    Reanalyzing,
    TopologyCommitted,
    FindingsCommitted,
    AnalysisFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    #[serde(default = "unassigned_project_id")]
    pub project_id: String,
    pub logical_trace_id: String,
    pub external_trace_id: String,
    pub revision: u64,
    pub lifecycle: TraceLifecycle,
    pub title: String,
    pub service_name: Option<String>,
    pub environment: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub identity_quality: IdentityQualityV1,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub last_committed_unix_ms: i64,
    pub span_count: u64,
    pub error_count: u64,
    pub analysis_status: AnalysisStatus,
    pub finding_count: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityQualityV1 {
    Explicit,
    Inferred,
    #[default]
    Unknown,
}

impl IdentityQualityV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QueryScopeCriteriaV1 {
    pub project_id: Option<String>,
    pub environment: Option<String>,
    pub build_id: Option<String>,
    pub session_id: Option<String>,
    pub service_name: Option<String>,
    pub started_after_unix_nano: Option<u64>,
    pub started_before_unix_nano: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryScopeV1 {
    pub schema_version: String,
    pub scope_id: String,
    pub criteria: QueryScopeCriteriaV1,
}

impl Default for QueryScopeV1 {
    fn default() -> Self {
        Self::new(QueryScopeCriteriaV1::default())
    }
}

impl QueryScopeV1 {
    pub fn new(criteria: QueryScopeCriteriaV1) -> Self {
        let scope_id = query_scope_id(&criteria);
        Self {
            schema_version: QUERY_SCOPE_SCHEMA_VERSION.into(),
            scope_id,
            criteria,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != QUERY_SCOPE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported query scope schema {}",
                self.schema_version
            ));
        }
        if self.scope_id != query_scope_id(&self.criteria) {
            return Err("query scope identity does not match its criteria".into());
        }
        if self
            .criteria
            .started_after_unix_nano
            .zip(self.criteria.started_before_unix_nano)
            .is_some_and(|(after, before)| after > before)
        {
            return Err("query scope time bounds are reversed".into());
        }
        Ok(())
    }
}

fn query_scope_id(criteria: &QueryScopeCriteriaV1) -> String {
    let encoded = serde_json::to_vec(criteria).expect("query scope criteria are serializable");
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(encoded)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RunFiltersV1 {
    #[serde(default)]
    pub scope: QueryScopeV1,
    pub lifecycle: Option<TraceLifecycle>,
    pub identity_quality: Option<IdentityQualityV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunComparisonRequestV1 {
    pub schema_version: String,
    pub scope: QueryScopeV1,
    pub baseline_trace_id: String,
    pub baseline_revision: u64,
    pub candidate_trace_id: String,
    pub candidate_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceDeltaV1 {
    pub schema_version: String,
    pub workspace_id: String,
    pub commit_sequence: u64,
    pub logical_trace_id: String,
    pub revision: u64,
    pub change: TraceChangeKind,
    pub changed_span_ids: Vec<String>,
    pub summary: RunSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanRow {
    pub logical_trace_id: String,
    pub revision: u64,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub category: String,
    pub start_time_unix_nano: u64,
    pub duration_nano: u64,
    pub status_code: i32,
    pub status_message: String,
    pub depth: u32,
    pub has_children: bool,
    pub attributes: BTreeMap<String, Value>,
    pub payload_refs: BTreeMap<String, BlobRefV1>,
    #[serde(default)]
    pub events: Vec<SpanEventV1>,
    #[serde(default)]
    pub links: Vec<SpanLinkV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanTreePageV1 {
    pub parent_span_id: Option<String>,
    pub offset: u64,
    pub total: u64,
    pub rows: Vec<SpanRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FailureFiltersV1 {
    pub scope: QueryScopeV1,
    pub severity: Option<FindingSeverity>,
    pub recovery: Option<RecoveryStatus>,
    pub detector_id: Option<String>,
    pub search: Option<String>,
    /// Default false keeps fully dismissed groups out of the actionable Inbox.
    pub include_fully_dismissed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingDispositionStateV1 {
    Confirmed,
    Dismissed,
    NeedsContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingDispositionV1 {
    pub project_id: String,
    pub group_id: String,
    pub finding_id: String,
    pub analysis_id: String,
    pub detector_id: String,
    pub detector_version: String,
    pub state: FindingDispositionStateV1,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureGroupSummary {
    pub scope: QueryScopeV1,
    pub project_id: String,
    pub group_id: String,
    pub failure_signature: String,
    pub detector_ids: Vec<String>,
    pub subject: Option<String>,
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<FindingPresentationV1>,
    pub severity: FindingSeverity,
    pub occurrence_count: u64,
    pub recovered_count: u64,
    pub unrecovered_count: u64,
    pub unknown_recovery_count: u64,
    pub affected_run_count: u64,
    #[serde(default)]
    pub affected_build_count: u64,
    #[serde(default)]
    pub affected_environment_count: u64,
    pub confirmed_count: u64,
    pub dismissed_count: u64,
    pub needs_context_count: u64,
    pub unreviewed_count: u64,
    pub stale_disposition_count: u64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    /// Legacy chronological finding counts. New clients should chart
    /// denominator-backed `recurrence` rates instead of these raw counts.
    #[serde(default)]
    pub occurrence_trend: Vec<u64>,
    /// Scope-wide, denominator-backed recurrence buckets. Every group returned
    /// by one query uses the same interval and bucket boundaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<FailureRecurrenceSeriesV1>,
    pub telemetry_gap_count: u64,
    pub reanalyzing: bool,
    #[serde(default)]
    #[serde(alias = "semantic_cohorts")]
    pub feature_similarity_cohorts: Vec<FeatureSimilarityCohortSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecurrenceBucketV1 {
    pub started_at_unix_nano: u64,
    pub ended_at_unix_nano: u64,
    pub eligible_run_count: u64,
    pub affected_run_count: u64,
    pub finding_count: u64,
    /// Recurrence rate in basis points (10_000 = 100%). `None` means there was
    /// no eligible denominator for this bucket.
    pub recurrence_rate_basis_points: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecurrenceSeriesV1 {
    pub started_at_unix_nano: u64,
    pub ended_at_unix_nano: u64,
    pub bucket_width_nano: u64,
    pub buckets: Vec<FailureRecurrenceBucketV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureGroupPageV1 {
    pub offset: u64,
    pub total: u64,
    pub rows: Vec<FailureGroupSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureSimilarityCohortSummary {
    pub model_id: String,
    pub cluster_id: String,
    pub member_count: u64,
    pub mean_confidence: f32,
    pub novelty_count: u64,
    pub method: String,
    pub embedding_provider: Option<String>,
    pub embedding_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureGroupDetail {
    pub summary: FailureGroupSummary,
    pub explanation: String,
    pub detector_versions: Vec<String>,
    pub adapter_versions: Vec<String>,
    pub telemetry_gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureOccurrence {
    pub scope: QueryScopeV1,
    pub project_id: String,
    pub group_id: String,
    pub logical_trace_id: String,
    pub revision: u64,
    pub run_title: String,
    pub service_name: Option<String>,
    pub analysis_status: AnalysisStatus,
    pub finding: BehaviorFinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<FindingDispositionV1>,
    #[serde(default)]
    pub disposition_stale: bool,
    pub telemetry_gaps: Vec<String>,
}

fn unassigned_project_id() -> String {
    UNASSIGNED_PROJECT_ID.into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingEvidence {
    pub occurrence: FailureOccurrence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<FindingPresentationV1>,
    pub spans: Vec<SpanRow>,
    pub evidence_span_ids: Vec<String>,
    pub final_outcome: Value,
    pub candidate: Option<EvalCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCandidatePreview {
    pub evidence_packet: EvidencePacket,
    pub candidate: EvalCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalEvidenceSelectionPolicyV1 {
    pub maximum_examples_per_group: u32,
    pub include_newest_unrecovered: bool,
    pub include_recovered: bool,
    pub include_distinct_builds: bool,
    pub include_distinct_shapes: bool,
}

impl Default for EvalEvidenceSelectionPolicyV1 {
    fn default() -> Self {
        Self {
            maximum_examples_per_group: 4,
            include_newest_unrecovered: true,
            include_recovered: true,
            include_distinct_builds: true,
            include_distinct_shapes: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalBatchSelectionSpecV1 {
    pub scope: QueryScopeV1,
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub policy: EvalEvidenceSelectionPolicyV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalSelectionReasonV1 {
    CanonicalMember,
    NewestUnrecovered,
    RecoveredExample,
    DistinctAgentBuild,
    DistinctExecutionShape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalBatchItemPreviewV1 {
    pub project_id: String,
    pub group_id: String,
    pub finding_id: String,
    pub logical_trace_id: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryStatus>,
    pub selection_reason: EvalSelectionReasonV1,
    pub telemetry_gaps: Vec<String>,
    pub already_exists: bool,
    pub candidate: EvalCandidate,
    pub evidence_packet: EvidencePacket,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalBatchExclusionV1 {
    pub group_id: String,
    pub finding_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalBatchPreviewV1 {
    pub schema_version: String,
    pub preview_id: String,
    pub project_id: String,
    pub selection_hash: String,
    pub selection_spec: EvalBatchSelectionSpecV1,
    pub maximum_candidate_count: u32,
    pub items: Vec<EvalBatchItemPreviewV1>,
    pub exclusions: Vec<EvalBatchExclusionV1>,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateGenerationOutcomeKindV1 {
    Created,
    AlreadyExists,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateGenerationItemOutcomeV1 {
    pub project_id: String,
    pub group_id: String,
    pub finding_id: String,
    pub candidate_id: Option<String>,
    pub outcome: CandidateGenerationOutcomeKindV1,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateGenerationJobStatusV1 {
    Queued,
    Running,
    Succeeded,
    PartialSuccess,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateGenerationJobV1 {
    pub schema_version: String,
    pub job_id: String,
    pub project_id: String,
    pub preview_id: String,
    pub selection_hash: String,
    pub idempotency_key: String,
    pub status: CandidateGenerationJobStatusV1,
    pub outcomes: Vec<CandidateGenerationItemOutcomeV1>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalReviewQueueStateV1 {
    Pending,
    Deferred,
    Accepted,
    Rejected,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCandidateRecordV1 {
    pub schema_version: String,
    pub project_id: String,
    pub group_id: String,
    pub finding_id: String,
    pub logical_trace_id: String,
    pub revision: u64,
    pub candidate: EvalCandidate,
    pub evidence_packet: EvidencePacket,
    pub queue_state: EvalReviewQueueStateV1,
    pub deferred_reason: Option<String>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalReviewDecisionV1 {
    Accept,
    Reject,
    Defer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvalCandidateV1 {
    pub project_id: String,
    pub candidate_id: String,
    pub decision: EvalReviewDecisionV1,
    pub reviewer_ref: String,
    pub reviewed_at: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisRequestV1 {
    pub logical_trace_id: String,
    pub revision: u64,
    pub reanalysis: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisResultV1 {
    pub schema_version: String,
    pub analysis_id: String,
    pub identity: AnalysisIdentityV1,
    pub logical_trace_id: String,
    pub revision: u64,
    pub adapter_id: String,
    pub adapter_version: String,
    pub behavior: AgentBehaviorTrace,
    pub detection_report: traces_to_evals::DetectionReportV1,
    pub findings: Vec<BehaviorFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisIdentityV1 {
    pub schema_version: String,
    pub logical_trace_id: String,
    pub revision: u64,
    pub input_schema_version: String,
    pub projection_version: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub detector_profile_id: String,
    pub detector_profile_version: String,
    pub detector_versions: BTreeMap<String, String>,
    #[serde(default = "default_analysis_grouping_version")]
    pub grouping_version: String,
    #[serde(default = "default_analysis_risk_model_version")]
    pub risk_model_version: String,
}

/// The trace-independent identity of the analysis implementation that should
/// own every active finalized revision. Changing any field makes an existing
/// active result stale without mutating or hiding that immutable result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDefinitionV1 {
    pub schema_version: String,
    pub input_schema_version: String,
    pub projection_version: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub detector_profile_id: String,
    pub detector_profile_version: String,
    pub detector_versions: BTreeMap<String, String>,
    #[serde(default = "default_analysis_grouping_version")]
    pub grouping_version: String,
    #[serde(default = "default_analysis_risk_model_version")]
    pub risk_model_version: String,
}

fn default_analysis_grouping_version() -> String {
    DEFAULT_ANALYSIS_GROUPING_VERSION.into()
}

fn default_analysis_risk_model_version() -> String {
    DEFAULT_ANALYSIS_RISK_MODEL_VERSION.into()
}

impl AnalysisDefinitionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ANALYSIS_DEFINITION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported analysis definition schema {}",
                self.schema_version
            ));
        }
        if [
            &self.input_schema_version,
            &self.projection_version,
            &self.adapter_id,
            &self.adapter_version,
            &self.detector_profile_id,
            &self.detector_profile_version,
            &self.grouping_version,
            &self.risk_model_version,
        ]
        .into_iter()
        .any(|value| value.trim().is_empty())
        {
            return Err("analysis definition identity fields must not be empty".into());
        }
        if self.detector_versions.is_empty()
            || self
                .detector_versions
                .iter()
                .any(|(id, version)| id.trim().is_empty() || version.trim().is_empty())
        {
            return Err("analysis definition requires named detector versions".into());
        }
        Ok(())
    }
}

impl AnalysisIdentityV1 {
    pub fn analysis_id(&self) -> String {
        let encoded = serde_json::to_vec(self).expect("analysis identity is serializable");
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(encoded)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SourceHealth {
    pub enabled: bool,
    pub effective_address: Option<String>,
    pub queue_batches: usize,
    pub queue_batch_capacity: usize,
    pub queue_bytes: usize,
    pub queue_byte_capacity: usize,
    pub accepted_spans: u64,
    pub rejected_spans: u64,
    pub journal_lag: u64,
    pub projection_lag: u64,
    pub projection_backlog_age_ms: u64,
    pub projection_degraded: bool,
    pub projection_retry_count: u64,
    pub projection_last_error: Option<String>,
    pub analysis_pending: u64,
    pub analysis_running: u64,
    pub cohort_assignment_pending: u64,
    pub cohort_rebuild_pending: u64,
    pub cohort_running: bool,
    pub topology_pending: u64,
    pub topology_running: u64,
    pub backpressured: bool,
    pub shutting_down: bool,
    pub last_error: Option<String>,
    #[serde(default)]
    pub openai: OpenAiProviderHealthV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OpenAiProviderHealthV1 {
    pub enabled: bool,
    pub configured: bool,
    pub running_jobs: usize,
    pub successful_jobs: u64,
    pub failed_jobs: u64,
    pub degraded: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyProjectionJobV1 {
    pub logical_trace_id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyProjectionRowV1 {
    pub span_id: String,
    pub order: u64,
    pub depth: u32,
    pub has_children: bool,
}

#[cfg(test)]
mod tests;

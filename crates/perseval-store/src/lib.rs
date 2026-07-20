#![forbid(unsafe_code)]

//! Persistence boundaries for Perseval.
//!
//! Transactional control state and analytical trace scans are deliberately
//! separate so SQLite can own the MVP control plane while DuckDB/Parquet can be
//! introduced behind the analytical boundary after benchmarks justify it.

pub mod analytics;
pub mod blobs;
pub mod control;
pub mod layout;
pub mod model;
pub mod workspace;

pub use analytics::{AnalyticsBackend, AnalyticsStore};
pub use blobs::{BlobStore, FsBlobStore};
pub use control::ControlStore;
pub use layout::WorkspaceStoreLayout;
pub use model::{
    ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION, ADJUDICATION_SCHEMA_VERSION,
    AGENT_CONTEXT_DRAFT_SCHEMA_VERSION, ANALYSIS_DEFINITION_SCHEMA_VERSION,
    ANALYSIS_IDENTITY_SCHEMA_VERSION, ANALYSIS_RESULT_SCHEMA_VERSION,
    ANNOTATION_CASE_SCHEMA_VERSION, ANNOTATION_REVISION_SCHEMA_VERSION,
    ANNOTATION_SCHEMA_RELEASE_SCHEMA_VERSION, ASSESSMENT_BACKFILL_PREVIEW_SCHEMA_VERSION,
    ASSESSMENT_DECISION_SCHEMA_VERSION, ASSESSMENT_JOB_EXPORT_SCHEMA_VERSION,
    ASSESSMENT_JOB_SCHEMA_VERSION, ASSESSMENT_RECORD_SCHEMA_VERSION,
    ASSESSMENT_SAMPLING_POLICY_SCHEMA_VERSION, AdjudicationV1, AgentContextDraftV1,
    AgentContextGovernanceSummaryV1, AnalysisDefinitionV1, AnalysisIdentityV1, AnalysisRequestV1,
    AnalysisResultV1, AnalysisStatus, AnnotationCaseV1, AnnotationLabelV1, AnnotationRevisionV1,
    AnnotationSchemaReleaseV1, AssessmentBackfillPreviewV1, AssessmentCommitV1,
    AssessmentDecisionV1, AssessmentItemStatusV1, AssessmentJobExportV1, AssessmentJobItemExportV1,
    AssessmentJobStatusV1, AssessmentJobV1, AssessmentPresentationV1, AssessmentPreviewTargetV1,
    AssessmentRecordV1, AssessmentRuntimeHealthV1, AssessmentSamplingPolicyV1,
    BlindReviewTaskViewV1, BlobRefV1, CALIBRATION_RELEASE_SCHEMA_VERSION,
    CALIBRATION_REPORT_SCHEMA_VERSION, CANDIDATE_GENERATION_JOB_SCHEMA_VERSION,
    CalibratedDecisionV1, CalibrationMemberV1, CalibrationReleaseV1, CalibrationReportV1,
    CalibrationSliceReportV1, CandidateGenerationItemOutcomeV1, CandidateGenerationJobStatusV1,
    CandidateGenerationJobV1, CandidateGenerationOutcomeKindV1, ClaimedAssessmentItemV1,
    ContextBackfillPreviewV1, ContextBackfillResultV1, ContextBindingRecordV1,
    ContextBindingRuleSetV1, ContextBindingSelectorV1, ContextBindingStatusV1, CreateProjectV1,
    DEFAULT_ANALYSIS_GROUPING_VERSION, DEFAULT_ANALYSIS_RISK_MODEL_VERSION,
    DEFAULT_INLINE_ATTRIBUTE_BYTES, EVAL_BATCH_PREVIEW_SCHEMA_VERSION,
    EVAL_CANDIDATE_RECORD_SCHEMA_VERSION, EvalBatchExclusionV1, EvalBatchItemPreviewV1,
    EvalBatchPreviewV1, EvalBatchSelectionSpecV1, EvalCandidatePreview, EvalCandidateRecordV1,
    EvalEvidenceSelectionPolicyV1, EvalReviewDecisionV1, EvalReviewQueueStateV1,
    EvalSelectionReasonV1, FailureFiltersV1, FailureGroupDetail, FailureGroupPageV1,
    FailureGroupSummary, FailureOccurrence, FailureRecurrenceBucketV1, FailureRecurrenceSeriesV1,
    FeatureSimilarityCohortSummary, FindingDispositionStateV1, FindingDispositionV1,
    FindingEvidence, IdentityQualityV1, OpenAiProviderHealthV1, PAYLOAD_IDENTITY_SCHEMA_VERSION,
    PIPELINE_DIAGNOSTICS_SCHEMA_VERSION, PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION,
    PROJECT_SCHEMA_VERSION, PayloadIdentityQualityV1, PayloadIdentityV1, PipelineDiagnosticsV1,
    PipelineStageAggregateV1, PipelineStageSampleV1, PipelineStageV1, ProjectAssessmentPolicyV1,
    ProjectV1, QUERY_SCOPE_SCHEMA_VERSION, QueryScopeCriteriaV1, QueryScopeV1,
    REVIEW_QUEUE_SCHEMA_VERSION, REVIEW_SPLIT_RELEASE_SCHEMA_VERSION, REVIEW_TASK_SCHEMA_VERSION,
    RUN_COMPARISON_REQUEST_SCHEMA_VERSION, RevealedReviewTaskViewV1, ReviewAdjudicationPacketV1,
    ReviewAssignmentV1, ReviewAuthorityV1, ReviewEvalCandidateV1, ReviewModeV1, ReviewQueueV1,
    ReviewSelectionReasonV1, ReviewSplitReleaseV1, ReviewTaskPresentationV1, ReviewTaskStatusV1,
    ReviewTaskV1, RunComparisonRequestV1, RunFiltersV1, RunOrderV1, RunSummary,
    SPAN_UPSERT_SCHEMA_VERSION, SourceHealth, SpanEventV1, SpanLinkV1, SpanRow, SpanTreePageV1,
    SpanUpsertBatchV1, SpanUpsertV1, TASK_COMPLETION_RELEASE_CONFIG_SCHEMA_VERSION,
    THRESHOLD_POLICY_ACTIVATION_SCHEMA_VERSION, THRESHOLD_POLICY_RELEASE_SCHEMA_VERSION,
    TRACE_DELTA_SCHEMA_VERSION, TaskCompletionQualityCheckV1, TaskCompletionReleaseConfigV1,
    TaxonomyChangeDraftRecordV1, TaxonomyGovernanceSummaryV1, ThresholdPolicyActivationV1,
    ThresholdPolicyReleaseV1, TopologyProjectionJobV1, TopologyProjectionRowV1, TraceChangeKind,
    TraceDeltaV1, TraceLifecycle, UNASSIGNED_PROJECT_ID,
};
pub use workspace::{JournalReceipt, StoreError, WorkspaceStore};

pub use traces_to_evals as analysis;
pub use traces_to_evals::{
    AlignedExecutionRow, AlignmentRelation, DivergenceSummary, ExecutionStep, TraceComparison,
};

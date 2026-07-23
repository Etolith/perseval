#![forbid(unsafe_code)]

//! Use-case boundary shared by the native application and MCP server.

#[path = "analysis.rs"]
mod analyzer;
pub mod assessments;
pub mod commands;
pub mod config;
mod demo_trace;
pub mod jobs;
pub mod live;
pub mod model_management;
pub mod queries;
pub mod runtime;
mod supervision;
mod topology;

pub use assessments::LocalTaskCompletionModelV1;
pub use config::{
    AssessmentConfig, ConfigError, McpConfig, OpenAiAnalysisConfig, PersevalConfigV1,
};
pub use live::{
    BlobPreviewV1, ComparisonCancellationToken, LiveServiceError, LiveTraceService,
    SubscriptionError, TaskCompletionExecutionRouteV1, TaskCompletionQualityCheckDraftV1,
    TraceFileImportResultV1, TraceSnapshot, TraceSubscription,
};
pub use model_management::{
    ManagedTaskCompletionModelV1, ModelDownloadFileV1, ModelManagementError,
    TASK_COMPLETION_MODEL_CATALOG_SCHEMA_VERSION, TASK_COMPLETION_MODEL_CATALOG_URL,
    TaskCompletionModelCatalogV1, TaskCompletionModelManager, inspect_managed_model,
    managed_model_root,
};
pub use perseval_store::{
    ASSESSMENT_SAMPLING_POLICY_SCHEMA_VERSION, AdjudicationV1, AgentContextDraftV1,
    AgentContextGovernanceSummaryV1, AnalysisStatus, AnnotationCaseV1, AnnotationLabelV1,
    AnnotationRevisionV1, AnnotationSchemaReleaseV1, AssessmentBackfillPreviewV1,
    AssessmentCommitV1, AssessmentDecisionV1, AssessmentItemStatusV1, AssessmentJobExportV1,
    AssessmentJobItemExportV1, AssessmentJobStatusV1, AssessmentJobV1, AssessmentPresentationV1,
    AssessmentPreviewTargetV1, AssessmentRecordV1, AssessmentRuntimeHealthV1,
    AssessmentSamplingPolicyV1, BlindReviewTaskViewV1, BlobRefV1, CalibratedDecisionV1,
    CalibrationMemberV1, CalibrationReleaseV1, CalibrationReportV1, CalibrationSliceReportV1,
    CandidateGenerationItemOutcomeV1, CandidateGenerationJobStatusV1, CandidateGenerationJobV1,
    CandidateGenerationOutcomeKindV1, ContextBackfillPreviewV1, ContextBackfillResultV1,
    ContextBindingRecordV1, ContextBindingRuleSetV1, ContextBindingSelectorV1,
    ContextBindingStatusV1, CreateProjectV1, EvalBatchExclusionV1, EvalBatchItemPreviewV1,
    EvalBatchPreviewV1, EvalBatchSelectionSpecV1, EvalCandidatePreview, EvalCandidateRecordV1,
    EvalEvidenceSelectionPolicyV1, EvalReviewDecisionV1, EvalReviewQueueStateV1,
    EvalSelectionReasonV1, FailureFiltersV1, FailureGroupDetail, FailureGroupPageV1,
    FailureGroupSummary, FailureOccurrence, FailureRecurrenceBucketV1, FailureRecurrenceSeriesV1,
    FeatureSimilarityCohortSummary, FindingDispositionStateV1, FindingDispositionV1,
    FindingEvidence, IdentityQualityV1, OpenAiProviderHealthV1, ProjectAssessmentPolicyV1,
    ProjectV1, QueryScopeCriteriaV1, QueryScopeV1, RUN_COMPARISON_REQUEST_SCHEMA_VERSION,
    RevealedReviewTaskViewV1, ReviewAdjudicationPacketV1, ReviewAssignmentV1, ReviewAuthorityV1,
    ReviewEvalCandidateV1, ReviewModeV1, ReviewQueueV1, ReviewSelectionReasonV1,
    ReviewSplitReleaseV1, ReviewTaskPresentationV1, ReviewTaskStatusV1, ReviewTaskV1,
    RunComparisonRequestV1, RunFiltersV1, RunOrderV1, RunSummary, SourceHealth, SpanRow,
    SpanTreePageV1, TaskCompletionQualityCheckV1, TaskCompletionReleaseConfigV1,
    TaxonomyChangeDraftRecordV1, TaxonomyGovernanceSummaryV1, ThresholdPolicyActivationV1,
    ThresholdPolicyReleaseV1, TraceChangeKind, TraceDeltaV1, TraceLifecycle, UNASSIGNED_PROJECT_ID,
};
pub use queries::{SpanCategory, SpanView, TRACE_FILE_ENV, TraceCatalog, TraceView};
pub use runtime::{
    RuntimeCapabilities, RuntimeConfigurationError, RuntimeMode, RuntimeStartError, ServiceRuntime,
};
pub use traces_to_evals::{BinaryCalibrationReportV1, CalibrationDataSplitV1};

pub use perseval_ingest as ingest;
pub use perseval_store as store;
pub use traces_to_evals as analysis;
pub use traces_to_evals::{
    AlignedExecutionRow, AlignmentRelation, DivergenceSummary, ExecutionStep, FindingSeverity,
    RecoveryStatus, TraceComparison,
};

#![forbid(unsafe_code)]

//! Use-case boundary shared by the native application and MCP server.

#[path = "analysis.rs"]
mod analyzer;
pub mod commands;
pub mod config;
mod demo_trace;
pub mod jobs;
pub mod live;
pub mod queries;
pub mod runtime;
mod topology;

pub use config::{ConfigError, McpConfig, OpenAiAnalysisConfig, PersevalConfigV1};
pub use live::{
    BlobPreviewV1, ComparisonCancellationToken, LiveServiceError, LiveTraceService,
    SubscriptionError, TraceFileImportResultV1, TraceSnapshot, TraceSubscription,
};
pub use perseval_store::{
    AnalysisStatus, BlobRefV1, CandidateGenerationItemOutcomeV1, CandidateGenerationJobStatusV1,
    CandidateGenerationJobV1, CandidateGenerationOutcomeKindV1, CreateProjectV1,
    EvalBatchExclusionV1, EvalBatchItemPreviewV1, EvalBatchPreviewV1, EvalBatchSelectionSpecV1,
    EvalCandidatePreview, EvalCandidateRecordV1, EvalEvidenceSelectionPolicyV1,
    EvalReviewDecisionV1, EvalReviewQueueStateV1, EvalSelectionReasonV1, FailureFiltersV1,
    FailureGroupDetail, FailureGroupPageV1, FailureGroupSummary, FailureOccurrence,
    FailureRecurrenceBucketV1, FailureRecurrenceSeriesV1, FeatureSimilarityCohortSummary,
    FindingDispositionStateV1, FindingDispositionV1, FindingEvidence, IdentityQualityV1,
    OpenAiProviderHealthV1, ProjectV1, QueryScopeCriteriaV1, QueryScopeV1,
    RUN_COMPARISON_REQUEST_SCHEMA_VERSION, ReviewEvalCandidateV1, RunComparisonRequestV1,
    RunFiltersV1, RunOrderV1, RunSummary, SourceHealth, SpanRow, SpanTreePageV1, TraceChangeKind,
    TraceDeltaV1, TraceLifecycle, UNASSIGNED_PROJECT_ID,
};
pub use queries::{SpanCategory, SpanView, TRACE_FILE_ENV, TraceCatalog, TraceView};
pub use runtime::{
    RuntimeCapabilities, RuntimeConfigurationError, RuntimeMode, RuntimeStartError, ServiceRuntime,
};

pub use perseval_ingest as ingest;
pub use perseval_store as store;
pub use traces_to_evals as analysis;
pub use traces_to_evals::{
    AlignedExecutionRow, AlignmentRelation, DivergenceSummary, ExecutionStep, FindingSeverity,
    RecoveryStatus, TraceComparison,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gpui::{Entity, FocusHandle, UniformListScrollHandle};
use perseval_service::{
    AssessmentDecisionV1, AssessmentRecordV1, BlobPreviewV1, CandidateGenerationJobV1,
    EvalBatchPreviewV1, EvalCandidatePreview, FailureFiltersV1, FailureGroupDetail,
    FailureGroupSummary, FailureOccurrence, FindingEvidence, LiveTraceService, SourceHealth,
    SpanRow,
};

use super::full_trace_timeline::FullTraceTimelineModel;
use super::full_trace_tree::FullTraceTreeModel;
use super::{FullTraceMode, InboxFilterMenu, InspectorTab};
use crate::components::TextInput;
use crate::workbench::{FailureInboxPreferencesV1, FullTraceOrigin, QueryScope};

/// Mutable data owned by the Failure Inbox screen. Keeping it separate from
/// the GPUI view object makes loading/selection/full-trace concerns explicit
/// and gives later reducers a stable state boundary without a flag-day rewrite.
pub(crate) struct FailureInboxState {
    pub(super) service: Arc<LiveTraceService>,
    pub(super) groups: Vec<FailureGroupSummary>,
    pub(super) group_total: u64,
    pub(super) groups_loading: bool,
    pub(super) groups_request_generation: u64,
    pub(super) search_request_generation: u64,
    pub(super) focused_group: Option<(String, String)>,
    pub(super) selected_group_ids: BTreeSet<String>,
    pub(super) selection_anchor: Option<(String, String)>,
    pub(super) showing_inbox: bool,
    pub(super) investigation_target: Option<(String, String)>,
    pub(super) selected_group: Option<FailureGroupDetail>,
    pub(super) occurrences: Vec<FailureOccurrence>,
    pub(super) selected_finding_id: Option<String>,
    pub(super) evidence: Option<FindingEvidence>,
    pub(super) investigation_loading: bool,
    pub(super) investigation_request_generation: u64,
    pub(super) evidence_loading: bool,
    pub(super) evidence_request_generation: u64,
    pub(super) pending_focus_span_id: Option<String>,
    pub(super) health: SourceHealth,
    pub(super) run_count: u64,
    pub(super) filters: FailureFiltersV1,
    pub(super) preferences: FailureInboxPreferencesV1,
    pub(super) query_scope: QueryScope,
    pub(super) detector_options: Vec<String>,
    pub(super) service_options: Vec<String>,
    pub(super) open_filter_menu: Option<InboxFilterMenu>,
    pub(super) search_input: Entity<TextInput>,
    pub(super) group_scroll: UniformListScrollHandle,
    pub(super) full_trace_search: Entity<TextInput>,
    pub(super) focus_handle: FocusHandle,
    pub(super) occurrence_offset: u64,
    pub(super) full_trace: bool,
    pub(super) full_trace_tree: FullTraceTreeModel,
    pub(super) full_trace_timeline: FullTraceTimelineModel,
    pub(super) full_trace_identity: Option<(String, u64)>,
    pub(super) full_trace_project_id: Option<String>,
    pub(super) trace_assessments: Vec<AssessmentRecordV1>,
    pub(super) trace_assessment_decisions: BTreeMap<String, Vec<AssessmentDecisionV1>>,
    pub(super) withheld_assessment_count: usize,
    pub(super) full_trace_origin: FullTraceOrigin,
    pub(super) full_trace_span_count: u64,
    pub(super) full_trace_loading: bool,
    pub(super) full_trace_mode: FullTraceMode,
    pub(super) full_trace_errors_only: bool,
    pub(super) full_trace_request_generation: u64,
    pub(super) full_trace_start_unix_nano: u64,
    pub(super) full_trace_end_unix_nano: u64,
    pub(super) focused_span_id: Option<String>,
    pub(super) focused_span_snapshot: Option<SpanRow>,
    pub(super) focused_review_evidence: Option<(String, String)>,
    pub(super) group_details_open: bool,
    pub(super) investigation_actions_open: bool,
    pub(super) finding_review_open: bool,
    pub(super) compare_base_finding_id: Option<String>,
    pub(super) inspector_open: bool,
    pub(super) inspector_auto_open_suppressed: bool,
    pub(super) inspector_width: f32,
    pub(super) text_scale: f32,
    pub(super) tab: InspectorTab,
    pub(super) candidate_preview: Option<EvalCandidatePreview>,
    pub(super) batch_preview: Option<EvalBatchPreviewV1>,
    pub(super) expanded_eval_group_ids: BTreeSet<String>,
    pub(super) generation_job: Option<CandidateGenerationJobV1>,
    pub(super) batch_loading: bool,
    pub(super) revealed_payload: Option<(String, String, BlobPreviewV1)>,
    pub(super) last_sequence: u64,
    pub(super) delayed: bool,
    pub(super) load_error: Option<String>,
}

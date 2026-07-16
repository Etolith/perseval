mod comparison;
mod components;
mod eval_batch_preview;
mod eval_generation;
mod evidence;
mod filter_menu;
mod full_trace_render;
mod full_trace_timeline;
mod full_trace_tree;
mod group_header;
mod investigation_state;
mod preferences;
mod recovery;
mod render;
mod render_root;
mod scope;
mod selection;

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyBinding, Render, Rgba, Role, ScrollStrategy, Toggled, UniformListScrollHandle, Window,
    actions, div, prelude::*, px, relative, uniform_list,
};
use perseval_service::analysis::{FindingSeverity, RecoveryStatus};
use perseval_service::{
    BlobPreviewV1, BlobRefV1, CandidateGenerationJobStatusV1, CandidateGenerationJobV1,
    EvalBatchPreviewV1, EvalBatchSelectionSpecV1, EvalCandidatePreview, FailureFiltersV1,
    FailureGroupDetail, FailureGroupSummary, FailureOccurrence, FailureRecurrenceBucketV1,
    FailureRecurrenceSeriesV1, FindingDispositionStateV1, FindingEvidence, LiveTraceService,
    QueryScopeCriteriaV1, QueryScopeV1, RunComparisonRequestV1, SourceHealth, SpanRow,
    TraceChangeKind, TraceSnapshot, TraceSubscription, UNASSIGNED_PROJECT_ID,
};

use crate::components::{
    DataColumn, TextInput, data_columns, data_page_header, data_page_toolbar, data_table_header,
};
use crate::design::{Breakpoint, ExecutionRole, Geometry, Theme};
use crate::workbench::{FailureInboxPreferencesV1, FailureInboxSort, FullTraceOrigin, QueryScope};

use full_trace_timeline::FullTraceTimelineModel;
use full_trace_tree::{FullTraceTreeModel, TreePageKey};
use selection::{occurrence_navigation_state, reconcile_group_identity_state};

actions!(
    perseval_failure_inbox,
    [
        FocusNextFailureGroup,
        FocusPreviousFailureGroup,
        ExtendNextFailureGroup,
        ExtendPreviousFailureGroup,
        OpenFocusedFailureGroup,
        ToggleFocusedFailureGroup,
    ]
);

pub(crate) fn init_key_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", FocusNextFailureGroup, Some("FailureInbox")),
        KeyBinding::new("up", FocusPreviousFailureGroup, Some("FailureInbox")),
        KeyBinding::new("shift-down", ExtendNextFailureGroup, Some("FailureInbox")),
        KeyBinding::new("shift-up", ExtendPreviousFailureGroup, Some("FailureInbox")),
        KeyBinding::new("enter", OpenFocusedFailureGroup, Some("FailureInbox")),
        KeyBinding::new("space", ToggleFocusedFailureGroup, Some("FailureInbox")),
    ]);
}

#[derive(Debug, Clone)]
pub(crate) enum FailureInboxEvent {
    OpenInvestigation {
        project_id: String,
        group_id: String,
    },
    OpenEvalQueue,
    OpenRuns,
    OpenSources,
    OpenCompare(RunComparisonRequestV1),
    OpenFullTrace {
        project_id: String,
        logical_trace_id: String,
        revision: u64,
        origin: FullTraceOrigin,
    },
    ReturnFromFullTrace {
        origin: FullTraceOrigin,
    },
    FullTraceSelectionChanged {
        span_id: String,
    },
    InspectorVisibilityChanged {
        visible: bool,
        auto_open_suppressed: bool,
    },
    PreferencesChanged {
        scope_key: String,
        preferences: FailureInboxPreferencesV1,
    },
    TraceDataChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Finding,
    Span,
    Attributes,
    Payload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullTraceMode {
    Tree,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboxFilterMenu {
    Filters,
    Organize,
}

pub(crate) struct FailureInbox {
    service: Arc<LiveTraceService>,
    groups: Vec<FailureGroupSummary>,
    group_total: u64,
    groups_loading: bool,
    groups_request_generation: u64,
    search_request_generation: u64,
    focused_group: Option<(String, String)>,
    selected_group_ids: BTreeSet<String>,
    selection_anchor: Option<(String, String)>,
    showing_inbox: bool,
    investigation_target: Option<(String, String)>,
    selected_group: Option<FailureGroupDetail>,
    occurrences: Vec<FailureOccurrence>,
    selected_finding_id: Option<String>,
    evidence: Option<FindingEvidence>,
    investigation_loading: bool,
    investigation_request_generation: u64,
    evidence_loading: bool,
    evidence_request_generation: u64,
    pending_focus_span_id: Option<String>,
    health: SourceHealth,
    run_count: u64,
    filters: FailureFiltersV1,
    preferences: FailureInboxPreferencesV1,
    query_scope: QueryScope,
    detector_options: Vec<String>,
    service_options: Vec<String>,
    open_filter_menu: Option<InboxFilterMenu>,
    search_input: Entity<TextInput>,
    group_scroll: UniformListScrollHandle,
    full_trace_search: Entity<TextInput>,
    focus_handle: FocusHandle,
    occurrence_offset: u64,
    full_trace: bool,
    full_trace_tree: FullTraceTreeModel,
    full_trace_timeline: FullTraceTimelineModel,
    full_trace_identity: Option<(String, u64)>,
    full_trace_origin: FullTraceOrigin,
    full_trace_span_count: u64,
    full_trace_loading: bool,
    full_trace_mode: FullTraceMode,
    full_trace_errors_only: bool,
    full_trace_request_generation: u64,
    full_trace_start_unix_nano: u64,
    full_trace_end_unix_nano: u64,
    focused_span_id: Option<String>,
    focused_span_snapshot: Option<SpanRow>,
    group_details_open: bool,
    investigation_actions_open: bool,
    finding_review_open: bool,
    compare_base_finding_id: Option<String>,
    inspector_open: bool,
    inspector_auto_open_suppressed: bool,
    inspector_width: f32,
    text_scale: f32,
    tab: InspectorTab,
    candidate_preview: Option<EvalCandidatePreview>,
    batch_preview: Option<EvalBatchPreviewV1>,
    expanded_eval_group_ids: BTreeSet<String>,
    generation_job: Option<CandidateGenerationJobV1>,
    batch_loading: bool,
    revealed_payload: Option<(String, String, BlobPreviewV1)>,
    last_sequence: u64,
    delayed: bool,
    load_error: Option<String>,
}

impl EventEmitter<FailureInboxEvent> for FailureInbox {}

impl FailureInbox {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        snapshot: TraceSnapshot,
        subscription: TraceSubscription,
        max_deltas_per_frame: usize,
        cached_pages: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let (detector_options, service_options, filter_options_error) =
            match service.failure_filter_options() {
                Ok((detectors, services)) => (detectors, services, None),
                Err(error) => (Vec::new(), Vec::new(), Some(error.to_string())),
            };
        let search_input = cx.new(|cx| TextInput::new("Search failures…", 512, cx));
        cx.observe(&search_input, |this, _, cx| {
            this.schedule_search(cx);
            cx.notify();
        })
        .detach();
        let full_trace_search = cx.new(|cx| TextInput::new("Search loaded spans…", 512, cx));
        cx.observe(&full_trace_search, |_, _, cx| cx.notify())
            .detach();
        let mut this = Self {
            service: service.clone(),
            groups: Vec::new(),
            group_total: 0,
            groups_loading: false,
            groups_request_generation: 0,
            search_request_generation: 0,
            focused_group: None,
            selected_group_ids: BTreeSet::new(),
            selection_anchor: None,
            showing_inbox: true,
            investigation_target: None,
            selected_group: None,
            occurrences: Vec::new(),
            selected_finding_id: None,
            evidence: None,
            investigation_loading: false,
            investigation_request_generation: 0,
            evidence_loading: false,
            evidence_request_generation: 0,
            pending_focus_span_id: None,
            health: snapshot.health,
            run_count: snapshot.total_runs,
            filters: FailureFiltersV1::default(),
            preferences: FailureInboxPreferencesV1::default(),
            query_scope: QueryScope::default(),
            detector_options,
            service_options,
            open_filter_menu: None,
            search_input,
            group_scroll: UniformListScrollHandle::new(),
            full_trace_search,
            focus_handle: cx.focus_handle(),
            occurrence_offset: 0,
            full_trace: false,
            full_trace_tree: FullTraceTreeModel::new(cached_pages),
            full_trace_timeline: FullTraceTimelineModel::new(cached_pages),
            full_trace_identity: None,
            full_trace_origin: FullTraceOrigin::Runs,
            full_trace_span_count: 0,
            full_trace_loading: false,
            full_trace_mode: FullTraceMode::Tree,
            full_trace_errors_only: false,
            full_trace_request_generation: 0,
            full_trace_start_unix_nano: 0,
            full_trace_end_unix_nano: 1,
            focused_span_id: None,
            focused_span_snapshot: None,
            group_details_open: false,
            investigation_actions_open: false,
            finding_review_open: false,
            compare_base_finding_id: None,
            inspector_open: false,
            inspector_auto_open_suppressed: false,
            inspector_width: 360.,
            text_scale: 1.,
            tab: InspectorTab::Finding,
            candidate_preview: None,
            batch_preview: None,
            expanded_eval_group_ids: BTreeSet::new(),
            generation_job: None,
            batch_loading: false,
            revealed_payload: None,
            last_sequence: snapshot.commit_sequence,
            delayed: false,
            load_error: None,
        };
        this.reload_groups(cx);
        if this.load_error.is_none() {
            this.load_error = filter_options_error;
        }

        cx.spawn(async move |weak, cx| {
            let mut subscription = subscription;
            loop {
                match subscription.recv_batch(max_deltas_per_frame).await {
                    Ok(deltas) => {
                        if weak
                            .update(cx, |this, cx| {
                                let trace_data_changed = !deltas.is_empty();
                                let refresh = deltas.iter().any(|delta| {
                                    if delta.commit_sequence != this.last_sequence.saturating_add(1)
                                        && delta.commit_sequence > this.last_sequence
                                    {
                                        this.delayed = true;
                                    }
                                    this.last_sequence =
                                        this.last_sequence.max(delta.commit_sequence);
                                    matches!(
                                        delta.change,
                                        TraceChangeKind::FindingsCommitted
                                            | TraceChangeKind::Reanalyzing
                                            | TraceChangeKind::AnalysisFailed
                                    )
                                });
                                if refresh {
                                    this.run_count =
                                        this.service.run_count().unwrap_or(this.run_count);
                                    this.reload_groups(cx);
                                }
                                if trace_data_changed {
                                    cx.emit(FailureInboxEvent::TraceDataChanged);
                                }
                                if let Ok(health) = this.service.source_health() {
                                    this.health = health;
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => match service.snapshot_and_subscribe() {
                        Ok((snapshot, next)) => {
                            if weak
                                .update(cx, |this, cx| {
                                    this.health = snapshot.health;
                                    this.run_count = snapshot.total_runs;
                                    this.last_sequence = snapshot.commit_sequence;
                                    this.delayed = false;
                                    this.reload_groups(cx);
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                            subscription = next;
                        }
                        Err(error) => {
                            let _ = weak.update(cx, |this, cx| {
                                this.delayed = true;
                                this.load_error = Some(error.to_string());
                                cx.notify();
                            });
                            break;
                        }
                    },
                }
            }
        })
        .detach();
        this
    }

    pub(crate) fn set_text_scale(&mut self, text_scale: f32, cx: &mut Context<Self>) {
        self.text_scale = text_scale.clamp(1., 2.);
        cx.notify();
    }

    fn reload_groups(&mut self, cx: &mut Context<Self>) {
        let previous_focus = self.focused_group.clone();
        let top_index = self
            .group_scroll
            .0
            .borrow()
            .base_handle
            .logical_scroll_top()
            .0;
        let scroll_anchor = self
            .groups
            .get(top_index)
            .map(|group| (group.project_id.clone(), group.group_id.clone()));
        let service = self.service.clone();
        let filters = self.filters.clone();
        self.groups_loading = true;
        self.groups_request_generation = self.groups_request_generation.wrapping_add(1);
        let generation = self.groups_request_generation;
        let task =
            cx.background_spawn(async move { service.list_failure_group_page(&filters, 0, 200) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.groups_request_generation != generation {
                    return;
                }
                this.groups_loading = false;
                match result {
                    Ok(page) => {
                        this.groups = page.rows;
                        this.group_total = page.total;
                        this.sort_groups();
                        let available = this
                            .groups
                            .iter()
                            .map(|group| (group.project_id.clone(), group.group_id.clone()))
                            .collect::<BTreeSet<_>>();
                        reconcile_group_identity_state(
                            &available,
                            &mut this.focused_group,
                            &mut this.selected_group_ids,
                            &mut this.selection_anchor,
                            previous_focus,
                        );
                        if let Some(anchor) = scroll_anchor
                            && let Some(index) = this.groups.iter().position(|group| {
                                group.project_id == anchor.0 && group.group_id == anchor.1
                            })
                        {
                            this.group_scroll
                                .scroll_to_item_strict(index, ScrollStrategy::Top);
                        }
                        this.load_error = None;
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_more_groups(&mut self, cx: &mut Context<Self>) {
        let offset = self.groups.len() as u64;
        if offset >= self.group_total {
            return;
        }
        let service = self.service.clone();
        let filters = self.filters.clone();
        self.groups_loading = true;
        self.groups_request_generation = self.groups_request_generation.wrapping_add(1);
        let generation = self.groups_request_generation;
        let task =
            cx.background_spawn(
                async move { service.list_failure_group_page(&filters, offset, 200) },
            );
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.groups_request_generation != generation {
                    return;
                }
                this.groups_loading = false;
                match result {
                    Ok(page) => {
                        this.groups.extend(page.rows);
                        this.group_total = page.total;
                        this.sort_groups();
                        this.load_error = None;
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.inspector_open = !self.inspector_open;
        self.inspector_auto_open_suppressed = !self.inspector_open;
        if self.inspector_open {
            self.group_details_open = false;
        }
        self.emit_inspector_preference(cx);
        cx.notify();
    }

    fn toggle_group_details(&mut self, cx: &mut Context<Self>) {
        self.group_details_open = !self.group_details_open;
        if self.group_details_open && self.inspector_open {
            self.inspector_open = false;
            self.emit_inspector_preference(cx);
        }
        cx.notify();
    }

    fn toggle_investigation_actions(&mut self, cx: &mut Context<Self>) {
        self.investigation_actions_open = !self.investigation_actions_open;
        cx.notify();
    }

    fn toggle_finding_review(&mut self, cx: &mut Context<Self>) {
        self.finding_review_open = !self.finding_review_open;
        cx.notify();
    }

    fn review_finding(&mut self, state: FindingDispositionStateV1, cx: &mut Context<Self>) {
        let Some((scope, group_id, finding_id)) = self.evidence.as_ref().map(|evidence| {
            (
                evidence.occurrence.scope.clone(),
                evidence.occurrence.group_id.clone(),
                evidence.occurrence.finding.finding_id.clone(),
            )
        }) else {
            return;
        };
        match self
            .service
            .set_finding_disposition(&scope, &group_id, &finding_id, state)
        {
            Ok(_) => self.refresh_reviewed_finding(&finding_id, cx),
            Err(error) => self.load_error = Some(error.to_string()),
        }
        self.finding_review_open = false;
        cx.notify();
    }

    fn undo_finding_review(&mut self, cx: &mut Context<Self>) {
        let Some((scope, group_id, finding_id)) = self.evidence.as_ref().map(|evidence| {
            (
                evidence.occurrence.scope.clone(),
                evidence.occurrence.group_id.clone(),
                evidence.occurrence.finding.finding_id.clone(),
            )
        }) else {
            return;
        };
        match self
            .service
            .undo_finding_disposition(&scope, &group_id, &finding_id)
        {
            Ok(_) => self.refresh_reviewed_finding(&finding_id, cx),
            Err(error) => self.load_error = Some(error.to_string()),
        }
        self.finding_review_open = false;
        cx.notify();
    }

    fn refresh_reviewed_finding(&mut self, finding_id: &str, cx: &mut Context<Self>) {
        let Some((project_id, group_id)) = self.selected_group.as_ref().map(|group| {
            (
                group.summary.project_id.clone(),
                group.summary.group_id.clone(),
            )
        }) else {
            return;
        };
        self.request_group(
            &project_id,
            &group_id,
            self.occurrence_offset,
            Some(finding_id.to_owned()),
            self.focused_span_id.clone(),
            cx,
        );
        self.reload_groups(cx);
    }

    pub(crate) fn set_inspector_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.inspector_open = open;
        self.inspector_auto_open_suppressed = !open;
        if open {
            self.group_details_open = false;
        }
        cx.notify();
    }

    pub(crate) fn restore_inspector_state(
        &mut self,
        open: bool,
        auto_open_suppressed: bool,
        cx: &mut Context<Self>,
    ) {
        self.inspector_open = open;
        self.inspector_auto_open_suppressed = auto_open_suppressed;
        if open {
            self.group_details_open = false;
        }
        cx.notify();
    }

    pub(crate) fn set_inspector_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.inspector_width = width.clamp(280., 640.);
        cx.notify();
    }

    fn focus_evidence_span(&mut self, span_id: String, cx: &mut Context<Self>) {
        self.focused_span_snapshot = self.evidence.as_ref().and_then(|evidence| {
            evidence
                .spans
                .iter()
                .find(|span| span.span_id == span_id)
                .cloned()
        });
        self.focused_span_id = Some(span_id);
        self.revealed_payload = None;
        self.auto_open_inspector(cx);
        cx.notify();
    }

    fn focus_full_trace_span(&mut self, span: SpanRow, cx: &mut Context<Self>) {
        self.focused_span_id = Some(span.span_id.clone());
        cx.emit(FailureInboxEvent::FullTraceSelectionChanged {
            span_id: span.span_id.clone(),
        });
        self.focused_span_snapshot = Some(span);
        self.revealed_payload = None;
        self.auto_open_inspector(cx);
        cx.notify();
    }

    fn auto_open_inspector(&mut self, cx: &mut Context<Self>) {
        if self.inspector_open || self.inspector_auto_open_suppressed {
            return;
        }
        self.inspector_open = true;
        self.group_details_open = false;
        self.emit_inspector_preference(cx);
    }

    fn emit_inspector_preference(&self, cx: &mut Context<Self>) {
        cx.emit(FailureInboxEvent::InspectorVisibilityChanged {
            visible: self.inspector_open,
            auto_open_suppressed: self.inspector_auto_open_suppressed,
        });
    }

    fn reveal_payload(&mut self, key: String, blob: BlobRefV1, cx: &mut Context<Self>) {
        match self.service.reveal_blob_preview(&blob) {
            Ok(preview) => {
                let value = serde_json::from_slice::<serde_json::Value>(&preview.bytes)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&preview.bytes).into_owned());
                self.revealed_payload = Some((key, value, preview));
            }
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn reveal_larger_payload(&mut self, cx: &mut Context<Self>) {
        let Some((key, _value, _preview)) = self.revealed_payload.clone() else {
            return;
        };
        let Some(blob) = self
            .focused_span_snapshot
            .as_ref()
            .and_then(|span| span.payload_refs.get(&key))
            .cloned()
        else {
            return;
        };
        match self.service.reveal_blob_larger_local(&blob) {
            Ok(preview) => {
                let value = serde_json::from_slice::<serde_json::Value>(&preview.bytes)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&preview.bytes).into_owned());
                self.revealed_payload = Some((key, value, preview));
            }
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn can_generate_eval(&self) -> bool {
        self.filters
            .scope
            .criteria
            .project_id
            .as_deref()
            .is_some_and(|project_id| project_id != UNASSIGNED_PROJECT_ID)
    }

    fn open_full_trace(&mut self, cx: &mut Context<Self>) {
        let Some(evidence) = &self.evidence else {
            return;
        };
        let project_id = evidence.occurrence.project_id.clone();
        let trace_id = evidence.occurrence.logical_trace_id.clone();
        let revision = evidence.occurrence.revision;
        let origin = FullTraceOrigin::FailureInvestigation {
            project_id: project_id.clone(),
            group_id: self
                .selected_group
                .as_ref()
                .map(|group| group.summary.group_id.clone())
                .unwrap_or_default(),
            finding_id: self.selected_finding_id.clone(),
            occurrence_offset: self.occurrence_offset,
            span_id: self.focused_span_id.clone(),
        };
        self.show_full_trace(
            &trace_id,
            revision,
            origin.clone(),
            self.focused_span_id.clone(),
            cx,
        );
        cx.emit(FailureInboxEvent::OpenFullTrace {
            project_id,
            logical_trace_id: trace_id,
            revision,
            origin,
        });
    }

    pub(crate) fn show_full_trace(
        &mut self,
        trace_id: &str,
        revision: u64,
        origin: FullTraceOrigin,
        selected_span_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match &origin {
            FullTraceOrigin::Runs => {
                self.evidence = None;
                self.focused_span_id = None;
                self.focused_span_snapshot = None;
            }
            FullTraceOrigin::FailureInvestigation {
                project_id,
                group_id,
                finding_id,
                occurrence_offset,
                span_id,
            } => {
                let context_matches = self.selected_group.as_ref().is_some_and(|group| {
                    group.summary.project_id == *project_id && group.summary.group_id == *group_id
                }) && self.selected_finding_id.as_deref()
                    == finding_id.as_deref();
                if !context_matches || self.evidence.is_none() {
                    self.restore_investigation_context(
                        project_id,
                        group_id,
                        finding_id.as_deref(),
                        *occurrence_offset,
                        span_id.as_deref(),
                        cx,
                    );
                } else if let Some(span_id) = span_id {
                    self.focused_span_id = Some(span_id.clone());
                    self.focused_span_snapshot = self.evidence.as_ref().and_then(|evidence| {
                        evidence
                            .spans
                            .iter()
                            .find(|span| span.span_id == *span_id)
                            .cloned()
                    });
                }
            }
        }
        if let Some(selected_span_id) = selected_span_id {
            self.focused_span_id = Some(selected_span_id);
            self.focused_span_snapshot = None;
        }
        if self.full_trace
            && self.full_trace_identity.as_ref() == Some(&(trace_id.to_owned(), revision))
            && self.full_trace_tree.has_page(&TreePageKey::new(None, 0))
        {
            self.full_trace_origin = origin;
            cx.notify();
            return;
        }
        let service = self.service.clone();
        let trace_id = trace_id.to_owned();
        self.full_trace_errors_only = false;
        self.full_trace_search.update(cx, |input, cx| {
            input.set_text("", cx);
        });
        self.full_trace_tree.clear();
        self.full_trace_timeline.clear();
        self.full_trace_start_unix_nano = 0;
        self.full_trace_end_unix_nano = 1;
        self.full_trace_identity = Some((trace_id.clone(), revision));
        self.full_trace_origin = origin;
        self.full_trace_span_count = 0;
        self.full_trace_loading = true;
        self.full_trace_request_generation = self.full_trace_request_generation.wrapping_add(1);
        let generation = self.full_trace_request_generation;
        self.full_trace = true;
        cx.notify();

        let task = cx.background_spawn(async move {
            let count = service.span_count(&trace_id, revision, None, false)?;
            let first_page = service.span_tree_page(&trace_id, revision, None, 0, 500)?;
            let timeline = service.list_spans_timeline(&trace_id, revision, 0, 500, None, false)?;
            let run = service.get_run(&trace_id)?;
            Ok::<_, perseval_service::LiveServiceError>((count, first_page, timeline, run))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.full_trace_request_generation != generation {
                    return;
                }
                this.full_trace_loading = false;
                match result {
                    Ok((count, first_page, timeline, run)) => {
                        this.full_trace_span_count = count;
                        this.full_trace_timeline.set_total(count);
                        this.full_trace_timeline.finish_load(0, timeline);
                        let key = TreePageKey::new(None, first_page.offset);
                        this.full_trace_tree.finish_load(&key, first_page);
                        if let Some(run) = run.filter(|run| run.revision == revision) {
                            this.full_trace_start_unix_nano = run.start_time_unix_nano;
                            this.full_trace_end_unix_nano = run
                                .end_time_unix_nano
                                .max(run.start_time_unix_nano.saturating_add(1));
                        }
                    }
                    Err(error) => this.load_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ensure_full_trace_tree_page(
        &mut self,
        parent_span_id: Option<String>,
        offset: u64,
        cx: &mut Context<Self>,
    ) {
        let key = TreePageKey::new(parent_span_id.clone(), offset);
        if !self.full_trace_tree.begin_load(key.clone()) {
            return;
        }
        let Some((trace_id, revision)) = self.full_trace_identity.clone() else {
            self.full_trace_tree.fail_load(&key);
            return;
        };
        let service = self.service.clone();
        let generation = self.full_trace_request_generation;
        let task = cx.background_spawn(async move {
            service.span_tree_page(
                &trace_id,
                revision,
                parent_span_id.as_deref(),
                offset,
                full_trace_tree::TREE_PAGE_SIZE as u32,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.full_trace_request_generation != generation {
                    return;
                }
                match result {
                    Ok(page) => {
                        this.full_trace_tree.finish_load(&key, page);
                    }
                    Err(error) => {
                        this.full_trace_tree.fail_load(&key);
                        this.load_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ensure_full_trace_timeline_page(&mut self, index: usize, cx: &mut Context<Self>) {
        let page = index as u64 / full_trace_timeline::TIMELINE_PAGE_SIZE;
        if !self.full_trace_timeline.begin_load(page) {
            return;
        }
        let Some((trace_id, revision)) = self.full_trace_identity.clone() else {
            self.full_trace_timeline.fail_load(page);
            return;
        };
        let service = self.service.clone();
        let generation = self.full_trace_request_generation;
        let offset = page.saturating_mul(full_trace_timeline::TIMELINE_PAGE_SIZE);
        let task = cx.background_spawn(async move {
            service.list_spans_timeline(
                &trace_id,
                revision,
                offset,
                full_trace_timeline::TIMELINE_PAGE_SIZE as u32,
                None,
                false,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.full_trace_request_generation != generation {
                    return;
                }
                match result {
                    Ok(rows) => this.full_trace_timeline.finish_load(page, rows),
                    Err(error) => {
                        this.full_trace_timeline.fail_load(page);
                        this.load_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn expand_all_loaded_trace_spans(&mut self, cx: &mut Context<Self>) {
        self.full_trace_tree.expand_all_loaded();
        cx.notify();
    }

    fn toggle_full_trace_span(
        &mut self,
        span_id: String,
        has_children: bool,
        cx: &mut Context<Self>,
    ) {
        if !has_children {
            self.focus_evidence_span(span_id, cx);
            return;
        }
        let expanded = self.full_trace_tree.toggle(&span_id);
        if expanded {
            self.ensure_full_trace_tree_page(Some(span_id), 0, cx);
        }
        cx.notify();
    }

    fn set_full_trace_mode(&mut self, mode: FullTraceMode, cx: &mut Context<Self>) {
        self.full_trace_mode = mode;
        cx.notify();
    }

    fn toggle_full_trace_errors(&mut self, cx: &mut Context<Self>) {
        self.full_trace_errors_only = !self.full_trace_errors_only;
        cx.notify();
    }

    fn reset_full_trace_filters(&mut self, cx: &mut Context<Self>) {
        self.full_trace_errors_only = false;
        self.full_trace_search.update(cx, |input, cx| {
            input.set_text("", cx);
        });
        cx.notify();
    }
}

impl Focusable for FailureInbox {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Context, Div, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyBinding, Render, Role, Window, actions, deferred, div, prelude::*, px,
};
use perseval_service::{
    LiveTraceService, PersevalConfigV1, ProjectV1, SourceHealth, TraceSnapshot, TraceSubscription,
};

use crate::components::TextInput;
use crate::design::{Breakpoint, Theme};
use crate::icons::{AppIcon, icon};
use crate::workbench::{
    ActivityId, EditorId, EditorKind, EditorNavigation, EditorResource, FullTraceOrigin, PaneId,
    WorkbenchAction, WorkbenchCommand, WorkbenchModel, command_descriptor, load_state, save_state,
};

use super::{
    compare, evals,
    failure_inbox::{FailureInbox, FailureInboxEvent},
    runs, settings, sources,
    welcome::{WelcomeEvent, WelcomeScreen},
};

mod chrome;
mod command_palette;
mod editor_labels;
mod navigation;
mod panes;

use editor_labels::*;

actions!(
    perseval_workbench,
    [
        OpenFailures,
        OpenRuns,
        OpenCompare,
        OpenEvals,
        OpenSources,
        OpenSettings,
        OpenCommandPalette,
        CommandPaletteNext,
        CommandPalettePrevious,
        CommandPaletteAccept,
        FocusNextControl,
        FocusPreviousControl,
        TogglePrimarySidebar,
        ToggleInspectorPane,
        ToggleBottomPanel,
        FocusNextPaneRegion,
        FocusPreviousPaneRegion,
        NavigateBack,
        NavigateForward,
        ReopenClosedEditor,
        DecreasePaneSize,
        IncreasePaneSize,
        ResetPaneSize,
        DismissTransient,
    ]
);

pub(crate) fn init_key_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-1", OpenFailures, None),
        KeyBinding::new("cmd-2", OpenRuns, None),
        KeyBinding::new("cmd-3", OpenCompare, None),
        KeyBinding::new("cmd-4", OpenEvals, None),
        KeyBinding::new("cmd-5", OpenSources, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-shift-p", OpenCommandPalette, None),
        KeyBinding::new("down", CommandPaletteNext, Some("CommandPalette")),
        KeyBinding::new("up", CommandPalettePrevious, Some("CommandPalette")),
        KeyBinding::new("enter", CommandPaletteAccept, Some("CommandPalette")),
        KeyBinding::new("tab", FocusNextControl, None),
        KeyBinding::new("shift-tab", FocusPreviousControl, None),
        KeyBinding::new("cmd-b", TogglePrimarySidebar, None),
        KeyBinding::new("cmd-alt-i", ToggleInspectorPane, None),
        KeyBinding::new("cmd-j", ToggleBottomPanel, None),
        KeyBinding::new("f6", FocusNextPaneRegion, None),
        KeyBinding::new("shift-f6", FocusPreviousPaneRegion, None),
        KeyBinding::new("ctrl-minus", NavigateBack, None),
        KeyBinding::new("ctrl-shift-minus", NavigateForward, None),
        KeyBinding::new("cmd-[", NavigateBack, None),
        KeyBinding::new("cmd-]", NavigateForward, None),
        KeyBinding::new("cmd-shift-t", ReopenClosedEditor, None),
        KeyBinding::new("left", DecreasePaneSize, Some("PaneResizeHandle")),
        KeyBinding::new("down", DecreasePaneSize, Some("PaneResizeHandle")),
        KeyBinding::new("right", IncreasePaneSize, Some("PaneResizeHandle")),
        KeyBinding::new("up", IncreasePaneSize, Some("PaneResizeHandle")),
        KeyBinding::new("enter", ResetPaneSize, Some("PaneResizeHandle")),
        KeyBinding::new("escape", DismissTransient, None),
    ]);
}

pub(crate) struct WorkbenchShell {
    service: Arc<LiveTraceService>,
    model: WorkbenchModel,
    failure_inbox: Entity<FailureInbox>,
    runs: Entity<runs::RunsScreen>,
    compare: Entity<compare::CompareScreen>,
    eval_review: Entity<evals::EvalReviewScreen>,
    sources: Entity<sources::SourcesScreen>,
    welcome: Entity<WelcomeScreen>,
    settings: Entity<settings::SettingsScreen>,
    projects: Vec<ProjectV1>,
    endpoint: String,
    health: SourceHealth,
    state_path: PathBuf,
    persistence_error: Option<String>,
    project_menu_open: bool,
    view_menu_open: bool,
    command_palette_open: bool,
    command_palette_selection: usize,
    pending_import_navigation: bool,
    transient_return_focus: Option<FocusHandle>,
    command_input: Entity<TextInput>,
    navigation: EditorNavigation,
    active_support_pane: Option<PaneId>,
    focus_handle: FocusHandle,
    primary_sidebar_focus: FocusHandle,
    inspector_pane_focus: FocusHandle,
    bottom_panel_focus: FocusHandle,
}

impl WorkbenchShell {
    pub(crate) fn new(
        config: &PersevalConfigV1,
        service: Arc<LiveTraceService>,
        snapshot: TraceSnapshot,
        subscription: TraceSubscription,
        cx: &mut Context<Self>,
    ) -> Self {
        let health = snapshot.health.clone();
        let total_runs = snapshot.total_runs;
        let projects = service
            .list_projects()
            .expect("list persisted workspace projects");
        let maximum_deltas = config.stream.ui_max_deltas_per_frame;
        let endpoint_address = health
            .effective_address
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| config.otlp.bind_addr.to_string());
        let endpoint = format!("http://{endpoint_address}/v1/traces");
        let sources = cx.new(|cx| {
            sources::SourcesScreen::new(
                service.clone(),
                projects.clone(),
                health.clone(),
                endpoint.clone(),
                cx,
            )
        });
        cx.subscribe(&sources, |this, _, event, cx| match event {
            sources::SourcesEvent::ProjectCreated(project) => {
                this.projects.push(project.clone());
                this.projects.sort_by_key(|project| {
                    (
                        project.display_name.to_lowercase(),
                        project.project_id.clone(),
                    )
                });
                this.model
                    .apply(WorkbenchAction::SetScope(crate::workbench::QueryScope {
                        project: crate::workbench::ProjectScope::Project(
                            project.project_id.clone(),
                        ),
                        ..crate::workbench::QueryScope::default()
                    }));
                let scope = this.model.state.scope.clone();
                let preferences = this.model.failure_inbox_preferences();
                this.failure_inbox.update(cx, |inbox, cx| {
                    inbox.set_query_scope(&scope, cx);
                    inbox.set_preferences(preferences, cx);
                });
                this.runs
                    .update(cx, |runs, cx| runs.set_query_scope(&scope, cx));
                this.eval_review.update(cx, |evals, cx| {
                    evals.set_project_scope(Some(project.project_id.clone()), cx)
                });
                this.refresh_settings_governance(cx);
                this.refresh_welcome_context(cx);
                this.persist();
                cx.notify();
                cx.refresh_windows();
            }
            sources::SourcesEvent::TraceImported => {
                this.refresh_welcome_context(cx);
                this.pending_import_navigation = true;
                // Durable acknowledgement can race the first projected/outbox
                // delta on small imports. Poll the bounded run summary instead
                // of depending on a later findings delta that may already have
                // been published before this event reaches the shell.
                let service = this.service.clone();
                let executor = cx.background_executor().clone();
                let task = cx.background_spawn(async move {
                    for _ in 0..100 {
                        if service.run_count().unwrap_or(0) > 0 {
                            return true;
                        }
                        executor.timer(Duration::from_millis(50)).await;
                    }
                    false
                });
                cx.spawn(async move |weak, cx| {
                    let projected = task.await;
                    let _ = weak.update(cx, |this, cx| {
                        if projected && this.pending_import_navigation {
                            this.pending_import_navigation = false;
                            this.runs.update(cx, |runs, cx| runs.reload(cx));
                            this.refresh_welcome_context(cx);
                            this.open_activity(ActivityId::Runs, cx);
                        }
                    });
                })
                .detach();
                cx.notify();
            }
        })
        .detach();
        let failure_inbox = cx.new(|cx| {
            FailureInbox::new(
                service.clone(),
                snapshot,
                subscription,
                maximum_deltas,
                config.query.cached_pages,
                cx,
            )
        });
        cx.subscribe(&failure_inbox, |this, _, event, cx| match event {
            FailureInboxEvent::OpenInvestigation {
                project_id,
                group_id,
            } => {
                this.open_editor(
                    EditorResource::FailureInvestigation {
                        project_id: project_id.clone(),
                        group_id: group_id.clone(),
                    },
                    false,
                );
                this.persist();
                cx.notify();
            }
            FailureInboxEvent::OpenEvalQueue => this.open_activity(ActivityId::Evals, cx),
            FailureInboxEvent::OpenRuns => this.open_activity(ActivityId::Runs, cx),
            FailureInboxEvent::OpenSources => this.open_activity(ActivityId::Sources, cx),
            FailureInboxEvent::OpenCompare(request) => {
                this.open_editor(EditorResource::CompareSetup, false);
                this.compare
                    .update(cx, |compare, cx| compare.compare(request.clone(), cx));
                this.persist();
                cx.notify();
            }
            FailureInboxEvent::OpenFullTrace {
                project_id,
                logical_trace_id,
                revision,
                origin,
            } => {
                let selected_span_id = match origin {
                    FullTraceOrigin::Runs => None,
                    FullTraceOrigin::FailureInvestigation { span_id, .. } => span_id.clone(),
                };
                this.open_editor(
                    EditorResource::FullTrace {
                        project_id: project_id.clone(),
                        logical_trace_id: logical_trace_id.clone(),
                        revision: *revision,
                        origin: origin.clone(),
                        selected_span_id,
                    },
                    false,
                );
                this.persist();
                cx.notify();
            }
            FailureInboxEvent::ReturnFromFullTrace { origin } => {
                match origin {
                    FullTraceOrigin::Runs => {
                        this.open_editor(EditorResource::Runs, true);
                    }
                    FullTraceOrigin::FailureInvestigation {
                        project_id,
                        group_id,
                        finding_id,
                        occurrence_offset,
                        span_id,
                    } => {
                        this.open_editor(
                            EditorResource::FailureInvestigation {
                                project_id: project_id.clone(),
                                group_id: group_id.clone(),
                            },
                            true,
                        );
                        this.failure_inbox.update(cx, |inbox, cx| {
                            inbox.restore_investigation_context(
                                project_id,
                                group_id,
                                finding_id.as_deref(),
                                *occurrence_offset,
                                span_id.as_deref(),
                                cx,
                            )
                        });
                    }
                }
                this.persist();
                cx.notify();
            }
            FailureInboxEvent::FullTraceSelectionChanged { span_id } => {
                this.model
                    .apply(WorkbenchAction::UpdateActiveFullTraceSelection(Some(
                        span_id.clone(),
                    )));
                this.persist();
            }
            FailureInboxEvent::InspectorVisibilityChanged {
                visible,
                auto_open_suppressed,
            } => {
                this.model.apply(WorkbenchAction::SetPaneVisible {
                    pane: PaneId::Inspector,
                    visible: *visible,
                });
                this.model
                    .apply(WorkbenchAction::SetInspectorAutoOpenSuppressed(
                        *auto_open_suppressed,
                    ));
                this.active_support_pane = (*visible).then_some(PaneId::Inspector);
                this.persist();
                cx.notify();
            }
            FailureInboxEvent::PreferencesChanged {
                scope_key,
                preferences,
            } => {
                this.model
                    .apply(WorkbenchAction::SetFailureInboxPreferences {
                        scope_key: scope_key.clone(),
                        preferences: preferences.clone(),
                    });
                this.persist();
            }
            FailureInboxEvent::TraceDataChanged => {
                this.runs.update(cx, |runs, cx| runs.reload(cx));
                this.refresh_welcome_context(cx);
                if this.pending_import_navigation {
                    this.pending_import_navigation = false;
                    this.open_activity(ActivityId::Runs, cx);
                }
            }
        })
        .detach();
        let runs = cx
            .new(|cx| runs::RunsScreen::new(service.clone(), None, config.query.cached_pages, cx));
        cx.subscribe(&runs, |this, _, event, cx| match event {
            runs::RunsEvent::OpenTrace {
                project_id,
                logical_trace_id,
                revision,
            } => {
                this.open_editor(
                    EditorResource::FullTrace {
                        project_id: project_id.clone(),
                        logical_trace_id: logical_trace_id.clone(),
                        revision: *revision,
                        origin: FullTraceOrigin::Runs,
                        selected_span_id: None,
                    },
                    false,
                );
                this.failure_inbox.update(cx, |inbox, cx| {
                    inbox.show_full_trace(
                        project_id,
                        logical_trace_id,
                        *revision,
                        FullTraceOrigin::Runs,
                        None,
                        cx,
                    )
                });
                this.persist();
                cx.notify();
            }
            runs::RunsEvent::OpenCompare(request) => {
                this.open_editor(EditorResource::CompareSetup, false);
                this.compare
                    .update(cx, |compare, cx| compare.compare(request.clone(), cx));
                this.persist();
                cx.notify();
            }
            runs::RunsEvent::OpenSources => this.open_activity(ActivityId::Sources, cx),
            runs::RunsEvent::ScopeChanged(scope) => {
                this.model.apply(WorkbenchAction::SetScope(scope.clone()));
                let preferences = this.model.failure_inbox_preferences();
                this.failure_inbox.update(cx, |inbox, cx| {
                    inbox.set_query_scope(scope, cx);
                    inbox.set_preferences(preferences, cx);
                });
                this.refresh_settings_governance(cx);
                this.persist();
                cx.notify();
            }
        })
        .detach();
        let compare = cx.new(|_| compare::CompareScreen::new(service.clone()));
        cx.subscribe(&compare, |this, _, event, cx| match event {
            compare::CompareEvent::ComparisonReady {
                project_id,
                comparison_id,
            } => {
                this.open_editor(
                    EditorResource::Compare {
                        project_id: project_id.clone(),
                        comparison_id: comparison_id.clone(),
                    },
                    false,
                );
                this.persist();
                cx.notify();
            }
            compare::CompareEvent::OpenRuns => this.open_activity(ActivityId::Runs, cx),
        })
        .detach();
        let eval_reviewer_ref = config.reviewer_ref.clone();
        let eval_review =
            cx.new(|cx| evals::EvalReviewScreen::new(service.clone(), None, eval_reviewer_ref, cx));
        cx.subscribe(&eval_review, |this, _, event, cx| match event {
            evals::EvalReviewEvent::Candidate {
                project_id,
                candidate_id,
            } => {
                this.open_editor(
                    EditorResource::EvalReview {
                        project_id: project_id.clone(),
                        candidate_id: candidate_id.clone(),
                    },
                    false,
                );
                this.eval_review.update(cx, |evals, cx| {
                    evals.show_candidate(project_id, candidate_id, cx)
                });
                this.persist();
                cx.notify();
            }
            evals::EvalReviewEvent::Queue => this.open_activity(ActivityId::Evals, cx),
            evals::EvalReviewEvent::SourceTrace {
                project_id,
                logical_trace_id,
                revision,
                selected_span_id,
            } => {
                this.open_editor(
                    EditorResource::FullTrace {
                        project_id: project_id.clone(),
                        logical_trace_id: logical_trace_id.clone(),
                        revision: *revision,
                        origin: FullTraceOrigin::Runs,
                        selected_span_id: selected_span_id.clone(),
                    },
                    false,
                );
                this.failure_inbox.update(cx, |inbox, cx| {
                    inbox.show_full_trace(
                        project_id,
                        logical_trace_id,
                        *revision,
                        FullTraceOrigin::Runs,
                        selected_span_id.clone(),
                        cx,
                    )
                });
                this.persist();
                cx.notify();
            }
        })
        .detach();
        let initial_editor = if total_runs == 0 {
            EditorResource::Welcome
        } else {
            EditorResource::FailureInbox
        };
        let state_path = config.workspace_dir.join("workbench-state-v1.json");
        let valid_projects = projects
            .iter()
            .map(|project| project.project_id.clone())
            .collect::<BTreeSet<_>>();
        let (mut model, persistence_error) = if projects.is_empty() {
            (WorkbenchModel::with_initial_editor(initial_editor), None)
        } else {
            match load_state(&state_path) {
                Ok(Some(state)) => (
                    WorkbenchModel::restore(state, &valid_projects, initial_editor.clone()),
                    None,
                ),
                Ok(None) => (
                    WorkbenchModel::with_initial_editor(initial_editor.clone()),
                    None,
                ),
                Err(error) => (
                    WorkbenchModel::with_initial_editor(initial_editor),
                    Some(error.to_string()),
                ),
            }
        };
        if let [project] = projects.as_slice()
            && matches!(
                model.state.scope.project,
                crate::workbench::ProjectScope::AllProjects
            )
        {
            model.apply(WorkbenchAction::SetScope(crate::workbench::QueryScope {
                project: crate::workbench::ProjectScope::Project(project.project_id.clone()),
                ..crate::workbench::QueryScope::default()
            }));
        }
        let selected_project_id = match &model.state.scope.project {
            crate::workbench::ProjectScope::Project(project_id) => Some(project_id.clone()),
            crate::workbench::ProjectScope::AllProjects => None,
        };
        if let Some(project_id) = selected_project_id.as_deref() {
            sources.update(cx, |sources, cx| sources.select_project(project_id, cx));
        }
        let has_failures = service
            .has_active_findings(selected_project_id.as_deref())
            .unwrap_or(false);
        let restored_scope = model.state.scope.clone();
        let restored_preferences = model.failure_inbox_preferences();
        let restored_text_scale = model.state.appearance.text_scale.factor();
        failure_inbox.update(cx, |inbox, cx| {
            inbox.set_query_scope(&restored_scope, cx);
            inbox.set_preferences(restored_preferences, cx);
            inbox.set_text_scale(restored_text_scale, cx);
        });
        runs.update(cx, |runs, cx| {
            runs.set_query_scope(&restored_scope, cx);
            runs.set_text_scale(restored_text_scale, cx);
        });
        eval_review.update(cx, |evals, cx| {
            evals.set_project_scope(selected_project_id.clone(), cx)
        });
        let restored_inspector_open = model.state.panes.inspector_visible
            && matches!(
                active_kind(&model),
                EditorKind::FailureInvestigation | EditorKind::FullTrace
            );
        let restored_inspector_width = model.state.panes.inspector_width;
        let inspector_auto_open_suppressed = model.state.inspector_auto_open_suppressed;
        failure_inbox.update(cx, |inbox, cx| {
            inbox.set_inspector_width(restored_inspector_width, cx);
            inbox.restore_inspector_state(
                restored_inspector_open,
                inspector_auto_open_suppressed,
                cx,
            );
        });
        sync_editor_resource(
            active_resource(&model),
            &failure_inbox,
            &compare,
            &eval_review,
            cx,
        );
        let settings_project_id = selected_project_id.clone();
        let welcome = cx.new(|_| {
            WelcomeScreen::new(
                projects.len(),
                total_runs,
                has_failures,
                selected_project_id,
                endpoint.clone(),
                health.clone(),
            )
        });
        cx.subscribe(&welcome, |this, _, event, cx| match event {
            WelcomeEvent::Sources => this.open_activity(ActivityId::Sources, cx),
            WelcomeEvent::Runs => this.open_activity(ActivityId::Runs, cx),
            WelcomeEvent::FailureInbox => this.open_activity(ActivityId::Failures, cx),
        })
        .detach();
        let appearance = model.state.appearance.clone();
        let assessment_health = service
            .assessment_runtime_health_for_project(settings_project_id.as_deref())
            .unwrap_or_default();
        let context_governance = settings_project_id
            .as_deref()
            .and_then(|project_id| service.agent_context_governance_summary(project_id).ok())
            .unwrap_or_default();
        let taxonomy_governance = settings_project_id
            .as_deref()
            .and_then(|project_id| service.taxonomy_governance_summary(project_id).ok())
            .unwrap_or_default();
        let settings = cx.new(|cx| {
            settings::SettingsScreen::new(
                Some(service.clone()),
                config.clone(),
                appearance.clone(),
                health.openai.clone(),
                assessment_health,
                settings_project_id,
                context_governance,
                taxonomy_governance,
                cx,
            )
        });
        cx.subscribe(&settings, |this, _, event, cx| match event {
            settings::SettingsEvent::AppearanceChanged(appearance) => {
                this.model
                    .apply(WorkbenchAction::SetAppearance(appearance.clone()));
                this.failure_inbox.update(cx, |inbox, cx| {
                    inbox.set_text_scale(appearance.text_scale.factor(), cx)
                });
                this.runs.update(cx, |runs, cx| {
                    runs.set_text_scale(appearance.text_scale.factor(), cx)
                });
                this.persist();
                cx.notify();
            }
        })
        .detach();
        let command_input = cx.new(|cx| TextInput::new("Search commands…", 256, cx));
        cx.observe(&command_input, |this, _, cx| {
            this.command_palette_selection = 0;
            cx.notify();
        })
        .detach();
        let shell_subscription = service
            .snapshot_and_subscribe()
            .expect("subscribe shell status updates")
            .1;
        let status_service = service.clone();
        cx.spawn(async move |weak, cx| {
            while shell_subscription.recv().await.is_ok() {
                if weak
                    .update(cx, |this, cx| {
                        this.health = status_service
                            .source_health()
                            .unwrap_or_else(|_| this.health.clone());
                        if let Some(address) = this.health.effective_address.as_deref() {
                            this.endpoint = format!("http://{address}/v1/traces");
                        }
                        let health = this.health.clone();
                        this.sources
                            .update(cx, |sources, cx| sources.update_health(health, cx));
                        let openai_health = this.health.openai.clone();
                        let selected_project_id = this.selected_project_id().map(str::to_owned);
                        let assessment_health = status_service
                            .assessment_runtime_health_for_project(selected_project_id.as_deref())
                            .unwrap_or_default();
                        let context_governance = selected_project_id
                            .as_deref()
                            .and_then(|project_id| {
                                status_service
                                    .agent_context_governance_summary(project_id)
                                    .ok()
                            })
                            .unwrap_or_default();
                        let taxonomy_governance = selected_project_id
                            .as_deref()
                            .and_then(|project_id| {
                                status_service.taxonomy_governance_summary(project_id).ok()
                            })
                            .unwrap_or_default();
                        this.settings.update(cx, |settings, cx| {
                            settings.update_openai_health(openai_health, cx);
                            settings.update_assessment_health(assessment_health, cx);
                            settings.update_governance(
                                selected_project_id,
                                context_governance,
                                taxonomy_governance,
                                cx,
                            );
                        });
                        this.refresh_welcome_context(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let navigation = EditorNavigation::new(model.state.active_editor.clone());
        let active_support_pane = if model.state.panes.inspector_visible {
            Some(PaneId::Inspector)
        } else if model.state.panes.primary_sidebar_visible {
            Some(PaneId::PrimarySidebar)
        } else if model.state.panes.bottom_panel_visible {
            Some(PaneId::BottomPanel)
        } else {
            None
        };
        Self {
            service,
            model,
            failure_inbox,
            runs,
            compare,
            eval_review,
            sources,
            welcome,
            settings,
            projects,
            endpoint,
            health,
            state_path,
            persistence_error,
            project_menu_open: false,
            view_menu_open: false,
            command_palette_open: false,
            command_palette_selection: 0,
            pending_import_navigation: false,
            transient_return_focus: None,
            command_input,
            navigation,
            active_support_pane,
            focus_handle: cx.focus_handle(),
            primary_sidebar_focus: cx.focus_handle(),
            inspector_pane_focus: cx.focus_handle(),
            bottom_panel_focus: cx.focus_handle(),
        }
    }

    fn persist(&mut self) {
        if let Err(error) = save_state(&self.state_path, &self.model.state) {
            self.persistence_error = Some(error.to_string());
        } else {
            self.persistence_error = None;
        }
    }

    fn refresh_welcome_context(&mut self, cx: &mut Context<Self>) {
        let selected_project_id = self.selected_project_id().map(str::to_string);
        let filters = perseval_service::RunFiltersV1 {
            scope: perseval_service::QueryScopeV1::new(perseval_service::QueryScopeCriteriaV1 {
                project_id: selected_project_id.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let run_count = self
            .service
            .run_count_filtered(&filters)
            .unwrap_or_default();
        let has_failures = self
            .service
            .has_active_findings(selected_project_id.as_deref())
            .unwrap_or(false);
        self.welcome.update(cx, |welcome, cx| {
            welcome.update_context(
                self.projects.len(),
                run_count,
                has_failures,
                selected_project_id,
                self.health.clone(),
                cx,
            )
        });
    }

    fn refresh_settings_governance(&mut self, cx: &mut Context<Self>) {
        let selected_project_id = self.selected_project_id().map(str::to_owned);
        let assessment_health = self
            .service
            .assessment_runtime_health_for_project(selected_project_id.as_deref())
            .unwrap_or_default();
        let context_governance = selected_project_id
            .as_deref()
            .and_then(|project_id| {
                self.service
                    .agent_context_governance_summary(project_id)
                    .ok()
            })
            .unwrap_or_default();
        let taxonomy_governance = selected_project_id
            .as_deref()
            .and_then(|project_id| self.service.taxonomy_governance_summary(project_id).ok())
            .unwrap_or_default();
        self.settings.update(cx, |settings, cx| {
            settings.update_assessment_health(assessment_health, cx);
            settings.update_governance(
                selected_project_id,
                context_governance,
                taxonomy_governance,
                cx,
            );
        });
    }

    fn toggle_project_menu(&mut self, cx: &mut Context<Self>) {
        self.view_menu_open = false;
        self.project_menu_open = !self.project_menu_open;
        cx.notify();
        cx.refresh_windows();
    }

    fn create_project_from_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_menu_open = false;
        self.sources
            .update(cx, |sources, cx| sources.show_project_creation(cx));
        self.open_activity_in_window(ActivityId::Sources, window, cx);
    }

    fn manage_project_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_menu_open = false;
        self.open_activity_in_window(ActivityId::Sources, window, cx);
    }

    fn toggle_view_menu(&mut self, cx: &mut Context<Self>) {
        self.project_menu_open = false;
        self.view_menu_open = !self.view_menu_open;
        cx.notify();
        cx.refresh_windows();
    }

    fn set_project_scope(
        &mut self,
        project: crate::workbench::ProjectScope,
        cx: &mut Context<Self>,
    ) {
        let project_id = match &project {
            crate::workbench::ProjectScope::Project(project_id) => Some(project_id.clone()),
            crate::workbench::ProjectScope::AllProjects => None,
        };
        self.model
            .apply(WorkbenchAction::SetScope(crate::workbench::QueryScope {
                project,
                ..self.model.state.scope.clone()
            }));
        let scope = self.model.state.scope.clone();
        let preferences = self.model.failure_inbox_preferences();
        self.failure_inbox.update(cx, |inbox, cx| {
            inbox.set_query_scope(&scope, cx);
            inbox.set_preferences(preferences, cx);
        });
        self.runs
            .update(cx, |runs, cx| runs.set_query_scope(&scope, cx));
        self.eval_review.update(cx, |evals, cx| {
            evals.set_project_scope(project_id.clone(), cx)
        });
        self.sources.update(cx, |sources, cx| {
            if let Some(project_id) = project_id.as_deref() {
                sources.select_project(project_id, cx);
            } else {
                sources.select_all_projects(cx);
            }
        });
        self.refresh_settings_governance(cx);
        self.refresh_welcome_context(cx);
        self.sync_failure_view(cx);
        self.project_menu_open = false;
        self.persist();
        cx.notify();
        cx.refresh_windows();
    }

    fn set_project_scope_in_window(
        &mut self,
        project: crate::workbench::ProjectScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_project_scope(project, cx);
        Self::request_navigation_repaint(window);
    }

    fn sync_failure_view(&mut self, cx: &mut Context<Self>) {
        sync_editor_resource(
            active_resource(&self.model),
            &self.failure_inbox,
            &self.compare,
            &self.eval_review,
            cx,
        );
    }

    fn selected_project_id(&self) -> Option<&str> {
        match &self.model.state.scope.project {
            crate::workbench::ProjectScope::Project(project_id) => Some(project_id),
            crate::workbench::ProjectScope::AllProjects => None,
        }
    }

    fn project_scope_label(&self) -> String {
        let Some(project_id) = self.selected_project_id() else {
            return if self.projects.is_empty() {
                "No project".into()
            } else {
                "All Projects · read-only".into()
            };
        };
        self.projects
            .iter()
            .find(|project| project.project_id == project_id)
            .map(|project| project.display_name.clone())
            .unwrap_or_else(|| format!("Unknown · {project_id}"))
    }

    fn secondary_scope_label(&self) -> String {
        let scope = &self.model.state.scope;
        let mut parts = Vec::new();
        if let Some(environment) = scope.environment.as_deref() {
            parts.push(environment.to_string());
        }
        if let Some(build) = scope.build.as_deref() {
            parts.push(format!("build {build}"));
        }
        if let Some(session) = scope.session.as_deref() {
            parts.push(format!("session {session}"));
        }
        if let Some(time_range) = scope.time_range.as_deref() {
            parts.push(
                match time_range {
                    "last_hour" => "last hour",
                    "last_day" => "last 24 hours",
                    "last_week" => "last 7 days",
                    other => other,
                }
                .to_string(),
            );
        }
        if parts.is_empty() {
            "All runs".into()
        } else {
            parts.join(" · ")
        }
    }

    fn has_secondary_scope(&self) -> bool {
        let scope = &self.model.state.scope;
        scope.environment.is_some()
            || scope.build.is_some()
            || scope.session.is_some()
            || scope.time_range.is_some()
    }
}

fn active_resource(model: &WorkbenchModel) -> Option<EditorResource> {
    model
        .state
        .active_editor
        .as_ref()
        .and_then(|id| model.state.editors.iter().find(|tab| &tab.id == id))
        .map(|tab| tab.resource.clone())
}

fn sync_editor_resource(
    resource: Option<EditorResource>,
    failure_inbox: &Entity<FailureInbox>,
    compare: &Entity<compare::CompareScreen>,
    eval_review: &Entity<evals::EvalReviewScreen>,
    cx: &mut Context<WorkbenchShell>,
) {
    match resource {
        Some(EditorResource::FailureInbox) => {
            failure_inbox.update(cx, |inbox, cx| inbox.show_inbox(cx));
        }
        Some(EditorResource::FailureInvestigation {
            project_id,
            group_id,
        }) => {
            failure_inbox.update(cx, |inbox, cx| {
                inbox.show_investigation(&project_id, &group_id, cx)
            });
        }
        Some(EditorResource::FullTrace {
            project_id,
            logical_trace_id,
            revision,
            origin,
            selected_span_id,
        }) => {
            failure_inbox.update(cx, |inbox, cx| {
                inbox.show_full_trace(
                    &project_id,
                    &logical_trace_id,
                    revision,
                    origin,
                    selected_span_id,
                    cx,
                )
            });
        }
        Some(EditorResource::EvaluatorStudio) => {
            eval_review.update(cx, |evals, cx| evals.show_studio(cx));
        }
        Some(EditorResource::EvalQueue) => {
            eval_review.update(cx, |evals, cx| evals.show_queue(cx));
        }
        Some(EditorResource::EvalReview {
            project_id,
            candidate_id,
        }) => {
            eval_review.update(cx, |evals, cx| {
                evals.show_candidate(&project_id, &candidate_id, cx)
            });
        }
        Some(EditorResource::CompareSetup) => {
            compare.update(cx, |compare, cx| compare.show_setup(cx));
        }
        Some(EditorResource::Compare { comparison_id, .. }) => {
            compare.update(cx, |compare, cx| {
                compare.show_comparison(&comparison_id, cx)
            });
        }
        _ => {}
    }
}

impl Render for WorkbenchShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_scale = self.model.state.appearance.text_scale.factor();
        window.set_rem_size(px(16. * text_scale));
        let width: f32 = window.viewport_size().width.into();
        let breakpoint = Breakpoint::for_width(width / text_scale);
        let compact = breakpoint == Breakpoint::Compact;
        let active_kind = active_kind(&self.model);
        let editor: gpui::AnyElement = match active_kind {
            EditorKind::Welcome => self.welcome.clone().into_any_element(),
            EditorKind::FailureInbox | EditorKind::FailureInvestigation | EditorKind::FullTrace => {
                self.failure_inbox.clone().into_any_element()
            }
            EditorKind::Runs => self.runs.clone().into_any_element(),
            EditorKind::Sources => self.sources.clone().into_any_element(),
            EditorKind::Compare => self.compare.clone().into_any_element(),
            EditorKind::EvalReview => self.eval_review.clone().into_any_element(),
            EditorKind::Settings => self.settings.clone().into_any_element(),
        };
        let command_palette = self
            .command_palette_open
            .then(|| self.render_command_palette(cx));
        let workspace_content = self.render_workspace_content(editor, breakpoint, active_kind, cx);
        div()
            .id("perseval-workbench")
            .role(Role::Application)
            .aria_label("Perseval trace investigation workbench")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &OpenFailures, window, cx| {
                this.open_activity_in_window(ActivityId::Failures, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenRuns, window, cx| {
                this.open_activity_in_window(ActivityId::Runs, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenCompare, window, cx| {
                this.open_activity_in_window(ActivityId::Compare, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenEvals, window, cx| {
                this.open_activity_in_window(ActivityId::Evals, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSources, window, cx| {
                this.open_activity_in_window(ActivityId::Sources, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_activity_in_window(ActivityId::Settings, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.open_command_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CommandPaletteNext, _, cx| {
                this.move_command_palette_selection(1, cx)
            }))
            .on_action(cx.listener(|this, _: &CommandPalettePrevious, _, cx| {
                this.move_command_palette_selection(-1, cx)
            }))
            .on_action(cx.listener(|this, _: &CommandPaletteAccept, window, cx| {
                this.accept_command_palette(window, cx)
            }))
            .on_action(cx.listener(|_, _: &FocusNextControl, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPreviousControl, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|this, _: &TogglePrimarySidebar, _, cx| {
                this.toggle_pane(PaneId::PrimarySidebar, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleInspectorPane, _, cx| {
                this.toggle_pane(PaneId::Inspector, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBottomPanel, _, cx| {
                this.toggle_pane(PaneId::BottomPanel, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusNextPaneRegion, window, cx| {
                this.focus_adjacent_pane(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FocusPreviousPaneRegion, window, cx| {
                    this.focus_adjacent_pane(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &NavigateBack, _, cx| this.navigate_back(cx)))
            .on_action(cx.listener(|this, _: &NavigateForward, _, cx| this.navigate_forward(cx)))
            .on_action(
                cx.listener(|this, _: &ReopenClosedEditor, _, cx| this.reopen_closed_editor(cx)),
            )
            .on_action(cx.listener(|this, _: &DismissTransient, window, cx| {
                this.dismiss_transient(window, cx)
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .child(self.render_header(compact, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_activity_rail(compact, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(self.render_tabs(cx))
                            .child(workspace_content)
                            .child(self.render_status_bar(compact)),
                    ),
            )
            .when_some(command_palette, |root, palette| root.child(palette))
    }
}

impl Focusable for WorkbenchShell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn activity_button(
    index: usize,
    glyph: AppIcon,
    label: &'static str,
    shortcut: &'static str,
    active: bool,
) -> gpui::Stateful<Div> {
    div()
        .id(("activity", index))
        .role(Role::Button)
        .aria_label(format!("{label}, {shortcut}"))
        .aria_selected(active)
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .size(px(40.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .border_1()
        .border_color(if active { Theme::CYAN } else { Theme::PANEL })
        .bg(if active {
            Theme::PANEL_ALT
        } else {
            Theme::PANEL
        })
        .text_color(if active { Theme::CYAN } else { Theme::MUTED })
        .cursor_pointer()
        .hover(|style| style.bg(Theme::PANEL_ALT).text_color(Theme::TEXT))
        .child(icon(glyph, 17., active))
        .tooltip(move |_, cx| cx.new(|_| ActivityTooltip { label, shortcut }).into())
}

struct ActivityTooltip {
    label: &'static str,
    shortcut: &'static str,
}

impl Render for ActivityTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(180.))
            .h(px(34.))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(5.))
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL_ALT)
            .shadow_lg()
            .text_xs()
            .text_color(Theme::TEXT)
            .child(self.label)
            .child(div().text_color(Theme::DIM).child(self.shortcut))
    }
}

fn scope_chip(label: &str, value: &str) -> Div {
    div()
        .h(px(28.))
        .max_w(px(240.))
        .min_w_0()
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .border_1()
        .border_color(Theme::BORDER)
        .text_xs()
        .child(
            div()
                .flex_none()
                .text_color(Theme::MUTED)
                .child(label.to_string()),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(value.to_string()),
        )
}

fn status_chip(label: &str, tint: gpui::Rgba) -> Div {
    div()
        .h(px(28.))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_sm()
        .border_1()
        .border_color(Theme::BORDER)
        .text_xs()
        .child(div().size(px(6.)).rounded_full().bg(tint))
        .child(label.to_string())
}

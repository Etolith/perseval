use std::collections::BTreeSet;

use super::{
    EditorId, EditorResource, EditorTabState, FocusRegion, PaneId, PaneLayout, ProjectContextState,
    ProjectScope, QueryScope, WorkbenchAction, WorkbenchStateV1,
};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkbenchModel {
    pub state: WorkbenchStateV1,
}

impl WorkbenchModel {
    pub fn new(state: WorkbenchStateV1) -> Self {
        Self { state }
    }

    pub fn with_initial_editor(resource: EditorResource) -> Self {
        let tab = EditorTabState::new(resource.clone(), !resource.is_activity_destination());
        Self {
            state: WorkbenchStateV1 {
                active_activity: resource.kind().activity(),
                active_editor: Some(tab.id.clone()),
                editors: vec![tab],
                ..WorkbenchStateV1::default()
            },
        }
    }

    pub fn restore(
        mut state: WorkbenchStateV1,
        valid_project_ids: &BTreeSet<String>,
        fallback: EditorResource,
    ) -> Self {
        state.editors.retain(|tab| {
            resource_project_id(&tab.resource)
                .is_none_or(|project_id| valid_project_ids.contains(project_id))
        });
        let active_editor = state.active_editor.clone();
        state.editors.retain(|tab| {
            !tab.resource.is_activity_destination() || active_editor.as_ref() == Some(&tab.id)
        });
        if let Some(active) = active_editor
            && let Some(tab) = state.editors.iter_mut().find(|tab| tab.id == active)
            && tab.resource.is_activity_destination()
        {
            tab.pinned = false;
        }
        if let super::ProjectScope::Project(project_id) = &state.scope.project
            && !valid_project_ids.contains(project_id)
        {
            state.scope = super::QueryScope::default();
            state.bulk_selection.failure_group_ids.clear();
        }
        state
            .project_contexts
            .retain(|project_id, _| valid_project_ids.contains(project_id));
        state.failure_inbox_preferences.retain(|scope_key, _| {
            scope_key == "all-projects"
                || scope_key
                    .strip_prefix("project:")
                    .is_some_and(|project_id| valid_project_ids.contains(project_id))
        });
        let valid_editor_ids = state
            .editors
            .iter()
            .map(|tab| tab.id.clone())
            .collect::<BTreeSet<_>>();
        for context in state.project_contexts.values_mut() {
            if context
                .active_editor
                .as_ref()
                .is_some_and(|id| !valid_editor_ids.contains(id))
            {
                context.active_editor = None;
            }
        }
        if state.editors.is_empty() {
            return Self::with_initial_editor(fallback);
        }
        let active_is_valid = state
            .active_editor
            .as_ref()
            .is_some_and(|id| state.editors.iter().any(|tab| &tab.id == id));
        if !active_is_valid {
            state.active_editor = state.editors.last().map(|tab| tab.id.clone());
        }
        if let Some(active) = state
            .active_editor
            .as_ref()
            .and_then(|id| state.editors.iter().find(|tab| &tab.id == id))
        {
            state.active_activity = active.resource.kind().activity();
        }
        Self { state }
    }

    pub fn apply(&mut self, action: WorkbenchAction) {
        match action {
            WorkbenchAction::OpenEditor { resource, pinned } => {
                let id = EditorId::for_resource(&resource);
                if let Some(existing) = self.state.editors.iter_mut().find(|tab| tab.id == id) {
                    existing.pinned |= pinned;
                    existing.resource = resource.clone();
                } else if !pinned {
                    if let Some(preview) = self.state.editors.iter_mut().find(|tab| !tab.pinned) {
                        *preview = EditorTabState::new(resource.clone(), false);
                    } else {
                        self.state
                            .editors
                            .push(EditorTabState::new(resource.clone(), false));
                    }
                } else {
                    self.state
                        .editors
                        .push(EditorTabState::new(resource.clone(), pinned));
                }
                self.state.active_activity = resource.kind().activity();
                self.state.active_editor = Some(id);
                self.state.focus = FocusRegion::Editor;
            }
            WorkbenchAction::ActivateEditor(id) => {
                if let Some(tab) = self.state.editors.iter().find(|tab| tab.id == id) {
                    self.state.active_activity = tab.resource.kind().activity();
                    self.state.active_editor = Some(id);
                    self.state.focus = FocusRegion::Editor;
                }
            }
            WorkbenchAction::PinEditor(id) => {
                if let Some(tab) = self.state.editors.iter_mut().find(|tab| tab.id == id) {
                    tab.pinned = true;
                }
            }
            WorkbenchAction::CloseEditor(id) => {
                let was_active = self.state.active_editor.as_ref() == Some(&id);
                self.state.editors.retain(|tab| tab.id != id);
                if was_active {
                    self.state.active_editor = self.state.editors.last().map(|tab| tab.id.clone());
                }
                self.state.focus = FocusRegion::EditorTabs;
            }
            WorkbenchAction::SetPaneVisible { pane, visible } => {
                match pane {
                    PaneId::PrimarySidebar => {
                        self.state.panes.primary_sidebar_visible = visible;
                    }
                    PaneId::Inspector => self.state.panes.inspector_visible = visible,
                    PaneId::BottomPanel => self.state.panes.bottom_panel_visible = visible,
                }
                if !visible && pane_focus(pane) == self.state.focus {
                    self.state.focus = FocusRegion::Editor;
                }
            }
            WorkbenchAction::ResizePane { pane, size } => match pane {
                PaneId::PrimarySidebar => {
                    self.state.panes.primary_sidebar_width = size.clamp(220., 480.);
                }
                PaneId::Inspector => {
                    self.state.panes.inspector_width = size.clamp(280., 640.);
                }
                PaneId::BottomPanel => {
                    self.state.panes.bottom_panel_height = size.clamp(140., 520.);
                }
            },
            WorkbenchAction::SetScope(scope) => {
                self.switch_scope(scope);
                self.state.bulk_selection.failure_group_ids.clear();
            }
            WorkbenchAction::SetFocus(focus) => self.state.focus = focus,
            WorkbenchAction::ToggleFailureGroup(group_id) => {
                if !self
                    .state
                    .bulk_selection
                    .failure_group_ids
                    .remove(&group_id)
                {
                    self.state.bulk_selection.failure_group_ids.insert(group_id);
                }
            }
            WorkbenchAction::ClearBulkSelection => {
                self.state.bulk_selection.failure_group_ids.clear();
            }
            WorkbenchAction::SetFailureInboxPreferences {
                scope_key,
                preferences,
            } => {
                self.state
                    .failure_inbox_preferences
                    .insert(scope_key, preferences);
            }
            WorkbenchAction::SetInspectorAutoOpenSuppressed(suppressed) => {
                self.state.inspector_auto_open_suppressed = suppressed;
            }
            WorkbenchAction::UpdateActiveFullTraceSelection(updated_span_id) => {
                if let Some(tab) = self
                    .state
                    .active_editor
                    .as_ref()
                    .and_then(|id| self.state.editors.iter_mut().find(|tab| &tab.id == id))
                    && let EditorResource::FullTrace {
                        selected_span_id, ..
                    } = &mut tab.resource
                {
                    *selected_span_id = updated_span_id;
                }
            }
            WorkbenchAction::SetAppearance(appearance) => {
                self.state.appearance = appearance;
            }
            WorkbenchAction::ResetLayout => self.state.panes = PaneLayout::default(),
        }
    }

    pub fn failure_inbox_preferences(&self) -> super::FailureInboxPreferencesV1 {
        self.state
            .failure_inbox_preferences
            .get(&self.state.scope.preference_key())
            .cloned()
            .unwrap_or_default()
    }

    fn switch_scope(&mut self, requested: QueryScope) {
        if self.state.scope.project == requested.project {
            self.state.scope = requested;
            return;
        }

        if let ProjectScope::Project(project_id) = &self.state.scope.project {
            self.state.project_contexts.insert(
                project_id.clone(),
                ProjectContextState {
                    environment: self.state.scope.environment.clone(),
                    build: self.state.scope.build.clone(),
                    session: self.state.scope.session.clone(),
                    time_range: self.state.scope.time_range.clone(),
                    active_activity: self.state.active_activity,
                    active_editor: self.state.active_editor.clone(),
                    panes: self.state.panes.clone(),
                },
            );
        }

        match &requested.project {
            ProjectScope::Project(project_id) => {
                if let Some(context) = self.state.project_contexts.get(project_id).cloned() {
                    self.state.scope = QueryScope {
                        project: requested.project.clone(),
                        environment: context.environment,
                        build: context.build,
                        session: context.session,
                        time_range: context.time_range,
                    };
                    self.state.active_activity = context.active_activity;
                    self.state.panes = context.panes;
                    if context.active_editor.as_ref().is_some_and(|id| {
                        self.state.editors.iter().any(|tab| {
                            &tab.id == id && resource_visible_for_project(&tab.resource, project_id)
                        })
                    }) {
                        self.state.active_editor = context.active_editor;
                    } else {
                        let restored_editor = self
                            .state
                            .editors
                            .iter()
                            .find(|tab| {
                                tab.resource.kind().activity() == context.active_activity
                                    && resource_visible_for_project(&tab.resource, project_id)
                            })
                            .map(|tab| tab.id.clone());
                        self.state.active_editor = Some(
                            restored_editor.unwrap_or_else(|| self.ensure_failure_inbox_editor()),
                        );
                        self.state.active_activity = self
                            .state
                            .active_editor
                            .as_ref()
                            .and_then(|id| self.state.editors.iter().find(|tab| &tab.id == id))
                            .map_or(super::ActivityId::Failures, |tab| {
                                tab.resource.kind().activity()
                            });
                    }
                } else {
                    self.state.scope = QueryScope {
                        project: requested.project,
                        ..QueryScope::default()
                    };
                    let active_is_scope_independent = self
                        .state
                        .active_editor
                        .as_ref()
                        .and_then(|id| self.state.editors.iter().find(|tab| &tab.id == id))
                        .is_some_and(|tab| resource_project_id(&tab.resource).is_none());
                    if !active_is_scope_independent {
                        self.state.active_editor = Some(self.ensure_failure_inbox_editor());
                        self.state.active_activity = super::ActivityId::Failures;
                    }
                }
            }
            ProjectScope::AllProjects => {
                self.state.scope = QueryScope {
                    project: ProjectScope::AllProjects,
                    ..QueryScope::default()
                };
                let active_is_scope_independent = self
                    .state
                    .active_editor
                    .as_ref()
                    .and_then(|id| self.state.editors.iter().find(|tab| &tab.id == id))
                    .is_some_and(|tab| resource_project_id(&tab.resource).is_none());
                if !active_is_scope_independent {
                    self.state.active_editor = Some(self.ensure_failure_inbox_editor());
                    self.state.active_activity = super::ActivityId::Failures;
                }
            }
        }
    }

    fn ensure_failure_inbox_editor(&mut self) -> EditorId {
        if let Some(tab) = self
            .state
            .editors
            .iter()
            .find(|tab| tab.resource == EditorResource::FailureInbox)
        {
            return tab.id.clone();
        }
        let tab = EditorTabState::new(EditorResource::FailureInbox, false);
        let id = tab.id.clone();
        self.state.editors.push(tab);
        id
    }
}

fn resource_visible_for_project(resource: &EditorResource, project_id: &str) -> bool {
    resource_project_id(resource).is_none_or(|resource_project| resource_project == project_id)
}

fn resource_project_id(resource: &EditorResource) -> Option<&str> {
    match resource {
        EditorResource::FailureInvestigation { project_id, .. }
        | EditorResource::FullTrace { project_id, .. }
        | EditorResource::EvalReview { project_id, .. }
        | EditorResource::Compare { project_id, .. } => Some(project_id),
        _ => None,
    }
}

const fn pane_focus(pane: PaneId) -> FocusRegion {
    match pane {
        PaneId::PrimarySidebar => FocusRegion::PrimarySidebar,
        PaneId::Inspector => FocusRegion::Inspector,
        PaneId::BottomPanel => FocusRegion::BottomPanel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::{ActivityId, EditorResource, ProjectScope, QueryScope, WorkbenchAction};

    #[test]
    fn editor_identity_is_stable_and_open_is_idempotent() {
        let mut model = WorkbenchModel::default();
        let resource = EditorResource::FailureInvestigation {
            project_id: "checkout".into(),
            group_id: "loop".into(),
        };
        model.apply(WorkbenchAction::OpenEditor {
            resource: resource.clone(),
            pinned: false,
        });
        model.apply(WorkbenchAction::OpenEditor {
            resource,
            pinned: true,
        });

        assert_eq!(model.state.editors.len(), 1);
        assert!(model.state.editors.last().is_some_and(|tab| tab.pinned));
    }

    #[test]
    fn one_preview_is_replaced_until_the_user_pins_it() {
        let mut model = WorkbenchModel::with_initial_editor(EditorResource::Runs);
        model.apply(WorkbenchAction::OpenEditor {
            resource: EditorResource::FullTrace {
                project_id: "checkout".into(),
                logical_trace_id: "trace-a".into(),
                revision: 1,
                origin: Default::default(),
                selected_span_id: None,
            },
            pinned: false,
        });
        let first_preview = model.state.active_editor.clone().expect("preview id");
        model.apply(WorkbenchAction::OpenEditor {
            resource: EditorResource::FullTrace {
                project_id: "checkout".into(),
                logical_trace_id: "trace-b".into(),
                revision: 1,
                origin: Default::default(),
                selected_span_id: None,
            },
            pinned: false,
        });
        assert_eq!(model.state.editors.len(), 1);
        assert!(
            !model
                .state
                .editors
                .iter()
                .any(|tab| tab.id == first_preview)
        );

        let second_preview = model.state.active_editor.clone().expect("preview id");
        model.apply(WorkbenchAction::PinEditor(second_preview));
        model.apply(WorkbenchAction::OpenEditor {
            resource: EditorResource::FullTrace {
                project_id: "checkout".into(),
                logical_trace_id: "trace-c".into(),
                revision: 1,
                origin: Default::default(),
                selected_span_id: None,
            },
            pinned: false,
        });
        assert_eq!(model.state.editors.len(), 2);
        assert_eq!(
            model.state.editors.iter().filter(|tab| !tab.pinned).count(),
            1
        );
    }

    #[test]
    fn initial_editor_does_not_leak_the_default_inbox() {
        let model = WorkbenchModel::with_initial_editor(EditorResource::Welcome);

        assert_eq!(model.state.editors.len(), 1);
        assert_eq!(model.state.editors[0].resource, EditorResource::Welcome);
        assert!(!model.state.editors[0].pinned);
    }

    #[test]
    fn restore_collapses_legacy_activity_tabs_into_one_preview() {
        let state = WorkbenchStateV1 {
            editors: vec![
                EditorTabState::new(EditorResource::Welcome, true),
                EditorTabState::new(EditorResource::Sources, true),
                EditorTabState::new(EditorResource::Runs, true),
                EditorTabState::new(EditorResource::Settings, true),
            ],
            active_editor: Some(EditorId::for_resource(&EditorResource::Runs)),
            ..Default::default()
        };

        let model = WorkbenchModel::restore(state, &BTreeSet::new(), EditorResource::Welcome);

        assert_eq!(model.state.editors.len(), 1);
        assert_eq!(model.state.editors[0].resource, EditorResource::Runs);
        assert!(!model.state.editors[0].pinned);
    }

    #[test]
    fn restore_discards_only_artifacts_from_missing_projects() {
        let mut state = WorkbenchStateV1::default();
        state.editors.push(EditorTabState::new(
            EditorResource::FailureInvestigation {
                project_id: "deleted".into(),
                group_id: "failure".into(),
            },
            true,
        ));
        state.active_editor = state.editors.last().map(|tab| tab.id.clone());

        let model = WorkbenchModel::restore(state, &BTreeSet::new(), EditorResource::Welcome);

        assert_eq!(model.state.editors.len(), 1);
        assert_eq!(model.state.editors[0].resource, EditorResource::Welcome);
        assert_eq!(
            model.state.active_editor,
            Some(model.state.editors[0].id.clone())
        );
    }

    #[test]
    fn scope_change_clears_cross_scope_bulk_selection() {
        let mut model = WorkbenchModel::default();
        model.apply(WorkbenchAction::ToggleFailureGroup("group-a".into()));
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("project-b".into()),
            ..QueryScope::default()
        }));

        assert!(model.state.bulk_selection.failure_group_ids.is_empty());
    }

    #[test]
    fn first_project_scope_keeps_the_active_global_setup_editor() {
        let mut model = WorkbenchModel::with_initial_editor(EditorResource::Sources);

        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("first-project".into()),
            ..QueryScope::default()
        }));

        assert_eq!(model.state.active_editor, Some(EditorId("sources".into())));
        assert_eq!(
            model.state.active_activity,
            crate::workbench::ActivityId::Sources
        );
        assert_eq!(
            model.state.scope.project,
            ProjectScope::Project("first-project".into())
        );
    }

    #[test]
    fn switching_projects_never_leaves_an_editor_from_the_previous_project_active() {
        let mut model = WorkbenchModel::with_initial_editor(EditorResource::Compare {
            project_id: "alpha".into(),
            comparison_id: "baseline-candidate".into(),
        });
        model.state.scope.project = ProjectScope::Project("alpha".into());

        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("beta".into()),
            ..QueryScope::default()
        }));

        let active = model
            .state
            .active_editor
            .as_ref()
            .and_then(|id| model.state.editors.iter().find(|tab| &tab.id == id))
            .expect("project switch keeps a usable editor");
        assert_eq!(active.resource, EditorResource::FailureInbox);
        assert_eq!(model.state.active_activity, ActivityId::Failures);
    }

    #[test]
    fn all_projects_scope_exits_project_specific_editors() {
        let mut model = WorkbenchModel::with_initial_editor(EditorResource::FullTrace {
            project_id: "alpha".into(),
            logical_trace_id: "trace".into(),
            revision: 1,
            origin: Default::default(),
            selected_span_id: None,
        });
        model.state.scope.project = ProjectScope::Project("alpha".into());

        model.apply(WorkbenchAction::SetScope(QueryScope::default()));

        let active = model
            .state
            .active_editor
            .as_ref()
            .and_then(|id| model.state.editors.iter().find(|tab| &tab.id == id))
            .expect("portfolio switch keeps a usable editor");
        assert_eq!(active.resource, EditorResource::FailureInbox);
        assert_eq!(model.state.active_activity, ActivityId::Failures);
    }

    #[test]
    fn project_switch_restores_filters_and_last_editor_per_project() {
        let mut model = WorkbenchModel::default();
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("alpha".into()),
            ..QueryScope::default()
        }));
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("alpha".into()),
            environment: Some("staging".into()),
            build: Some("build-7".into()),
            ..QueryScope::default()
        }));
        model.apply(WorkbenchAction::OpenEditor {
            resource: EditorResource::Runs,
            pinned: true,
        });
        model.apply(WorkbenchAction::SetPaneVisible {
            pane: PaneId::Inspector,
            visible: true,
        });
        model.apply(WorkbenchAction::ResizePane {
            pane: PaneId::Inspector,
            size: 420.,
        });
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("beta".into()),
            ..QueryScope::default()
        }));
        model.apply(WorkbenchAction::OpenEditor {
            resource: EditorResource::FailureInbox,
            pinned: true,
        });
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("alpha".into()),
            ..QueryScope::default()
        }));

        assert_eq!(model.state.scope.environment.as_deref(), Some("staging"));
        assert_eq!(model.state.scope.build.as_deref(), Some("build-7"));
        assert_eq!(
            model.state.active_activity,
            crate::workbench::ActivityId::Runs
        );
        assert_eq!(model.state.active_editor, Some(EditorId("runs".into())));
        assert!(model.state.panes.inspector_visible);
        assert_eq!(model.state.panes.inspector_width, 420.);
    }

    #[test]
    fn pane_sizes_are_bounded() {
        let mut model = WorkbenchModel::default();
        model.apply(WorkbenchAction::ResizePane {
            pane: PaneId::Inspector,
            size: 10_000.,
        });
        assert_eq!(model.state.panes.inspector_width, 640.);
    }

    #[test]
    fn appearance_preferences_are_applied_without_resetting_workspace_state() {
        let mut model = WorkbenchModel::default();
        let active_editor = model.state.active_editor.clone();
        model.apply(WorkbenchAction::SetAppearance(
            crate::workbench::AppearancePreferencesV1 {
                text_scale: crate::workbench::TextScale::Double,
                reduced_motion: true,
            },
        ));

        assert_eq!(
            model.state.appearance.text_scale,
            crate::workbench::TextScale::Double
        );
        assert!(model.state.appearance.reduced_motion);
        assert_eq!(model.state.active_editor, active_editor);
    }

    #[test]
    fn failure_views_are_saved_and_restored_per_project_scope() {
        let mut model = WorkbenchModel::default();
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("alpha".into()),
            ..QueryScope::default()
        }));
        let mut alpha = crate::workbench::FailureInboxPreferencesV1::default();
        alpha.current.sort = crate::workbench::FailureInboxSort::MostUnresolved;
        model.apply(WorkbenchAction::SetFailureInboxPreferences {
            scope_key: model.state.scope.preference_key(),
            preferences: alpha.clone(),
        });
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("beta".into()),
            ..QueryScope::default()
        }));
        assert_eq!(
            model.failure_inbox_preferences(),
            crate::workbench::FailureInboxPreferencesV1::default()
        );
        model.apply(WorkbenchAction::SetScope(QueryScope {
            project: ProjectScope::Project("alpha".into()),
            ..QueryScope::default()
        }));
        assert_eq!(model.failure_inbox_preferences(), alpha);
    }

    #[test]
    fn full_trace_is_a_stable_editor_distinct_from_its_investigation() {
        let mut model = WorkbenchModel::with_initial_editor(EditorResource::FailureInvestigation {
            project_id: "checkout".into(),
            group_id: "missing-resolution".into(),
        });
        let trace = EditorResource::FullTrace {
            project_id: "checkout".into(),
            logical_trace_id: "trace-1".into(),
            revision: 3,
            origin: crate::workbench::FullTraceOrigin::Runs,
            selected_span_id: None,
        };
        model.apply(WorkbenchAction::OpenEditor {
            resource: trace.clone(),
            pinned: true,
        });
        model.apply(WorkbenchAction::OpenEditor {
            resource: trace,
            pinned: true,
        });

        assert_eq!(model.state.editors.len(), 2);
        assert_eq!(
            model.state.editors[0].resource.kind(),
            crate::workbench::EditorKind::FailureInvestigation
        );
        assert_eq!(
            model.state.editors[1].resource.kind(),
            crate::workbench::EditorKind::FullTrace
        );
        assert_eq!(
            model.state.active_editor,
            Some(EditorId("full-trace:checkout:trace-1:3".into()))
        );
    }

    #[test]
    fn reopening_the_same_trace_updates_its_return_origin_without_duplicating_the_tab() {
        let trace = EditorResource::FullTrace {
            project_id: "checkout".into(),
            logical_trace_id: "trace-1".into(),
            revision: 3,
            origin: crate::workbench::FullTraceOrigin::Runs,
            selected_span_id: None,
        };
        let mut model = WorkbenchModel::with_initial_editor(trace.clone());
        model.apply(WorkbenchAction::OpenEditor {
            resource: EditorResource::FullTrace {
                project_id: "checkout".into(),
                logical_trace_id: "trace-1".into(),
                revision: 3,
                origin: crate::workbench::FullTraceOrigin::FailureInvestigation {
                    project_id: "checkout".into(),
                    group_id: "missing-resolution".into(),
                    finding_id: Some("finding-2".into()),
                    occurrence_offset: 100,
                    span_id: Some("span-9".into()),
                },
                selected_span_id: Some("span-9".into()),
            },
            pinned: true,
        });

        assert_eq!(model.state.editors.len(), 1);
        assert!(matches!(
            &model.state.editors[0].resource,
            EditorResource::FullTrace {
                origin: crate::workbench::FullTraceOrigin::FailureInvestigation {
                    occurrence_offset: 100,
                    span_id: Some(span_id),
                    ..
                },
                ..
            } if span_id == "span-9"
        ));
    }

    #[test]
    fn updating_full_trace_selection_does_not_change_its_return_origin() {
        let mut model = WorkbenchModel::with_initial_editor(EditorResource::FullTrace {
            project_id: "checkout".into(),
            logical_trace_id: "trace-1".into(),
            revision: 3,
            origin: crate::workbench::FullTraceOrigin::FailureInvestigation {
                project_id: "checkout".into(),
                group_id: "missing-resolution".into(),
                finding_id: Some("finding-2".into()),
                occurrence_offset: 0,
                span_id: Some("root".into()),
            },
            selected_span_id: Some("root".into()),
        });

        model.apply(WorkbenchAction::UpdateActiveFullTraceSelection(Some(
            "planner".into(),
        )));

        assert!(matches!(
            &model.state.editors[0].resource,
            EditorResource::FullTrace {
                origin: crate::workbench::FullTraceOrigin::FailureInvestigation {
                    span_id: Some(origin_span_id),
                    ..
                },
                selected_span_id: Some(selected_span_id),
                ..
            } if origin_span_id == "root" && selected_span_id == "planner"
        ));
    }
}

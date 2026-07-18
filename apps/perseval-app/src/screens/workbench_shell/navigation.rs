use super::*;

impl WorkbenchShell {
    fn request_visible_repaint(window: &mut Window) {
        window.refresh();
        window.request_animation_frame();
    }

    pub(super) fn open_activity(&mut self, activity: ActivityId, cx: &mut Context<Self>) {
        let resource = match activity {
            ActivityId::Failures => EditorResource::FailureInbox,
            ActivityId::Runs => EditorResource::Runs,
            ActivityId::Compare => EditorResource::CompareSetup,
            ActivityId::Evals => EditorResource::EvalQueue,
            ActivityId::Sources => EditorResource::Sources,
            ActivityId::Settings => EditorResource::Settings,
        };
        self.open_editor(resource, false);
        self.sync_failure_view(cx);
        self.persist();
        cx.notify();
        cx.refresh_windows();
    }

    pub(super) fn open_activity_in_window(
        &mut self,
        activity: ActivityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_activity(activity, cx);
        Self::request_visible_repaint(window);
    }

    pub(super) fn request_navigation_repaint(window: &mut Window) {
        Self::request_visible_repaint(window);
    }

    pub(super) fn activate_editor(&mut self, id: EditorId, cx: &mut Context<Self>) {
        self.model.apply(WorkbenchAction::ActivateEditor(id));
        self.record_active_editor();
        self.sync_failure_view(cx);
        self.persist();
        cx.notify();
        cx.refresh_windows();
    }

    pub(super) fn close_editor(&mut self, id: EditorId, cx: &mut Context<Self>) {
        if let Some(tab) = self
            .model
            .state
            .editors
            .iter()
            .find(|tab| tab.id == id)
            .cloned()
        {
            self.navigation.remember_closed(tab);
        }
        self.model.apply(WorkbenchAction::CloseEditor(id));
        self.record_active_editor();
        self.sync_failure_view(cx);
        self.persist();
        cx.notify();
        cx.refresh_windows();
    }

    pub(super) fn pin_editor(&mut self, id: EditorId, cx: &mut Context<Self>) {
        self.model.apply(WorkbenchAction::PinEditor(id));
        self.persist();
        cx.notify();
        cx.refresh_windows();
    }

    pub(super) fn open_editor(&mut self, resource: EditorResource, pinned: bool) {
        self.model
            .apply(WorkbenchAction::OpenEditor { resource, pinned });
        self.record_active_editor();
    }

    fn record_active_editor(&mut self) {
        if let Some(active) = self.model.state.active_editor.clone() {
            self.navigation.record(active);
        }
    }

    fn available_editor_ids(&self) -> BTreeSet<EditorId> {
        self.model
            .state
            .editors
            .iter()
            .filter(|tab| editor_visible_in_scope(&tab.resource, &self.model.state.scope.project))
            .map(|tab| tab.id.clone())
            .collect()
    }

    pub(super) fn navigate_back(&mut self, cx: &mut Context<Self>) {
        let available = self.available_editor_ids();
        if let Some(editor) = self.navigation.back(&available) {
            self.model.apply(WorkbenchAction::ActivateEditor(editor));
            self.sync_failure_view(cx);
            self.persist();
            cx.notify();
            cx.refresh_windows();
        }
    }

    pub(super) fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        let available = self.available_editor_ids();
        if let Some(editor) = self.navigation.forward(&available) {
            self.model.apply(WorkbenchAction::ActivateEditor(editor));
            self.sync_failure_view(cx);
            self.persist();
            cx.notify();
            cx.refresh_windows();
        }
    }

    pub(super) fn reopen_closed_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.navigation.reopen() else {
            return;
        };
        self.open_editor(tab.resource, tab.pinned);
        self.sync_failure_view(cx);
        self.persist();
        cx.notify();
        cx.refresh_windows();
    }
}

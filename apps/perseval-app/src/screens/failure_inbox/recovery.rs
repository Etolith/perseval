use super::*;

impl FailureInbox {
    pub(super) fn retry_current_view(&mut self, cx: &mut Context<Self>) {
        self.load_error = None;
        if self.full_trace {
            if let (Some(project_id), Some((trace_id, revision))) = (
                self.full_trace_project_id.clone(),
                self.full_trace_identity.clone(),
            ) {
                self.full_trace_tree.clear();
                self.show_full_trace(
                    &project_id,
                    &trace_id,
                    revision,
                    self.full_trace_origin.clone(),
                    self.focused_span_id.clone(),
                    cx,
                );
            }
        } else if self.showing_inbox {
            self.reload_groups(cx);
        } else if let Some((project_id, group_id)) =
            self.investigation_target.clone().or_else(|| {
                self.selected_group.as_ref().map(|group| {
                    (
                        group.summary.project_id.clone(),
                        group.summary.group_id.clone(),
                    )
                })
            })
        {
            self.request_group(
                &project_id,
                &group_id,
                self.occurrence_offset,
                self.selected_finding_id.clone(),
                self.focused_span_id.clone(),
                cx,
            );
        }
    }

    pub(super) fn dismiss_load_error(&mut self, cx: &mut Context<Self>) {
        self.load_error = None;
        cx.notify();
    }
}

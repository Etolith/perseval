use super::components::button;
use super::*;

impl Render for FailureInbox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_scale = self.text_scale;
        let width: f32 = window.viewport_size().width.into();
        let breakpoint = full_trace_breakpoint(width, text_scale);
        let compact = breakpoint == Breakpoint::Compact;
        let full_trace_compact = full_trace_uses_compact_layout(breakpoint, self.inspector_open);
        let compact_group_row_height = 112. * text_scale;
        let content = if self.batch_preview.is_some() {
            self.render_eval_batch_preview(compact, cx)
        } else if self.batch_loading {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(Theme::MUTED)
                .child("Selecting representative findings…")
        } else if self.full_trace {
            if compact && self.inspector_open {
                self.render_shared_inspector(true, cx)
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_full_trace(full_trace_compact, text_scale, cx))
                    .when(self.inspector_open, |layout| {
                        layout.child(self.render_shared_inspector(false, cx))
                    })
            }
        } else if self.showing_inbox {
            self.render_group_list(compact, compact_group_row_height, cx)
        } else if compact && self.group_details_open {
            self.render_group_detail(cx)
        } else if compact && self.inspector_open {
            self.render_shared_inspector(true, cx)
        } else if compact {
            self.render_evidence(true, cx)
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .when(self.group_details_open, |layout| {
                    layout.child(self.render_group_detail(cx))
                })
                .child(self.render_evidence(false, cx))
                .when(self.inspector_open, |layout| {
                    layout.child(self.render_shared_inspector(false, cx))
                })
        };
        div()
            .size_full()
            .key_context("FailureInbox")
            .track_focus(&self.focus_handle)
            .on_action(
                cx.listener(|this, _: &FocusNextFailureGroup, _, cx| {
                    this.move_primary_focus(1, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FocusPreviousFailureGroup, _, cx| {
                this.move_primary_focus(-1, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendNextFailureGroup, _, cx| {
                this.extend_group_selection(1, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendPreviousFailureGroup, _, cx| {
                this.extend_group_selection(-1, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenFocusedFailureGroup, _, cx| this.open_focused_group(cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleFocusedFailureGroup, _, cx| {
                this.toggle_focused_group(cx)
            }))
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .when_some(self.load_error.clone(), |root, error| {
                root.child(
                    div()
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .bg(Theme::DANGER_SURFACE)
                        .text_xs()
                        .text_color(Theme::RED)
                        .child(div().flex_1().min_w_0().child(error))
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    button("Retry", true)
                                        .id("retry-failure-view")
                                        .role(Role::Button)
                                        .aria_label("Retry the failed failure view request")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.retry_current_view(cx)
                                        })),
                                )
                                .child(
                                    button("Dismiss", false)
                                        .id("dismiss-failure-view-error")
                                        .role(Role::Button)
                                        .aria_label("Dismiss this error")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.dismiss_load_error(cx)
                                        })),
                                ),
                        ),
                )
            })
            .child(content)
    }
}

fn full_trace_breakpoint(width: f32, text_scale: f32) -> Breakpoint {
    Breakpoint::for_width(width / text_scale.clamp(1., 2.))
}

fn full_trace_uses_compact_layout(breakpoint: Breakpoint, inspector_open: bool) -> bool {
    breakpoint == Breakpoint::Compact || (breakpoint == Breakpoint::Standard && inspector_open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_trace_compacts_when_inspector_consumes_the_side_lane() {
        assert!(full_trace_uses_compact_layout(Breakpoint::Standard, true));
        assert!(!full_trace_uses_compact_layout(Breakpoint::Standard, false));
        assert!(!full_trace_uses_compact_layout(Breakpoint::Wide, true));
        assert!(full_trace_uses_compact_layout(Breakpoint::Compact, false));
    }

    #[test]
    fn double_text_scale_uses_the_effective_content_width() {
        assert_eq!(full_trace_breakpoint(1_320., 1.), Breakpoint::Standard);
        assert_eq!(full_trace_breakpoint(1_320., 2.), Breakpoint::Compact);
    }
}

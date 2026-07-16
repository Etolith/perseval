use super::*;

impl RunsScreen {
    pub(super) fn render_compact_row(
        &self,
        run: &RunSummary,
        index: usize,
        row_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let status_tint = if run.error_count > 0 {
            Theme::RED
        } else if run.finding_count > 0 {
            Theme::AMBER
        } else {
            Theme::GREEN
        };
        let status_label = if run.error_count > 0 {
            "ERRORS"
        } else if run.finding_count > 0 {
            "FINDINGS"
        } else {
            "CLEAN"
        };
        let selected = self
            .selected_runs
            .iter()
            .any(|candidate| candidate.logical_trace_id == run.logical_trace_id);
        let selected_run = run.clone();
        let title = run.title.clone();
        let metadata = [run.environment.as_deref(), run.build_id.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");

        div()
            .id(("run-row", index))
            .role(Role::Row)
            .aria_label(format!(
                "{}; {}; session {}; build {}; environment {}; {} spans; {} findings; {} errors",
                run.title,
                lifecycle_label(run.lifecycle),
                run.session_id.as_deref().unwrap_or("Unknown"),
                run.build_id.as_deref().unwrap_or("Unknown"),
                run.environment.as_deref().unwrap_or("Unknown"),
                run.span_count,
                run.finding_count,
                run.error_count
            ))
            .w_full()
            .h(px(row_height))
            .px_3()
            .py_3()
            .flex()
            .items_start()
            .gap_3()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .id(("select-run", index))
                    .role(Role::CheckBox)
                    .aria_label(format!("Select {} for comparison", run.title))
                    .aria_toggled(if selected {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .size(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(if selected { Theme::CYAN } else { Theme::BORDER })
                    .bg(if selected { Theme::SELECTED } else { Theme::BG })
                    .text_xs()
                    .text_color(Theme::CYAN)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_run_selection(selected_run.clone(), cx)
                    }))
                    .child(if selected { "✓" } else { "" }),
            )
            .child(
                div()
                    .id(("open-run", index))
                    .role(Role::Button)
                    .aria_label(format!("Open full trace for {}", run.title))
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_run(index, cx)))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .child(tag(status_label, status_tint)),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(Theme::DIM)
                            .text_ellipsis()
                            .child(short_id(&run.logical_trace_id).to_owned()),
                    )
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(tag(
                                identity_label(run.identity_quality),
                                identity_tint(run.identity_quality),
                            ))
                            .child(div().text_xs().text_color(Theme::MUTED).child(format!(
                                "{} spans · revision {}",
                                run.span_count, run.revision
                            )))
                            .when(!metadata.is_empty(), |row| {
                                row.child(div().text_xs().text_color(Theme::DIM).child(metadata))
                            }),
                    ),
            )
            .into_any_element()
    }
}

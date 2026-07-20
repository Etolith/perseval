use super::components::*;
use super::*;

impl FailureInbox {
    pub(super) fn render_full_trace(
        &self,
        compact: bool,
        text_scale: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let breadcrumb = self
            .evidence
            .as_ref()
            .map(|evidence| {
                format!(
                    "Failures / {} / {} / Full trace",
                    self.selected_group
                        .as_ref()
                        .and_then(|group| group.summary.presentation.as_ref())
                        .map(|presentation| presentation.title.clone())
                        .or_else(|| {
                            self.selected_group
                                .as_ref()
                                .and_then(|group| group.summary.detector_ids.first())
                                .map(|value| humanize(value))
                        })
                        .unwrap_or_else(|| "Failure".into()),
                    evidence.occurrence.run_title
                )
            })
            .unwrap_or_else(|| "Failures / Full trace".into());
        let evidence_ids = self
            .evidence
            .as_ref()
            .map(|evidence| evidence.evidence_span_ids.clone())
            .unwrap_or_default();
        let evidence_count = evidence_ids.len();
        let search = self.full_trace_search.read(cx).text().trim().to_lowercase();
        let filter_active = !search.is_empty() || self.full_trace_errors_only;
        let all_tree_rows = self.full_trace_tree.visible_rows();
        let loaded_tree_span_count = all_tree_rows
            .iter()
            .filter(|row| matches!(row, full_trace_tree::FullTraceListRow::Span(_)))
            .count();
        let visible_tree_rows = if filter_active {
            all_tree_rows
                .into_iter()
                .filter(|row| match row {
                    full_trace_tree::FullTraceListRow::Span(span) => {
                        (!self.full_trace_errors_only || span.status_code == 2)
                            && (search.is_empty() || span_matches_search(span, &search))
                    }
                    full_trace_tree::FullTraceListRow::LoadMore { .. }
                    | full_trace_tree::FullTraceListRow::Loading { .. } => true,
                })
                .collect::<Vec<_>>()
        } else {
            all_tree_rows
        };
        let matching_tree_span_count = visible_tree_rows
            .iter()
            .filter(|row| matches!(row, full_trace_tree::FullTraceListRow::Span(_)))
            .count();
        let loaded_timeline_rows = self.full_trace_timeline.loaded_rows();
        let filtered_timeline_rows = filter_active.then(|| {
            loaded_timeline_rows
                .iter()
                .filter(|span| {
                    (!self.full_trace_errors_only || span.status_code == 2)
                        && (search.is_empty() || span_matches_search(span, &search))
                })
                .cloned()
                .collect::<Vec<_>>()
        });
        let loaded_span_count = match self.full_trace_mode {
            FullTraceMode::Tree => loaded_tree_span_count,
            FullTraceMode::Timeline => self.full_trace_timeline.loaded_count(),
        };
        let matching_span_count = match self.full_trace_mode {
            FullTraceMode::Tree => matching_tree_span_count,
            FullTraceMode::Timeline => filtered_timeline_rows
                .as_ref()
                .map(Vec::len)
                .unwrap_or(loaded_span_count),
        };
        let trace_start = self.full_trace_start_unix_nano;
        let trace_end = self
            .full_trace_end_unix_nano
            .max(trace_start.saturating_add(1));
        let trace_duration = trace_end.saturating_sub(trace_start).max(1);
        let mode = self.full_trace_mode;
        let list = match mode {
            FullTraceMode::Tree => {
                let total = visible_tree_rows.len();
                uniform_list(
                    "full-trace-tree-spans",
                    total,
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        range
                            .filter_map(|index| {
                                visible_tree_rows
                                    .get(index)
                                    .cloned()
                                    .map(|row| (index, row))
                            })
                            .map(|(index, row)| match row {
                                full_trace_tree::FullTraceListRow::Span(span) => {
                                    let selected =
                                        this.focused_span_id.as_deref() == Some(&span.span_id);
                                    let expanded = this.full_trace_tree.is_expanded(&span.span_id);
                                    let search_match =
                                        !search.is_empty() && span_matches_search(&span, &search);
                                    full_trace_span_row(
                                        &span,
                                        FullTraceRowState::new(
                                            evidence_ids.contains(&span.span_id),
                                            search_match,
                                            selected,
                                            compact,
                                            52. * text_scale,
                                        ),
                                        expanded,
                                        cx,
                                    )
                                    .id(("full-trace-span", index))
                                    .into_any_element()
                                }
                                full_trace_tree::FullTraceListRow::LoadMore {
                                    parent_span_id,
                                    offset,
                                    depth,
                                } => div()
                                    .id(("full-trace-load-more", index))
                                    .role(Role::Button)
                                    .aria_label("Load more child spans")
                                    .tab_index(0)
                                    .focus_visible(|style| {
                                        style.border_2().border_color(Theme::FOCUS_RING)
                                    })
                                    .w_full()
                                    .h(px(40.))
                                    .pl(px(28. + FULL_TRACE_DEPTH_INDENT * depth as f32))
                                    .flex()
                                    .items_center()
                                    .text_xs()
                                    .text_color(Theme::CYAN)
                                    .cursor_pointer()
                                    .hover(|style| style.bg(Theme::ROW_HOVER))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.ensure_full_trace_tree_page(
                                            parent_span_id.clone(),
                                            offset,
                                            cx,
                                        )
                                    }))
                                    .child("Load more children…")
                                    .into_any_element(),
                                full_trace_tree::FullTraceListRow::Loading { depth } => div()
                                    .id(("full-trace-loading", index))
                                    .role(Role::Status)
                                    .aria_label("Loading child spans")
                                    .w_full()
                                    .h(px(40.))
                                    .pl(px(28. + FULL_TRACE_DEPTH_INDENT * depth as f32))
                                    .flex()
                                    .items_center()
                                    .text_xs()
                                    .text_color(Theme::DIM)
                                    .child("Loading children…")
                                    .into_any_element(),
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .w_full()
                .flex_1()
                .min_h_0()
                .px_4()
                .into_any_element()
            }
            FullTraceMode::Timeline => {
                let timeline = self.full_trace_timeline.clone();
                let total = filtered_timeline_rows
                    .as_ref()
                    .map(Vec::len)
                    .unwrap_or_else(|| timeline.total());
                uniform_list(
                    "full-trace-timeline-spans",
                    total,
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let span = filtered_timeline_rows
                                    .as_ref()
                                    .and_then(|rows| rows.get(index))
                                    .or_else(|| timeline.row(index));
                                if let Some(span) = span {
                                    let selected = this.focused_span_id.as_deref()
                                        == Some(span.span_id.as_str());
                                    let search_match =
                                        !search.is_empty() && span_matches_search(span, &search);
                                    full_trace_timeline_row(
                                        span,
                                        FullTraceRowState::new(
                                            evidence_ids.contains(&span.span_id),
                                            search_match,
                                            selected,
                                            compact,
                                            if compact {
                                                76. * text_scale
                                            } else {
                                                52. * text_scale
                                            },
                                        ),
                                        trace_start,
                                        trace_duration,
                                        cx,
                                    )
                                    .into_any_element()
                                } else {
                                    this.ensure_full_trace_timeline_page(index, cx);
                                    div()
                                        .id(("timeline-placeholder", index))
                                        .w_full()
                                        .h(px(if compact {
                                            72. * text_scale
                                        } else {
                                            44. * text_scale
                                        }))
                                        .border_b_1()
                                        .border_color(Theme::BORDER)
                                        .bg(Theme::PANEL_SURFACE)
                                        .child(
                                            div()
                                                .px_3()
                                                .py_2()
                                                .text_xs()
                                                .text_color(Theme::DIM)
                                                .child("Loading timeline page…"),
                                        )
                                        .into_any_element()
                                }
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .w_full()
                .flex_1()
                .min_h_0()
                .px_4()
                .into_any_element()
            }
        };
        let trace_status = if self.full_trace_loading {
            "Loading trace…".to_string()
        } else {
            let filtered = if filter_active {
                format!(" · {matching_span_count} of {loaded_span_count} loaded rows match")
            } else {
                format!(" · {loaded_span_count} rows loaded")
            };
            format!(
                "{} total spans{filtered} · {} evidence spans highlighted",
                self.full_trace_span_count, evidence_count
            )
        };
        let content = if self.full_trace_loading {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(Theme::MUTED)
                .child("Loading the first 500 spans…")
        } else if self.full_trace_span_count == 0 {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(Theme::DIM)
                .child("This trace has no spans.")
        } else {
            div().flex_1().min_h_0().flex().flex_col().child(list)
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .when(compact, |header| {
                        header.flex_col().items_start().gap_2().px_4().py_3()
                    })
                    .when(!compact, |header| {
                        header.h(px(52.)).items_center().gap_3().px_5()
                    })
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .child(
                        button(
                            match &self.full_trace_origin {
                                FullTraceOrigin::Runs => "← Back to runs",
                                FullTraceOrigin::FailureInvestigation { .. } => {
                                    "← Back to investigation"
                                }
                            },
                            false,
                        )
                        .id("back-to-failure")
                        .role(Role::Button)
                        .aria_label(match &self.full_trace_origin {
                            FullTraceOrigin::Runs => "Back to runs",
                            FullTraceOrigin::FailureInvestigation { .. } => {
                                "Back to originating investigation"
                            }
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.emit(FailureInboxEvent::ReturnFromFullTrace {
                                origin: this.full_trace_origin.clone(),
                            });
                        })),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(div().text_sm().text_color(Theme::MUTED).child(breadcrumb))
                            .child(
                                div()
                                    .id("full-trace-status")
                                    .role(Role::Status)
                                    .aria_label(trace_status.clone())
                                    .mt_1()
                                    .text_xs()
                                    .text_color(Theme::DIM)
                                    .child(trace_status),
                            ),
                    )
                    .child(
                        div()
                            .id("full-trace-view-modes")
                            .role(Role::TabList)
                            .aria_label("Trace view mode")
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .child(
                                button("Tree", self.full_trace_mode == FullTraceMode::Tree)
                                    .id("full-trace-tree-mode")
                                    .role(Role::Tab)
                                    .aria_label("Tree view")
                                    .aria_selected(self.full_trace_mode == FullTraceMode::Tree)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_full_trace_mode(FullTraceMode::Tree, cx)
                                    })),
                            )
                            .child(
                                button("Timeline", self.full_trace_mode == FullTraceMode::Timeline)
                                    .id("full-trace-timeline-mode")
                                    .role(Role::Tab)
                                    .aria_label("Chronological timeline view")
                                    .aria_selected(self.full_trace_mode == FullTraceMode::Timeline)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_full_trace_mode(FullTraceMode::Timeline, cx)
                                    })),
                            )
                            .child(
                                button("Inspector", self.inspector_open)
                                    .id("toggle-full-trace-inspector")
                                    .role(Role::Button)
                                    .aria_label("Toggle full trace inspector")
                                    .aria_toggled(if self.inspector_open {
                                        Toggled::True
                                    } else {
                                        Toggled::False
                                    })
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_inspector(cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .flex()
                    .when(compact, |bar| bar.flex_col().items_start().gap_2().py_3())
                    .when(!compact, |bar| bar.h(px(44.)).items_center().gap_2())
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL_ALT)
                    .child(
                        div()
                            .when(compact, |search| search.w_full())
                            .when(!compact, |search| search.w(px(320.)))
                            .h(px(32.))
                            .child(self.full_trace_search.clone()),
                    )
                    .when(self.full_trace_mode == FullTraceMode::Tree, |bar| {
                        bar.child(
                            button("Expand loaded branches", false)
                                .id("expand-loaded-trace-spans")
                                .role(Role::Button)
                                .aria_label(
                                    "Expand loaded parents and load their immediate children",
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.expand_all_loaded_trace_spans(cx)
                                })),
                        )
                    })
                    .child(
                        button("Errors only", self.full_trace_errors_only)
                            .id("full-trace-errors-only")
                            .role(Role::CheckBox)
                            .aria_label("Show errors only in cached trace pages")
                            .aria_toggled(if self.full_trace_errors_only {
                                Toggled::True
                            } else {
                                Toggled::False
                            })
                            .on_click(
                                cx.listener(|this, _, _, cx| this.toggle_full_trace_errors(cx)),
                            ),
                    )
                    .when(filter_active, |bar| {
                        bar.child(
                            button("Reset", false)
                                .id("reset-full-trace-filters")
                                .role(Role::Button)
                                .aria_label("Reset full trace filters")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.reset_full_trace_filters(cx)),
                                ),
                        )
                        .child(
                            div().text_xs().text_color(Theme::AMBER).child(
                                "Filtering cached pages; uncached matches load as you browse.",
                            ),
                        )
                    }),
            )
            .child(content)
    }
}

fn span_matches_search(span: &SpanRow, search: &str) -> bool {
    span.name.to_lowercase().contains(search)
        || span.span_id.to_lowercase().contains(search)
        || span.category.to_lowercase().contains(search)
        || span.status_message.to_lowercase().contains(search)
}

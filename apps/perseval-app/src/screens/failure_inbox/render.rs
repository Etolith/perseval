use super::components::*;
use super::*;

const FAILURE_COLUMNS: [DataColumn; 6] = [
    DataColumn::Flexible,
    DataColumn::Fixed(88.),
    DataColumn::Fixed(120.),
    DataColumn::Fixed(72.),
    DataColumn::Fixed(156.),
    DataColumn::Fixed(124.),
];

impl FailureInbox {
    fn render_group_row(
        &self,
        group: &FailureGroupSummary,
        index: usize,
        compact: bool,
        compact_row_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = group.group_id.clone();
        let project_id = group.project_id.clone();
        let toggle_id = group.group_id.clone();
        let toggle_project_id = group.project_id.clone();
        let open_id = group.group_id.clone();
        let open_project_id = group.project_id.clone();
        let failure_title = group
            .presentation
            .as_ref()
            .map(|presentation| presentation.title.clone())
            .unwrap_or_else(|| {
                humanize(
                    group
                        .detector_ids
                        .first()
                        .map(String::as_str)
                        .unwrap_or("failure"),
                )
            });
        let focused = self
            .focused_group
            .as_ref()
            .is_some_and(|(project_id, group_id)| {
                project_id == &group.project_id && group_id == &group.group_id
            });
        let selected = self.selected_group_ids.contains(&group.group_id);
        if compact {
            let detector = failure_title.clone();
            let operation = group.operation.as_ref().or(group.subject.as_ref()).cloned();
            return div()
                .id(("failure-group", index))
                .role(Role::Row)
                .aria_label(format!(
                    "{detector}; {:?}; {} examples; {} unresolved; {}",
                    group.severity,
                    group.occurrence_count,
                    group.unrecovered_count + group.unknown_recovery_count,
                    display_timestamp(&group.last_seen_at)
                ))
                .aria_selected(focused)
                .aria_row_index(index + 1)
                .tab_index(0)
                .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                // `uniform_list` needs a pixel-stable row extent. Text scales
                // inside this taller compact composition without making the
                // virtualized row measurement disappear at 200%.
                .h(px(compact_row_height))
                .w_full()
                .px_3()
                .py_3()
                .flex()
                .items_start()
                .gap_3()
                .border_b_1()
                .border_color(Theme::BORDER)
                .bg(if focused {
                    Theme::PANEL_ALT
                } else {
                    Theme::PANEL
                })
                .cursor_pointer()
                .hover(|style| style.bg(Theme::PANEL_ALT))
                .on_action(cx.listener(|this, _: &FocusNextFailureGroup, _, cx| {
                    this.move_primary_focus(1, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusPreviousFailureGroup, _, cx| {
                    this.move_primary_focus(-1, cx)
                }))
                .on_action(cx.listener(|this, _: &ExtendNextFailureGroup, _, cx| {
                    this.extend_group_selection(1, cx)
                }))
                .on_action(cx.listener(|this, _: &ExtendPreviousFailureGroup, _, cx| {
                    this.extend_group_selection(-1, cx)
                }))
                .on_action(cx.listener(|this, _: &OpenFocusedFailureGroup, _, cx| {
                    this.open_focused_group(cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleFocusedFailureGroup, _, cx| {
                    this.toggle_focused_group(cx)
                }))
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        if event.click_count() > 1 {
                            this.open_group(project_id.clone(), id.clone(), cx);
                        } else {
                            let modifiers = event.modifiers();
                            this.interact_with_group(
                                project_id.clone(),
                                id.clone(),
                                modifiers.shift,
                                modifiers.secondary(),
                                false,
                                cx,
                            );
                        }
                    }),
                )
                .child(
                    div()
                        .id(("select-failure-group", index))
                        .role(Role::CheckBox)
                        .aria_label(format!("Select {detector} for eval generation"))
                        .aria_toggled(if selected {
                            Toggled::True
                        } else {
                            Toggled::False
                        })
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(if selected { Theme::CYAN } else { Theme::BORDER })
                        .bg(if selected { Theme::CYAN } else { Theme::BG })
                        .text_color(Theme::TEXT_ON_ACCENT)
                        .child(if selected { "✓" } else { "" })
                        .when(self.can_generate_eval(), |checkbox| {
                            checkbox.cursor_pointer().on_click(cx.listener(
                                move |this, event: &gpui::ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.focus_handle.focus(window, cx);
                                    let modifiers = event.modifiers();
                                    this.interact_with_group(
                                        toggle_project_id.clone(),
                                        toggle_id.clone(),
                                        modifiers.shift,
                                        modifiers.secondary(),
                                        true,
                                        cx,
                                    )
                                },
                            ))
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(detector),
                                )
                                .child(tag(
                                    &format!("{:?}", group.severity),
                                    severity_color(group.severity),
                                )),
                        )
                        .when_some(operation, |row, value| {
                            row.child(div().mt_1().text_xs().text_color(Theme::MUTED).child(value))
                        })
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(Theme::DIM)
                                .child(format!("{} examples", group.occurrence_count))
                                .child(format!(
                                    "{} unresolved",
                                    group.unrecovered_count + group.unknown_recovery_count
                                ))
                                .child(display_timestamp(&group.last_seen_at))
                                .child(
                                    button("Investigate", false)
                                        .id(("investigate-failure-group", index))
                                        .role(Role::Button)
                                        .aria_label(format!("Investigate {failure_title}"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.open_group(
                                                open_project_id.clone(),
                                                open_id.clone(),
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                )
                .into_any_element();
        }
        let cells = vec![
            div()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(failure_title.clone()),
                )
                .when_some(
                    group.operation.as_ref().or(group.subject.as_ref()),
                    |row, value| {
                        row.child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(value.clone()),
                        )
                    },
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(Theme::DIM)
                        .child("Open →"),
                )
                .into_any_element(),
            tag(
                &format!("{:?}", group.severity),
                severity_color(group.severity),
            )
            .into_any_element(),
            trend_sparkline(&group.occurrence_trend, group.recurrence.as_ref()).into_any_element(),
            div()
                .text_xs()
                .child(format!("{}", group.occurrence_count))
                .into_any_element(),
            div()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(format!(
                    "{} unreviewed · {} dismissed",
                    group.unreviewed_count, group.dismissed_count
                ))
                .into_any_element(),
            div()
                .text_xs()
                .text_color(Theme::DIM)
                .child(display_timestamp(&group.last_seen_at))
                .into_any_element(),
        ];
        div()
            .id(("failure-group", index))
            .w_full()
            .role(Role::Row)
            .aria_label(format!(
                "{}; {:?}; {} examples; {} unresolved; {}",
                failure_title,
                group.severity,
                group.occurrence_count,
                group.unrecovered_count + group.unknown_recovery_count,
                display_timestamp(&group.last_seen_at)
            ))
            .aria_selected(focused)
            .aria_row_index(index + 1)
            .tab_index(0)
            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
            .h(px(72.))
            .px_6()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(if focused {
                Theme::PANEL_ALT
            } else {
                Theme::PANEL
            })
            .cursor_pointer()
            .hover(|style| style.bg(Theme::PANEL_ALT))
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
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    let modifiers = event.modifiers();
                    if modifiers.shift || modifiers.secondary() {
                        this.interact_with_group(
                            project_id.clone(),
                            id.clone(),
                            modifiers.shift,
                            modifiers.secondary(),
                            false,
                            cx,
                        );
                    } else {
                        this.open_group(project_id.clone(), id.clone(), cx);
                    }
                }),
            )
            .child(
                div()
                    .id(("select-failure-group", index))
                    .role(Role::CheckBox)
                    .aria_label(format!(
                        "Select {} for eval generation",
                        humanize(
                            group
                                .detector_ids
                                .first()
                                .map(String::as_str)
                                .unwrap_or("failure")
                        )
                    ))
                    .aria_toggled(if selected {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if selected { Theme::CYAN } else { Theme::BORDER })
                    .bg(if selected { Theme::CYAN } else { Theme::BG })
                    .text_color(Theme::TEXT_ON_ACCENT)
                    .child(if selected { "✓" } else { "" })
                    .when(self.can_generate_eval(), |checkbox| {
                        checkbox.cursor_pointer().on_click(cx.listener(
                            move |this, event: &gpui::ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.focus_handle.focus(window, cx);
                                let modifiers = event.modifiers();
                                this.interact_with_group(
                                    toggle_project_id.clone(),
                                    toggle_id.clone(),
                                    modifiers.shift,
                                    modifiers.secondary(),
                                    true,
                                    cx,
                                )
                            },
                        ))
                    }),
            )
            .child(data_columns(&FAILURE_COLUMNS, cells))
            .into_any_element()
    }

    pub(super) fn render_group_list(
        &self,
        compact: bool,
        compact_row_height: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let group_count = self.groups.len();
        let selected_groups = self
            .groups
            .iter()
            .filter(|group| self.selected_group_ids.contains(&group.group_id))
            .collect::<Vec<_>>();
        let selected_findings = selected_groups
            .iter()
            .map(|group| group.occurrence_count)
            .sum::<u64>();
        let eligible_runs = selected_groups
            .iter()
            .find_map(|group| group.recurrence.as_ref())
            .map(|series| {
                series
                    .buckets
                    .iter()
                    .map(|bucket| bucket.eligible_run_count)
                    .sum::<u64>()
            });
        let list = if self.groups.is_empty() {
            let mut empty = div()
                .p_5()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(div().text_sm().text_color(Theme::MUTED).child(
                    if self.has_active_group_filters() {
                        "No failure groups match the current filters."
                    } else if self.health.analysis_pending > 0 || self.health.analysis_running > 0 {
                        "Finalized traces are being analyzed. Groups will appear here."
                    } else if self.run_count == 0 {
                        "No traces have arrived in this scope yet."
                    } else {
                        "No actionable groups. Error spans remain in Runs when requiredness or final-outcome evidence is missing; open a run to review its telemetry gaps."
                    },
                ));
            if self.has_active_group_filters() {
                empty = empty.child(
                    button("Reset filters", false)
                        .id("empty-reset-filters")
                        .role(Role::Button)
                        .aria_label("Reset failure filters")
                        .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
                );
            } else if self.health.analysis_pending == 0 && self.health.analysis_running == 0 {
                empty = if self.run_count == 0 {
                    empty.child(
                        button("Add traces", false)
                            .id("empty-open-sources")
                            .role(Role::Button)
                            .aria_label("Open Sources to add traces")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(FailureInboxEvent::OpenSources)),
                            ),
                    )
                } else {
                    empty.child(
                        button("Inspect runs", false)
                            .id("empty-open-runs")
                            .role(Role::Button)
                            .aria_label("Inspect analyzed runs")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(FailureInboxEvent::OpenRuns)),
                            ),
                    )
                };
            }
            div()
                .id("failure-groups-empty")
                .role(Role::Table)
                .aria_label("Failure groups")
                .aria_row_count(0)
                .flex_1()
                .overflow_y_scroll()
                .child(empty)
                .into_any_element()
        } else {
            div()
                .id("failure-groups-table")
                .role(Role::Table)
                .aria_label("Failure groups")
                .aria_row_count(group_count)
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    uniform_list(
                        "failure-groups-scroll",
                        group_count,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            range
                                .filter_map(|index| {
                                    this.groups.get(index).cloned().map(|group| (index, group))
                                })
                                .map(|(index, group)| {
                                    this.render_group_row(
                                        &group,
                                        index,
                                        compact,
                                        compact_row_height,
                                        cx,
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.group_scroll)
                    .w_full()
                    .flex_1()
                    .min_h_0(),
                )
                .into_any_element()
        };
        div()
            .size_full()
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::PANEL)
            .when(compact, |table| {
                table.child(self.render_compact_group_header(cx))
            })
            .when(!compact, |table| {
                table
                    .child(
                        data_page_header(
                            "Failure Inbox",
                            "Ranked groups of related failures, ready for investigation.",
                            format!(
                                "{} of {} groups loaded · {} selected{}",
                                self.groups.len(),
                                self.group_total,
                                self.selected_group_ids.len(),
                                if self.groups_loading {
                                    " · Updating…"
                                } else {
                                    ""
                                }
                            ),
                        )
                        .child(
                            data_page_toolbar()
                                .child(
                                    button(
                                        &match self.active_group_filter_count() {
                                            0 => "Filters".into(),
                                            count => format!("Filters · {count}"),
                                        },
                                        self.active_group_filter_count() > 0,
                                    )
                                    .id("failure-filters")
                                    .role(Role::Button)
                                    .aria_label(format!(
                                        "Open failure filters; {} active",
                                        self.active_group_filter_count()
                                    ))
                                    .aria_expanded(
                                        self.open_filter_menu == Some(InboxFilterMenu::Filters),
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.toggle_filter_menu(InboxFilterMenu::Filters, cx)
                                        },
                                    )),
                                )
                                .child(div().w(px(260.)).child(self.search_input.clone()))
                                .child(
                                    button(
                                        &format!("Organize · {}", self.current_sort().label()),
                                        self.current_sort() != FailureInboxSort::Priority
                                            || self.preferences.active_saved_view_id.is_some(),
                                    )
                                    .id("organize-failure-inbox")
                                    .role(Role::Button)
                                    .aria_label("Open failure sorting and saved views")
                                    .aria_expanded(
                                        self.open_filter_menu == Some(InboxFilterMenu::Organize),
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.toggle_filter_menu(InboxFilterMenu::Organize, cx)
                                        },
                                    )),
                                ),
                        )
                        .when_some(self.render_open_filter_menu(cx), |header, menu| {
                            header.child(menu)
                        }),
                    )
                    .child(data_table_header(
                        20.,
                        data_columns(
                            &FAILURE_COLUMNS,
                            [
                                "Failure group",
                                "Severity",
                                "Trend",
                                "Examples",
                                "Review",
                                "Last seen",
                            ]
                            .into_iter()
                            .map(|label| div().child(label).into_any_element())
                            .collect(),
                        ),
                    ))
            })
            .when(!selected_groups.is_empty(), |table| {
                table.child(
                    div()
                        .px_4()
                        .py_2()
                        .flex()
                        .when(compact, |summary| summary.flex_col().items_start())
                        .when(!compact, |summary| summary.items_center().justify_between())
                        .gap_2()
                        .border_b_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::ROW_SELECTED)
                        .child(div().text_xs().text_color(Theme::TEXT).child(format!(
                                    "{} groups selected · {} findings · {} representatives{}",
                                    selected_groups.len(),
                                    selected_findings,
                                    selected_groups.len(),
                                    eligible_runs
                                        .map(|runs| format!(" · {runs} eligible runs in scope"))
                                        .unwrap_or_default()
                                )))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .when(selected_groups.len() == 1, |actions| {
                                    actions.child(
                                        button("Open investigation", false)
                                            .id("open-selected-investigation")
                                            .role(Role::Button)
                                            .aria_label("Open the selected failure investigation")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.open_only_selected_group(cx)
                                            })),
                                    )
                                })
                                .child(
                                    button_state(
                                        "Create evals from selected groups",
                                        true,
                                        self.can_generate_eval(),
                                    )
                                    .id("create-evals-selection-summary")
                                    .role(Role::Button)
                                    .aria_label(format!(
                                        "Create eval candidates from {} selected failure groups",
                                        selected_groups.len()
                                    ))
                                    .when(
                                        self.can_generate_eval(),
                                        |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.preview_selected_groups(cx)
                                            }))
                                        },
                                    ),
                                ),
                        ),
                )
            })
            .child(list)
            .when((self.groups.len() as u64) < self.group_total, |view| {
                view.child(
                    div()
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_t_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::PANEL_ALT)
                        .child(
                            div()
                                .id("failure-group-page-status")
                                .role(Role::Status)
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(format!(
                                    "{} of {} failure groups loaded",
                                    self.groups.len(),
                                    self.group_total
                                )),
                        )
                        .child(
                            button_state("Load more", false, !self.groups_loading)
                                .id("load-more-failure-groups")
                                .role(Role::Button)
                                .aria_label("Load the next failure groups")
                                .when(!self.groups_loading, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| this.load_more_groups(cx)),
                                    )
                                }),
                        ),
                )
            })
    }

    pub(super) fn render_group_detail(&self, cx: &mut Context<Self>) -> Div {
        let mut body = div()
            .id("group-detail-scroll")
            .flex_1()
            .overflow_y_scroll()
            .p_5();
        if let Some(detail) = &self.selected_group {
            let presentation = detail.summary.presentation.as_ref();
            let title = presentation
                .map(|presentation| presentation.title.clone())
                .unwrap_or_else(|| {
                    humanize(
                        detail
                            .summary
                            .detector_ids
                            .first()
                            .map(String::as_str)
                            .unwrap_or("failure"),
                    )
                });
            body = body
                .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                .when_some(detail.summary.operation.as_ref().or(detail.summary.subject.as_ref()), |row, value| row.child(div().mt_1().text_sm().text_color(Theme::CYAN).child(value.clone())))
                .when_some(presentation, |row, presentation| {
                    row.child(
                        div()
                            .mt_5()
                            .p_3()
                            .rounded_sm()
                            .bg(Theme::INSET_SURFACE)
                            .text_sm()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(presentation.diagnosis.clone()))
                            .child(div().mt_3().text_xs().text_color(Theme::MUTED).child(format!("Expected · {}", presentation.expected_behavior)))
                            .child(div().mt_2().text_xs().text_color(Theme::MUTED).child(format!("Observed · {}", presentation.observed_behavior)))
                            .child(div().mt_2().text_xs().text_color(Theme::MUTED).child(presentation.recovery_summary.clone()))
                            .when_some(presentation.caveat.as_ref(), |card, caveat| card.child(div().mt_2().text_xs().text_color(Theme::AMBER).child(caveat.clone())))
                            .child(div().mt_3().text_xs().text_color(Theme::CYAN).child(format!("Next · {}", presentation.remediation_hint))),
                    )
                })
                .when(presentation.is_none(), |row| row.child(div().mt_5().p_3().rounded_sm().bg(Theme::INSET_SURFACE).text_sm().child(detail.explanation.clone())))
                .child(kv("Failure signature", &detail.summary.failure_signature))
                .child(kv("Detector", &detail.detector_versions.join(", ")))
                .child(kv("Adapter", &detail.adapter_versions.join(", ")))
                .when(!detail.summary.feature_similarity_cohorts.is_empty(), |row| {
                    row.child(kv(
                        "Feature similarity",
                        &detail
                            .summary
                            .feature_similarity_cohorts
                            .iter()
                            .map(|cohort| {
                                format!(
                                    "{} ({} related, {} novel; {}; {} / {})",
                                    cohort.cluster_id,
                                    cohort.member_count,
                                    cohort.novelty_count,
                                    cohort.method,
                                    cohort.embedding_provider.as_deref().unwrap_or("unknown"),
                                    cohort.embedding_model.as_deref().unwrap_or("unknown")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                    ))
                })
                .child(kv("First / last", &format!("{}  →  {}", detail.summary.first_seen_at, detail.summary.last_seen_at)))
                .child(kv("Recovery", &format!("{} recovered · {} unrecovered · {} unknown", detail.summary.recovered_count, detail.summary.unrecovered_count, detail.summary.unknown_recovery_count)))
                .when(!detail.telemetry_gaps.is_empty(), |row| row.child(div().mt_4().p_3().rounded_sm().bg(Theme::WARNING_SURFACE).text_xs().text_color(Theme::AMBER).child("⚠ Retry safety and state verification were not reported. This finding may require human review.")))
                .child(
                    div()
                        .mt_6()
                        .mb_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .child("Occurrences"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(Theme::DIM)
                                .child("Unrecovered, then newest"),
                        ),
                );
            for (index, occurrence) in self.occurrences.iter().enumerate() {
                let finding_id = occurrence.finding.finding_id.clone();
                let selected = self.selected_finding_id.as_deref() == Some(&finding_id);
                let build = occurrence
                    .scope
                    .criteria
                    .build_id
                    .as_deref()
                    .unwrap_or("Unknown build");
                let session = occurrence
                    .scope
                    .criteria
                    .session_id
                    .as_deref()
                    .unwrap_or("Unknown session");
                let selection_reason = match occurrence.finding.recovery {
                    RecoveryStatus::Unrecovered => "Unrecovered example",
                    RecoveryStatus::Recovered => "Recovered contrast",
                    RecoveryStatus::Unknown => "Recovery needs review",
                };
                let comparison_base = self.is_comparison_base(&finding_id);
                let comparison_target = self.is_compatible_comparison_target(occurrence);
                let comparison_target_id = finding_id.clone();
                body = body.child(
                    div()
                        .id(("occurrence", index))
                        .role(Role::ListBoxOption)
                        .aria_label(format!(
                            "{}; revision {}; build {}; session {}; {}; {:?}",
                            occurrence.run_title,
                            occurrence.revision,
                            build,
                            session,
                            selection_reason,
                            occurrence.analysis_status
                        ))
                        .aria_selected(selected)
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                        .p_3()
                        .border_b_1()
                        .border_color(Theme::BORDER)
                        .bg(if selected { Theme::SELECTED_SUBTLE } else { Theme::BG })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_occurrence(finding_id.clone(), cx)
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(occurrence.run_title.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(comparison_base, |row| {
                                            row.child(
                                                div()
                                                    .px_2()
                                                    .py_1()
                                                    .rounded_sm()
                                                    .bg(Theme::SELECTED)
                                                    .text_xs()
                                                    .text_color(Theme::CYAN)
                                                    .child("Current"),
                                            )
                                        })
                                        .when(
                                            self.compare_base_finding_id.is_some()
                                                && !comparison_base,
                                            |row| {
                                                row.child(
                                                    button_state(
                                                        "Compare",
                                                        true,
                                                        comparison_target,
                                                    )
                                                    .id(("compare-occurrence", index))
                                                    .role(Role::Button)
                                                    .aria_label(if comparison_target {
                                                        "Compare this example with the current example"
                                                    } else {
                                                        "This example belongs to the same trace revision and cannot be compared"
                                                    })
                                                    .when(comparison_target, |button| {
                                                        button.on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.compare_with_occurrence(
                                                                    &comparison_target_id,
                                                                    cx,
                                                                )
                                                            },
                                                        ))
                                                    }),
                                                )
                                            },
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(Theme::DIM)
                                                .child(occurrence.finding.created_at.clone()),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(format!(
                                    "rev {} · {} · {} · {} · {:?}",
                                    occurrence.revision,
                                    build,
                                    session,
                                    selection_reason,
                                    occurrence.analysis_status
                                )),
                        ),
                );
            }
            body = body.child(
                div()
                    .mt_3()
                    .text_xs()
                    .text_color(Theme::DIM)
                    .child(format!(
                        "Showing examples {}–{} of {}. Use the pinned controls in Evidence to move one example at a time.",
                        if self.occurrences.is_empty() {
                            0
                        } else {
                            self.occurrence_offset + 1
                        },
                        self.occurrence_offset + self.occurrences.len() as u64,
                        detail.summary.occurrence_count
                    )),
            );
        } else {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(Theme::DIM)
                    .child("Select a failure group."),
            );
        }
        div()
            .w(relative(0.36))
            .min_w(px(360.))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(Theme::BORDER)
            .bg(Theme::BG)
            .child(
                div()
                    .h(px(48.))
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .child(if self.compare_base_finding_id.is_some() {
                        "CHOOSE AN EXAMPLE TO COMPARE"
                    } else {
                        "FAILURE DETAILS & EXAMPLES"
                    })
                    .when(self.compare_base_finding_id.is_some(), |header| {
                        header.child(
                            button("Cancel compare", false)
                                .id("cancel-example-comparison")
                                .role(Role::Button)
                                .aria_label("Cancel example comparison")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.cancel_compare_examples(cx)),
                                ),
                        )
                    })
                    .child(
                        button("Close", false)
                            .id("close-failure-details")
                            .role(Role::Button)
                            .aria_label("Close failure details")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_group_details(cx))),
                    ),
            )
            .child(body)
    }
}

pub(super) fn short_hash(value: &str) -> &str {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    value.get(..value.len().min(12)).unwrap_or(value)
}

fn display_timestamp(value: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            let nanoseconds = value.parse::<u64>().ok()?;
            let seconds = (nanoseconds / 1_000_000_000) as i64;
            let subsecond = (nanoseconds % 1_000_000_000) as u32;
            chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, subsecond)
        });
    parsed
        .map(|timestamp| timestamp.format("%b %d · %H:%M UTC").to_string())
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod timestamp_tests {
    use super::display_timestamp;

    #[test]
    fn otlp_nanoseconds_and_rfc3339_are_humanized() {
        assert_eq!(
            display_timestamp("1767225600000000000"),
            "Jan 01 · 00:00 UTC"
        );
        assert_eq!(
            display_timestamp("2026-07-12T14:30:00Z"),
            "Jul 12 · 14:30 UTC"
        );
        assert_eq!(display_timestamp("unknown"), "unknown");
    }
}

use super::components::*;
use super::*;

fn diagnosis_fact(label: &str, value: &str, tint: Rgba) -> Div {
    div()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(tint)
                .child(label.to_uppercase()),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(value.to_string()),
        )
}

struct EvidenceGraphRow<'a> {
    index: usize,
    total: usize,
    role: &'a str,
    explanation: &'a str,
    highlighted: bool,
    selected: bool,
    compact: bool,
}

fn evidence_graph_row(span: &SpanRow, row: EvidenceGraphRow<'_>) -> Div {
    let EvidenceGraphRow {
        index,
        total,
        role,
        explanation,
        highlighted,
        selected,
        compact,
    } = row;
    let status_label = if span.status_code == 2 {
        "Error"
    } else if span.status_code == 1 {
        "OK"
    } else {
        "Unset"
    };
    let execution_role = full_trace_role(span);
    let execution_badge = execution_tag(&execution_role, execution_role_for_span(span));
    let supporting_tint = if selected || highlighted {
        Theme::MUTED
    } else {
        Theme::DIM
    };
    let span_summary = format!(
        "{status_label} · {} · {:.1} ms",
        span.category,
        span.duration_nano as f64 / 1_000_000.
    );
    div()
        .w_full()
        .min_h(px(64.))
        .flex()
        .items_stretch()
        .border_b_1()
        .border_color(Theme::BORDER)
        .bg(if selected {
            Theme::SELECTED
        } else if highlighted {
            Theme::WARNING_SURFACE
        } else {
            Theme::BG
        })
        .child(
            div()
                .w(px(38.))
                .flex_none()
                .relative()
                .flex()
                .justify_center()
                .child(
                    div()
                        .mt_3()
                        .size(px(20.))
                        .rounded_full()
                        .border_1()
                        .border_color(if highlighted {
                            Theme::AMBER
                        } else {
                            Theme::BORDER
                        })
                        .bg(Theme::PANEL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(if highlighted {
                            Theme::AMBER
                        } else {
                            Theme::DIM
                        })
                        .child((index + 1).to_string()),
                )
                .when(index + 1 < total, |rail| {
                    rail.child(
                        div()
                            .absolute()
                            .top(px(32.))
                            .bottom_0()
                            .w(px(1.))
                            .bg(Theme::BORDER),
                    )
                }),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .py_3()
                .pr_3()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(span.name.clone()),
                        )
                        .child(execution_badge)
                        .child(if role == "Context" {
                            tag(role, Theme::DIM)
                        } else {
                            execution_tag(role, ExecutionRole::Evidence)
                        }),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(supporting_tint)
                        .child(explanation.to_string()),
                )
                .when(compact, |content| {
                    content.child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(if span.status_code == 2 {
                                Theme::RED
                            } else {
                                supporting_tint
                            })
                            .child(span_summary.clone()),
                    )
                }),
        )
        .when(!compact, |row| {
            row.child(
                div()
                    .w(px(170.))
                    .flex_none()
                    .py_3()
                    .pr_3()
                    .text_right()
                    .text_xs()
                    .text_color(if span.status_code == 2 {
                        Theme::RED
                    } else {
                        supporting_tint
                    })
                    .child(span_summary),
            )
        })
}

impl FailureInbox {
    pub(super) fn render_evidence(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        if self.investigation_loading || self.evidence_loading {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(Theme::PANEL_SURFACE)
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                    if self.investigation_loading {
                        "Loading failure group…"
                    } else {
                        "Loading execution evidence…"
                    },
                ))
                .child(div().text_xs().text_color(Theme::MUTED).child(
                    "The workbench remains responsive while Perseval reads the trace index.",
                ));
        }
        let toolbar = self.evidence.as_ref().map(|evidence| {
            let previous_enabled = self.can_navigate_occurrence(false);
            let next_enabled = self.can_navigate_occurrence(true);
            let position = self.occurrence_position().unwrap_or_default();
            let total = self
                .selected_group
                .as_ref()
                .map(|group| group.summary.occurrence_count)
                .unwrap_or_default();
            let position_label = if total == 1 {
                "Only example".to_string()
            } else {
                format!("Example {position} of {total}")
            };
            let navigation_hint = if total <= 1 {
                Some("No other examples in this group")
            } else if !previous_enabled {
                Some("First example")
            } else if !next_enabled {
                Some("Last example")
            } else {
                None
            };
            let failure_title = evidence
                .presentation
                .as_ref()
                .map(|presentation| presentation.title.clone())
                .or_else(|| {
                    self.selected_group
                        .as_ref()
                        .and_then(|group| group.summary.detector_ids.first())
                        .map(|detector| humanize(detector))
                })
                .unwrap_or_else(|| "Failure investigation".into());
            let failure_severity = self
                .selected_group
                .as_ref()
                .map(|group| format!("{:?}", group.summary.severity))
                .unwrap_or_default();
            let current_disposition = evidence
                .occurrence
                .disposition
                .as_ref()
                .filter(|_| !evidence.occurrence.disposition_stale)
                .map(|disposition| disposition.state);
            let review_label = if evidence.occurrence.disposition_stale {
                Some(("Review stale", Theme::AMBER))
            } else {
                current_disposition.map(|state| match state {
                    FindingDispositionStateV1::Confirmed => ("Confirmed", Theme::GREEN),
                    FindingDispositionStateV1::Dismissed => ("Dismissed", Theme::MUTED),
                    FindingDispositionStateV1::NeedsContext => ("Needs context", Theme::AMBER),
                })
            };
            div()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(Theme::BORDER)
                .bg(Theme::PANEL_ALT)
                .child(
                    div()
                        .flex()
                        .when(compact, |row| row.flex_col().items_start())
                        .when(!compact, |row| row.items_center().justify_between())
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(format!(
                                                    "{failure_title} · {failure_severity}"
                                                )),
                                        )
                                        .when_some(review_label, |row, (label, tint)| {
                                            row.child(tag(label, tint))
                                        }),
                                )
                                .child(div().mt_1().text_xs().text_color(Theme::MUTED).child(
                                    format!(
                                        "{} · revision {}",
                                        evidence.occurrence.run_title, evidence.occurrence.revision
                                    ),
                                )),
                        )
                        .child(
                            div()
                                .id("occurrence-position")
                                .role(Role::Status)
                                .aria_label(position_label.clone())
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .when(compact, |position| position.mt_2())
                                .child(position_label),
                        ),
                )
                .child(
                    div()
                        .mt_3()
                        .flex()
                        .when(compact, |row| row.flex_col().items_start())
                        .when(!compact, |row| row.items_center().justify_between())
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    button_state("← Previous", false, previous_enabled)
                                        .id("occurrence-previous")
                                        .role(Role::Button)
                                        .aria_label(if previous_enabled {
                                            "Previous example"
                                        } else {
                                            "Previous example, unavailable"
                                        })
                                        .when(previous_enabled, |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.navigate_occurrence(false, cx)
                                            }))
                                        }),
                                )
                                .child(
                                    button_state("Next →", false, next_enabled)
                                        .id("occurrence-next")
                                        .role(Role::Button)
                                        .aria_label(if next_enabled {
                                            "Next example"
                                        } else {
                                            "Next example, unavailable"
                                        })
                                        .when(next_enabled, |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.navigate_occurrence(true, cx)
                                            }))
                                        }),
                                ),
                        )
                        .when_some(navigation_hint, |navigation, hint| {
                            navigation.child(
                                div()
                                    .when(!compact, |hint_view| hint_view.ml_2())
                                    .text_xs()
                                    .text_color(Theme::DIM)
                                    .child(hint),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    button_state(
                                        "Create draft evals",
                                        true,
                                        self.can_generate_eval(),
                                    )
                                    .id("preview-group-evals")
                                    .role(Role::Button)
                                    .aria_label(if self.can_generate_eval() {
                                        "Generate eval candidates for this failure group"
                                    } else {
                                        "Generate eval candidates unavailable in this scope"
                                    })
                                    .when(
                                        self.can_generate_eval(),
                                        |button| {
                                            button.on_click(cx.listener(|this, _, _, cx| {
                                                this.preview_current_group(cx)
                                            }))
                                        },
                                    ),
                                )
                                .child(
                                    button("More…", self.investigation_actions_open)
                                        .id("investigation-actions")
                                        .role(Role::Button)
                                        .aria_label(if self.investigation_actions_open {
                                            "Close investigation actions"
                                        } else {
                                            "Open investigation actions"
                                        })
                                        .aria_toggled(if self.investigation_actions_open {
                                            Toggled::True
                                        } else {
                                            Toggled::False
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_investigation_actions(cx)
                                        })),
                                ),
                        ),
                )
                .when(self.investigation_actions_open, |toolbar| {
                    toolbar.child(
                        div()
                            .id("investigation-actions-menu")
                            .role(Role::Menu)
                            .mt_2()
                            .p_2()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(Theme::BORDER)
                            .bg(Theme::INSET_SURFACE)
                            .child(
                                button("Review finding", self.finding_review_open)
                                    .id("toggle-finding-review")
                                    .role(Role::MenuItem)
                                    .aria_label("Confirm, dismiss, or request more context")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.investigation_actions_open = false;
                                        this.toggle_finding_review(cx)
                                    })),
                            )
                            .child(
                                button("Examples & details", self.group_details_open)
                                    .id("toggle-failure-details")
                                    .role(Role::MenuItem)
                                    .aria_label("Open failure details and example chooser")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.investigation_actions_open = false;
                                        this.toggle_group_details(cx)
                                    })),
                            )
                            .child(
                                button("Open full trace", false)
                                    .id("full-trace")
                                    .role(Role::MenuItem)
                                    .aria_label("Open the complete trace for this example")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.investigation_actions_open = false;
                                        this.open_full_trace(cx)
                                    })),
                            )
                            .child(
                                button("Add this example", false)
                                    .id("create-candidate")
                                    .role(Role::MenuItem)
                                    .aria_label("Add this example as an eval candidate")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.investigation_actions_open = false;
                                        this.create_candidate(cx)
                                    })),
                            )
                            .child(
                                button_state(
                                    "Compare examples",
                                    false,
                                    self.can_compare_examples(),
                                )
                                .id("compare-examples")
                                .role(Role::MenuItem)
                                .aria_label(if self.can_compare_examples() {
                                    "Choose another example to compare with this one"
                                } else {
                                    "Compare unavailable; this group has no other example"
                                })
                                .when(
                                    self.can_compare_examples(),
                                    |button| {
                                        button.on_click(cx.listener(|this, _, _, cx| {
                                            this.investigation_actions_open = false;
                                            this.begin_compare_examples(cx)
                                        }))
                                    },
                                ),
                            )
                            .child(
                                button("Inspector", self.inspector_open)
                                    .id("toggle-investigation-inspector")
                                    .role(Role::MenuItem)
                                    .aria_label("Toggle investigation inspector")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.investigation_actions_open = false;
                                        this.toggle_inspector(cx)
                                    })),
                            ),
                    )
                })
                .when(self.finding_review_open, |toolbar| {
                    toolbar.child(
                        div()
                            .id("finding-review-menu")
                            .role(Role::Menu)
                            .mt_2()
                            .p_2()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(Theme::BORDER)
                            .bg(Theme::INSET_SURFACE)
                            .child(div().mr_2().text_xs().text_color(Theme::MUTED).child(
                                if evidence.occurrence.disposition_stale {
                                    "Prior review is stale"
                                } else {
                                    "Review this finding"
                                },
                            ))
                            .child(
                                button(
                                    "Confirm",
                                    current_disposition
                                        == Some(FindingDispositionStateV1::Confirmed),
                                )
                                .id("confirm-finding")
                                .role(Role::MenuItem)
                                .aria_label("Confirm this finding as a real failure")
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.review_finding(
                                            FindingDispositionStateV1::Confirmed,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                button(
                                    "Dismiss",
                                    current_disposition
                                        == Some(FindingDispositionStateV1::Dismissed),
                                )
                                .id("dismiss-finding")
                                .role(Role::MenuItem)
                                .aria_label("Dismiss this finding as not actionable")
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.review_finding(
                                            FindingDispositionStateV1::Dismissed,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                button(
                                    "Needs context",
                                    current_disposition
                                        == Some(FindingDispositionStateV1::NeedsContext),
                                )
                                .id("needs-context-finding")
                                .role(Role::MenuItem)
                                .aria_label("Mark this finding as needing more context")
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.review_finding(
                                            FindingDispositionStateV1::NeedsContext,
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .when(current_disposition.is_some(), |menu| {
                                menu.child(
                                    button("Undo", false)
                                        .id("undo-finding-review")
                                        .role(Role::MenuItem)
                                        .aria_label("Undo the current finding review")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.undo_finding_review(cx)
                                        })),
                                )
                            }),
                    )
                })
        });
        let mut body = div()
            .id("evidence-scroll")
            .role(Role::ListBox)
            .aria_label("Evidence spans")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_4();
        if let Some(evidence) = &self.evidence {
            if let Some(presentation) = evidence.presentation.as_ref() {
                let impact = self.selected_group.as_ref().map(|group| {
                    format!(
                        "{:?} severity · {} occurrences across {} runs",
                        group.summary.severity,
                        group.summary.occurrence_count,
                        group.summary.affected_run_count
                    )
                });
                body = body.child(
                    div()
                        .id("failure-diagnosis")
                        .role(Role::Status)
                        .aria_label(format!(
                            "Diagnosis: {} Expected: {} Observed: {} Recovery: {}",
                            presentation.diagnosis,
                            presentation.expected_behavior,
                            presentation.observed_behavior,
                            presentation.recovery_summary
                        ))
                        .p_4()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::INSET_SURFACE)
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(Theme::CYAN)
                                .child("DIAGNOSIS"),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(presentation.diagnosis.clone()),
                        )
                        .child(
                            div()
                                .mt_4()
                                .grid()
                                .grid_cols(if compact { 1 } else { 2 })
                                .gap_3()
                                .child(diagnosis_fact(
                                    "Expected",
                                    &presentation.expected_behavior,
                                    Theme::GREEN,
                                ))
                                .child(diagnosis_fact(
                                    "Observed",
                                    &presentation.observed_behavior,
                                    Theme::AMBER,
                                )),
                        )
                        .when_some(impact, |card, impact| {
                            card.child(diagnosis_fact("Impact", &impact, Theme::RED).mt_3())
                        })
                        .child(
                            diagnosis_fact(
                                "Recovery",
                                &presentation.recovery_summary,
                                Theme::MUTED,
                            )
                            .mt_3(),
                        )
                        .when_some(presentation.caveat.as_ref(), |card, caveat| {
                            card.child(diagnosis_fact("Caveat", caveat, Theme::AMBER).mt_3())
                        })
                        .child(
                            diagnosis_fact("Next", &presentation.remediation_hint, Theme::CYAN)
                                .mt_3(),
                        ),
                );
            }
            body = body
                .child(
                    div()
                        .mt_5()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .child("Execution evidence"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(format!(
                                    "{} evidence · {} with context",
                                    evidence.evidence_span_ids.len(),
                                    evidence.spans.len()
                                )),
                        ),
                )
                .child(
                    div()
                        .mt_1()
                        .mb_3()
                        .text_xs()
                        .text_color(Theme::DIM)
                        .child(
                            "Ordered by execution time. Each highlighted step explains why the detector used it; context stays visible without exposing payloads.",
                        ),
                )
                .when(evidence.spans.is_empty(), |view| {
                    view.child(
                        div()
                            .p_4()
                            .rounded_sm()
                            .border_1()
                            .border_color(Theme::BORDER)
                            .bg(Theme::PANEL_ALT)
                            .text_sm()
                            .text_color(Theme::MUTED)
                            .child("This finding has no committed span evidence. The failure metadata is available in Failure details, but Perseval will not invent execution context."),
                    )
                })
                .when(
                    !evidence.spans.is_empty() && evidence.evidence_span_ids.is_empty(),
                    |view| {
                        view.child(
                            div()
                                .mb_3()
                                .p_3()
                                .rounded_sm()
                                .bg(Theme::WARNING_SURFACE)
                                .text_xs()
                                .text_color(Theme::AMBER)
                                .child("Context spans are available, but the detector did not report an exact evidence span. Nothing is highlighted as causal."),
                        )
                    },
                );
            let presentation = evidence.presentation.as_ref();
            let mut ordered_spans = evidence.spans.iter().collect::<Vec<_>>();
            ordered_spans.sort_by(|left, right| {
                left.start_time_unix_nano
                    .cmp(&right.start_time_unix_nano)
                    .then_with(|| left.depth.cmp(&right.depth))
                    .then_with(|| left.span_id.cmp(&right.span_id))
            });
            let total_steps = ordered_spans.len();
            for (index, span) in ordered_spans.into_iter().enumerate() {
                let highlighted = evidence.evidence_span_ids.contains(&span.span_id);
                let presented_evidence = presentation.and_then(|presentation| {
                    presentation.evidence.iter().find(|presented| {
                        presented.evidence.span_id.as_deref() == Some(&span.span_id)
                    })
                });
                let evidence_role =
                    presented_evidence.map(|presented| humanize(&format!("{:?}", presented.role)));
                let role_label = evidence_role.clone().unwrap_or_else(|| {
                    if highlighted {
                        "Evidence".into()
                    } else {
                        "Context".into()
                    }
                });
                let explanation = presented_evidence
                    .map(|presented| presented.explanation.as_str())
                    .unwrap_or(if highlighted {
                        "This step is direct finding evidence."
                    } else {
                        "Execution context around the finding."
                    });
                let focused = self.focused_span_id.as_deref() == Some(&span.span_id);
                let span_id = span.span_id.clone();
                body = body.child(
                    evidence_graph_row(
                        span,
                        EvidenceGraphRow {
                            index,
                            total: total_steps,
                            role: &role_label,
                            explanation,
                            highlighted,
                            selected: focused,
                            compact,
                        },
                    )
                    .id(("evidence-span", index))
                    .role(Role::ListBoxOption)
                    .aria_label(format!(
                        "Step {}; {}; {}; {:.1} milliseconds; {}",
                        index + 1,
                        span.name,
                        span.category,
                        span.duration_nano as f64 / 1_000_000.,
                        explanation
                    ))
                    .aria_selected(focused)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.focus_evidence_span(span_id.clone(), cx)
                    })),
                );
            }
            if let Some(preview) = &self.candidate_preview {
                let candidate = &preview.candidate;
                body =
                    body.child(
                        div()
                            .mt_4()
                            .p_4()
                            .rounded_sm()
                            .border_1()
                            .border_color(Theme::CYAN)
                            .bg(Theme::INSET_SURFACE)
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(Theme::CYAN)
                                    .child("Unreviewed eval candidate"),
                            )
                            .child(kv(
                                "Evidence packet",
                                &format!(
                                    "{}\n{}",
                                    preview.evidence_packet.packet_id,
                                    preview.evidence_packet.content_hash
                                ),
                            ))
                            .child(kv(
                                "Source",
                                &format!(
                                    "{} · rev {} · {}",
                                    evidence.occurrence.run_title,
                                    evidence.occurrence.revision,
                                    evidence.occurrence.finding.finding_id
                                ),
                            ))
                            .child(kv(
                                "Proposed expectations",
                                &candidate.proposed_expected_behavior.join("\n✓ "),
                            ))
                            .child(kv("Rubric", &candidate.proposed_rubric))
                            .child(kv("Grader", &candidate.proposed_grader))
                            .child(kv(
                                "Generator",
                                &format!(
                                    "{}@{}",
                                    candidate.generator.name, candidate.generator.version
                                ),
                            ))
                            .when(
                                !preview.evidence_packet.telemetry_gaps.is_empty(),
                                |drawer| {
                                    drawer.child(kv(
                                        "Telemetry gaps",
                                        &preview.evidence_packet.telemetry_gaps.join("\n"),
                                    ))
                                },
                            )
                            .child(
                                div()
                                    .mt_4()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        button("Cancel", false)
                                            .id("cancel-candidate")
                                            .role(Role::Button)
                                            .aria_label("Cancel eval candidate preview")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.cancel_candidate(cx)
                                            })),
                                    )
                                    .child(
                                        button("Create 1 draft eval", true)
                                            .id("confirm-candidate")
                                            .role(Role::Button)
                                            .aria_label("Create one draft eval for review; this does not activate a grader")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.confirm_candidate(cx)
                                            })),
                                    ),
                            ),
                    );
            }
        } else {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(Theme::DIM)
                    .child("Select an occurrence to inspect exact evidence."),
            );
        }
        div()
            .min_w(px(390.))
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::PANEL)
            .child(
                div()
                    .h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .child("Evidence for this failure"),
            )
            .when_some(toolbar, |panel, toolbar| panel.child(toolbar))
            .child(body)
    }

    fn render_inspector_tab(&self) -> gpui::Stateful<Div> {
        let content = match self.tab {
            InspectorTab::Finding => self
                .evidence
                .as_ref()
                .map(|evidence| {
                    let certainty = &evidence.occurrence.finding.certainty;
                    let missing = if certainty.missing_facts.is_empty() {
                        "none".into()
                    } else {
                        certainty.missing_facts.join(", ")
                    };
                    let risk = certainty
                        .calibrated_failure_risk
                        .map(|value| format!("\nCalibrated failure risk: {:.0}%", value * 100.0))
                        .unwrap_or_default();
                    format!(
                        "Detector: {}@{}\nSeverity: {:?}\nRecovery: {:?}\nRule match: {:?}\nMissing facts: {}{}\n\n{}",
                        evidence.occurrence.finding.detector_id,
                        evidence.occurrence.finding.detector_version,
                        evidence.occurrence.finding.severity,
                        evidence.occurrence.finding.recovery,
                        certainty.rule_match,
                        missing,
                        risk,
                        if evidence.candidate.is_some() {
                            "UNREVIEWED EVAL CANDIDATE CREATED"
                        } else {
                            "No candidate created."
                        }
                    )
                })
                .unwrap_or_else(|| "This trace was opened without failure context.".into()),
            InspectorTab::Span => self
                .focused_span_snapshot
                .as_ref()
                .map(|span| {
                    format!(
                        "{}\n{}\nstatus {} · {} ns\nparent {}\n\nevents ({})\n{}\n\nlinks ({})\n{}",
                        span.name,
                        span.category,
                        span.status_code,
                        span.duration_nano,
                        span.parent_span_id.as_deref().unwrap_or("root"),
                        span.events.len(),
                        serde_json::to_string_pretty(&span.events).unwrap_or_default(),
                        span.links.len(),
                        serde_json::to_string_pretty(&span.links).unwrap_or_default(),
                    )
                })
                .unwrap_or_else(|| "Choose a span.".into()),
            InspectorTab::Attributes => self
                .focused_span_snapshot
                .as_ref()
                .map(|span| serde_json::to_string_pretty(&span.attributes).unwrap_or_default())
                .unwrap_or_else(|| "No focused span.".into()),
            InspectorTab::Payload => {
                "Payloads remain hidden until an explicit bounded reveal below.".into()
            }
        };
        div()
            .id("inspector-tab-panel")
            .role(Role::TabPanel)
            .aria_label(content.clone())
            .mt_3()
            .p_3()
            .rounded_sm()
            .bg(Theme::BG)
            .text_xs()
            .text_color(Theme::MUTED)
            .child(content)
    }

    pub(super) fn render_shared_inspector(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let mut body = div()
            .id("shared-inspector-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_3()
            .child(
                div()
                    .id("inspector-tabs")
                    .role(Role::TabList)
                    .aria_label("Inspector sections")
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children([
                        tab_button(
                            "Finding",
                            self.tab == InspectorTab::Finding,
                            InspectorTab::Finding,
                            cx,
                        ),
                        tab_button(
                            "Span",
                            self.tab == InspectorTab::Span,
                            InspectorTab::Span,
                            cx,
                        ),
                        tab_button(
                            "Attributes",
                            self.tab == InspectorTab::Attributes,
                            InspectorTab::Attributes,
                            cx,
                        ),
                        tab_button(
                            "Payload",
                            self.tab == InspectorTab::Payload,
                            InspectorTab::Payload,
                            cx,
                        ),
                    ]),
            )
            .child(self.render_inspector_tab());
        if self.tab == InspectorTab::Payload
            && let Some(span) = self.focused_span_snapshot.as_ref()
        {
            if span.payload_refs.is_empty() {
                body = body.child(
                    div()
                        .mt_3()
                        .text_xs()
                        .text_color(Theme::DIM)
                        .child("This span has no externalized payloads."),
                );
            }
            for (index, (key, payload)) in span.payload_refs.iter().enumerate() {
                let key_for_click = key.clone();
                let blob = payload.clone();
                body = body.child(
                    button(&format!("Reveal {key} (bounded)"), false)
                        .id(("reveal-inspector-payload", index))
                        .role(Role::Button)
                        .aria_label(format!("Reveal {key}, bounded preview"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reveal_payload(key_for_click.clone(), blob.clone(), cx)
                        })),
                );
            }
            if let Some((key, value, preview)) = &self.revealed_payload {
                let revealed = format!(
                    "{key} · {} of {} bytes{}\n\n{value}",
                    preview.revealed_bytes,
                    preview.original_bytes,
                    if preview.truncated {
                        " · truncated"
                    } else {
                        ""
                    }
                );
                body = body.child(
                    div()
                        .id("revealed-payload-preview")
                        .role(Role::Document)
                        .aria_label(revealed.clone())
                        .mt_3()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::BG)
                        .text_xs()
                        .child(revealed),
                );
                if preview.truncated {
                    if preview.larger_local_reveal_allowed {
                        body = body.child(
                            button("Reveal larger local preview", false)
                                .id("reveal-larger-local-payload")
                                .role(Role::Button)
                                .aria_label("Reveal a larger local payload preview")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.reveal_larger_payload(cx)),
                                ),
                        );
                    } else {
                        body =
                            body.child(div().mt_2().text_xs().text_color(Theme::AMBER).child(
                                "Larger reveal is blocked by this workspace's payload policy.",
                            ));
                    }
                }
            }
        }
        div()
            .h_full()
            .flex()
            .flex_col()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .when(compact, |panel| panel.w_full().min_w_0())
            .when(!compact, |panel| {
                panel
                    .w(px(self.inspector_width))
                    .min_w(px(280.))
                    .flex_none()
                    .border_l_1()
            })
            .child(
                div()
                    .id("shared-inspector-header")
                    .role(Role::Complementary)
                    .aria_label("Trace and evidence inspector")
                    .h(px(48.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .child("Inspector"),
                    )
                    .child(
                        button("Close", false)
                            .id("close-shared-inspector")
                            .role(Role::Button)
                            .aria_label("Close inspector")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx))),
                    ),
            )
            .child(body)
    }
}

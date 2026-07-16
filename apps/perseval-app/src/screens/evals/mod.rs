use std::sync::Arc;

use gpui::{
    AppContext, Context, Div, EventEmitter, FontWeight, IntoElement, Render, Role, Window, div,
    prelude::*, px,
};
use perseval_service::{
    EvalCandidateRecordV1, EvalReviewDecisionV1, EvalReviewQueueStateV1, LiveTraceService,
};

use crate::components::{button, button_state, tag, telemetry_gap_summary};
use crate::design::Theme;

#[derive(Debug, Clone)]
pub(crate) enum EvalReviewEvent {
    Candidate {
        project_id: String,
        candidate_id: String,
    },
    Queue,
    SourceTrace {
        project_id: String,
        logical_trace_id: String,
        revision: u64,
        selected_span_id: Option<String>,
    },
}

pub(crate) struct EvalReviewScreen {
    service: Arc<LiveTraceService>,
    project_id: Option<String>,
    candidates: Vec<EvalCandidateRecordV1>,
    selected: Option<EvalCandidateRecordV1>,
    loading: bool,
    mutating: bool,
    error: Option<String>,
    notice: Option<String>,
    request_generation: u64,
}

impl EventEmitter<EvalReviewEvent> for EvalReviewScreen {}

impl EvalReviewScreen {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut screen = Self {
            service,
            project_id,
            candidates: Vec::new(),
            selected: None,
            loading: false,
            mutating: false,
            error: None,
            notice: None,
            request_generation: 0,
        };
        screen.reload(cx);
        screen
    }

    pub(crate) fn set_project_scope(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        if self.project_id == project_id {
            return;
        }
        self.project_id = project_id;
        self.selected = None;
        self.notice = None;
        self.reload(cx);
    }

    pub(crate) fn show_queue(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.reload(cx);
    }

    pub(crate) fn show_candidate(
        &mut self,
        project_id: &str,
        candidate_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.project_id = Some(project_id.to_owned());
        self.loading = true;
        self.error = None;
        self.notice = None;
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        let service = self.service.clone();
        let project_id = project_id.to_owned();
        let candidate_id = candidate_id.to_owned();
        let task =
            cx.background_spawn(
                async move { service.get_eval_candidate(&project_id, &candidate_id) },
            );
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(Some(candidate)) => this.selected = Some(candidate),
                    Ok(None) => {
                        this.selected = None;
                        this.error = Some("This eval candidate no longer exists.".into());
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        let service = self.service.clone();
        let project_id = self.project_id.clone();
        let task = cx.background_spawn(async move {
            service.list_eval_candidates(project_id.as_deref(), 0, 200)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(candidates) => this.candidates = candidates,
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_candidate(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(candidate) = self.candidates.get(index) else {
            return;
        };
        cx.emit(EvalReviewEvent::Candidate {
            project_id: candidate.project_id.clone(),
            candidate_id: candidate.candidate.candidate_id.clone(),
        });
    }

    fn open_queue(&mut self, cx: &mut Context<Self>) {
        cx.emit(EvalReviewEvent::Queue);
    }

    fn selected_candidate_index(&self) -> Option<usize> {
        let candidate_id = &self.selected.as_ref()?.candidate.candidate_id;
        self.candidates
            .iter()
            .position(|candidate| candidate.candidate.candidate_id == *candidate_id)
    }

    fn navigate_candidate(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(index) = self.selected_candidate_index() else {
            return;
        };
        let next = if forward {
            index.checked_add(1)
        } else {
            index.checked_sub(1)
        };
        if let Some(index) = next.filter(|index| *index < self.candidates.len()) {
            self.open_candidate(index, cx);
        }
    }

    fn open_source_trace(&mut self, cx: &mut Context<Self>) {
        let Some(candidate) = self.selected.as_ref() else {
            return;
        };
        cx.emit(EvalReviewEvent::SourceTrace {
            project_id: candidate.project_id.clone(),
            logical_trace_id: candidate.logical_trace_id.clone(),
            revision: candidate.revision,
            selected_span_id: candidate
                .evidence_packet
                .evidence
                .iter()
                .find_map(|evidence| evidence.span_id.clone()),
        });
    }

    fn review(&mut self, decision: EvalReviewDecisionV1, cx: &mut Context<Self>) {
        if self.mutating {
            return;
        }
        let Some(candidate) = self.selected.as_ref() else {
            return;
        };
        if self.project_id.as_deref() != Some(candidate.project_id.as_str()) {
            self.error = Some("Choose the candidate's project before reviewing it.".into());
            cx.notify();
            return;
        }
        self.mutating = true;
        self.error = None;
        self.notice = None;
        let service = self.service.clone();
        let project_id = candidate.project_id.clone();
        let candidate_id = candidate.candidate.candidate_id.clone();
        let task = crate::blocking::run(
            "perseval-eval-review",
            move || service.review_eval_candidate(&project_id, &candidate_id, decision, None),
            cx,
        );
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(Ok(candidate)) => {
                        this.notice = Some(match candidate.queue_state {
                            EvalReviewQueueStateV1::Accepted => {
                                "Accepted. This candidate is now an approved definition.".into()
                            }
                            EvalReviewQueueStateV1::Rejected => {
                                "Rejected. The review record and evidence remain available.".into()
                            }
                            EvalReviewQueueStateV1::Deferred => {
                                "Deferred. It remains in the review queue.".into()
                            }
                            _ => "Review saved.".into(),
                        });
                        if let Some(existing) = this.candidates.iter_mut().find(|existing| {
                            existing.candidate.candidate_id == candidate.candidate.candidate_id
                        }) {
                            *existing = candidate.clone();
                        }
                        this.selected = Some(candidate);
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string()),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_queue(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let pending = self
            .candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.queue_state,
                    EvalReviewQueueStateV1::Pending | EvalReviewQueueStateV1::Deferred
                )
            })
            .count();
        let mut list = div()
            .id("eval-review-queue-scroll")
            .role(Role::ListBox)
            .aria_label("Eval candidates awaiting review")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when(compact, |list| list.px_4())
            .when(!compact, |list| list.px_6())
            .pb_6();
        if self.loading && self.candidates.is_empty() {
            list = list.child(
                div()
                    .py_8()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child("Loading eval candidates…"),
            );
        } else if self.candidates.is_empty() {
            list = list.child(
                div()
                    .mt_6()
                    .p_6()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("No candidates to review"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .text_color(Theme::MUTED)
                            .child("Select one or more failure groups in the Inbox and generate a bounded candidate batch."),
                    ),
            );
        }
        for (index, candidate) in self.candidates.iter().enumerate() {
            let tint = queue_state_tint(candidate.queue_state);
            list = list.child(
                div()
                    .id(("eval-candidate-row", index))
                    .role(Role::ListBoxOption)
                    .aria_label(format!(
                        "{}; {}; project {}; trace {}; revision {}",
                        candidate.candidate.proposed_rubric,
                        queue_state_label(candidate.queue_state),
                        candidate.project_id,
                        short_id(&candidate.logical_trace_id),
                        candidate.revision
                    ))
                    .aria_selected(false)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .mt_3()
                    .p_4()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.open_candidate(index, cx)))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_start()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(candidate.candidate.proposed_rubric.clone()),
                                    )
                                    .child(div().mt_2().text_xs().text_color(Theme::MUTED).child(
                                        format!(
                                            "Project {} · group {} · trace {} · revision {}",
                                            candidate.project_id,
                                            short_id(&candidate.group_id),
                                            short_id(&candidate.logical_trace_id),
                                            candidate.revision
                                        ),
                                    )),
                            )
                            .child(tag(queue_state_label(candidate.queue_state), tint)),
                    )
                    .when(
                        !candidate.evidence_packet.telemetry_gaps.is_empty(),
                        |card| {
                            card.child(div().mt_3().text_xs().text_color(Theme::AMBER).child(
                                format!(
                                    "Telemetry gaps: {}",
                                    telemetry_gap_summary(
                                        &candidate.evidence_packet.telemetry_gaps
                                    )
                                ),
                            ))
                        },
                    ),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .child(
                div()
                    .px_6()
                    .py_5()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(FontWeight::BOLD)
                                            .child("Eval review"),
                                    )
                                    .child(div().mt_1().text_sm().text_color(Theme::MUTED).child(
                                        format!(
                                            "{pending} awaiting a decision · {} total",
                                            self.candidates.len()
                                        ),
                                    )),
                            )
                            .child(tag(
                                if self.project_id.is_some() {
                                    "PROJECT SCOPE"
                                } else {
                                    "ALL PROJECTS · READ ONLY"
                                },
                                if self.project_id.is_some() {
                                    Theme::CYAN
                                } else {
                                    Theme::AMBER
                                },
                            )),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .mx_6()
                        .mt_3()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::DANGER_SURFACE)
                        .text_sm()
                        .text_color(Theme::RED)
                        .child(error),
                )
            })
            .child(list)
    }

    fn render_candidate(
        &self,
        candidate: &EvalCandidateRecordV1,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let actionable = matches!(
            candidate.queue_state,
            EvalReviewQueueStateV1::Pending | EvalReviewQueueStateV1::Deferred
        );
        let enabled = actionable && !self.mutating && self.project_id.is_some();
        let selected_index = self.selected_candidate_index();
        let previous_enabled = selected_index.is_some_and(|index| index > 0);
        let next_enabled = selected_index.is_some_and(|index| index + 1 < self.candidates.len());
        let position = selected_index
            .map(|index| format!("Candidate {} of {}", index + 1, self.candidates.len()))
            .unwrap_or_else(|| "Candidate opened directly".into());
        let input_missing = candidate.candidate.proposed_input.is_none();
        let proposed_input = candidate
            .candidate
            .proposed_input
            .as_ref()
            .map(|input| input.summary().to_owned())
            .unwrap_or_else(|| "No runnable input fixture has been materialized.".into());
        let expected = if candidate.candidate.proposed_expected_behavior.is_empty() {
            "No expected behavior proposed.".into()
        } else {
            candidate.candidate.proposed_expected_behavior.join("\n• ")
        };
        let mut primary = div()
            .flex_1()
            .min_w_0()
            .child(review_card(
                "Proposed behavior",
                &candidate.candidate.proposed_rubric,
            ))
            .child(
                review_card("Redacted input", &proposed_input).when(input_missing, |card| {
                    card.border_color(Theme::AMBER).bg(Theme::WARNING_SURFACE)
                }),
            )
            .child(review_card("Expected behavior", &format!("• {expected}")))
            .child(review_card("Grader", &candidate.candidate.proposed_grader));
        if input_missing {
            primary = primary.child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(Theme::AMBER)
                    .child("This draft is not runnable yet. Accepting it approves the definition, not an executable fixture."),
            );
        }
        let mut provenance = div()
            .when(compact, |column| column.w_full())
            .when(!compact, |column| column.w(px(340.)).flex_none())
            .child(review_card(
                "Evidence provenance",
                &format!(
                    "Packet {}\nContent {}\nFinding {}\nTrace {} · revision {}\n{} evidence references",
                    short_id(&candidate.evidence_packet.packet_id),
                    short_id(&candidate.evidence_packet.content_hash),
                    short_id(&candidate.finding_id),
                    short_id(&candidate.logical_trace_id),
                    candidate.revision,
                    candidate.evidence_packet.evidence.len()
                ),
            ))
            .child(review_card(
                "Generator",
                &format!(
                    "{} {}\nDefinition {}",
                    candidate.candidate.generator.name,
                    candidate.candidate.generator.version,
                    short_id(&candidate.candidate.definition_hash)
                ),
            ));
        if !candidate.evidence_packet.telemetry_gaps.is_empty() {
            provenance = provenance.child(
                review_card(
                    "Telemetry gaps",
                    &telemetry_gap_summary(&candidate.evidence_packet.telemetry_gaps),
                )
                .border_color(Theme::AMBER),
            );
        }
        if let Some(review) = &candidate.candidate.review {
            provenance = provenance.child(review_card(
                "Review record",
                &format!(
                    "{:?} by {} at {}{}",
                    review.decision,
                    review.reviewer_ref,
                    review.reviewed_at,
                    review
                        .reason
                        .as_ref()
                        .map(|reason| format!("\nReason: {reason}"))
                        .unwrap_or_default()
                ),
            ));
        }
        let body = div()
            .id("eval-candidate-review-scroll")
            .role(Role::Document)
            .aria_label(format!(
                "Eval candidate review. Proposed behavior: {}. Redacted input: {}. Expected behavior: {}. Grader: {}. Evidence packet {}. Finding {}. Trace {}, revision {}.",
                candidate.candidate.proposed_rubric,
                proposed_input,
                expected,
                candidate.candidate.proposed_grader,
                short_id(&candidate.evidence_packet.packet_id),
                short_id(&candidate.finding_id),
                short_id(&candidate.logical_trace_id),
                candidate.revision
            ))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when(compact, |body| body.px_4())
            .when(!compact, |body| body.px_6())
            .py_5()
            .child(
                div()
                    .w_full()
                    .flex()
                    .when(compact, |columns| columns.flex_col())
                    .when(!compact, |columns| columns.items_start())
                    .gap_4()
                    .child(primary)
                    .child(provenance),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .child(
                div()
                    .when(compact, |header| header.px_4())
                    .when(!compact, |header| header.px_6())
                    .py_4()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::CYAN)
                                    .child("Eval candidate review"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(candidate.candidate.proposed_rubric.clone()),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(Theme::MUTED)
                                    .child(format!(
                                        "Project {} · concrete finding {}",
                                        candidate.project_id,
                                        short_id(&candidate.finding_id)
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                button_state("← Previous", false, previous_enabled)
                                    .id("previous-eval-candidate")
                                    .role(Role::Button)
                                    .aria_label(if previous_enabled {
                                        "Previous eval candidate"
                                    } else {
                                        "Previous eval candidate, unavailable"
                                    })
                                    .when(previous_enabled, |button| {
                                        button.on_click(cx.listener(|this, _, _, cx| {
                                            this.navigate_candidate(false, cx)
                                        }))
                                    }),
                            )
                            .child(div().text_xs().text_color(Theme::MUTED).child(position))
                            .child(
                                button_state("Next →", false, next_enabled)
                                    .id("next-eval-candidate")
                                    .role(Role::Button)
                                    .aria_label(if next_enabled {
                                        "Next eval candidate"
                                    } else {
                                        "Next eval candidate, unavailable"
                                    })
                                    .when(next_enabled, |button| {
                                        button.on_click(cx.listener(|this, _, _, cx| {
                                            this.navigate_candidate(true, cx)
                                        }))
                                    }),
                            )
                            .child(tag(
                                queue_state_label(candidate.queue_state),
                                queue_state_tint(candidate.queue_state),
                            )),
                    ),
            )
            .when_some(self.notice.clone(), |view, notice| {
                view.child(
                    div()
                        .id("eval-review-notice")
                        .role(Role::Status)
                        .aria_label(notice.clone())
                        .mx_6()
                        .mt_3()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::SUCCESS_SURFACE)
                        .text_sm()
                        .text_color(Theme::GREEN)
                        .child(notice),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("eval-review-error")
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .mx_6()
                        .mt_3()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::DANGER_SURFACE)
                        .text_sm()
                        .text_color(Theme::RED)
                        .child(error),
                )
            })
            .child(body)
            .child(
                div()
                    .px_5()
                    .flex()
                    .when(compact, |footer| {
                        footer.flex_col().items_start().gap_2().py_3()
                    })
                    .when(!compact, |footer| {
                        footer.h(px(60.)).items_center().justify_between()
                    })
                    .border_t_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("Back to queue", false)
                                    .id("back-to-eval-queue")
                                    .role(Role::Button)
                                    .aria_label("Back to eval review queue")
                                    .on_click(cx.listener(|this, _, _, cx| this.open_queue(cx))),
                            )
                            .child(
                                button("Open source trace", false)
                                    .id("open-eval-source-trace")
                                    .role(Role::Button)
                                    .aria_label(
                                        "Open the trace used to generate this eval candidate",
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_source_trace(cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .when(!actionable, |actions| {
                                actions.child(
                                    div()
                                        .id("eval-review-complete")
                                        .role(Role::Status)
                                        .text_sm()
                                        .text_color(queue_state_tint(candidate.queue_state))
                                        .child(format!(
                                            "Review complete · {}",
                                            queue_state_label(candidate.queue_state)
                                        )),
                                )
                            })
                            .when(actionable, |actions| {
                                actions
                                    .child(
                                        button_state("Defer", false, enabled)
                                            .id("defer-eval-candidate")
                                            .role(Role::Button)
                                            .aria_label(review_action_label(
                                                "Defer eval candidate",
                                                self.mutating,
                                                self.project_id.is_some(),
                                            ))
                                            .when(enabled, |button| {
                                                button.on_click(cx.listener(|this, _, _, cx| {
                                                    this.review(EvalReviewDecisionV1::Defer, cx)
                                                }))
                                            }),
                                    )
                                    .child(
                                        button_state("Reject", false, enabled)
                                            .id("reject-eval-candidate")
                                            .role(Role::Button)
                                            .aria_label(review_action_label(
                                                "Reject eval candidate",
                                                self.mutating,
                                                self.project_id.is_some(),
                                            ))
                                            .when(enabled, |button| {
                                                button.on_click(cx.listener(|this, _, _, cx| {
                                                    this.review(EvalReviewDecisionV1::Reject, cx)
                                                }))
                                            }),
                                    )
                                    .child(
                                        button_state("Accept candidate", true, enabled)
                                            .id("accept-eval-candidate")
                                            .role(Role::Button)
                                            .aria_label(review_action_label(
                                                "Accept eval candidate",
                                                self.mutating,
                                                self.project_id.is_some(),
                                            ))
                                            .when(enabled, |button| {
                                                button.on_click(cx.listener(|this, _, _, cx| {
                                                    this.review(EvalReviewDecisionV1::Accept, cx)
                                                }))
                                            }),
                                    )
                            }),
                    ),
            )
    }
}

impl Render for EvalReviewScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact =
            crate::design::Breakpoint::for_window(window) == crate::design::Breakpoint::Compact;
        if let Some(candidate) = &self.selected {
            self.render_candidate(candidate, compact, cx)
        } else {
            self.render_queue(compact, cx)
        }
    }
}

fn review_card(title: &str, content: &str) -> Div {
    div()
        .mt_3()
        .w_full()
        .p_4()
        .rounded(px(6.))
        .border_1()
        .border_color(Theme::BORDER)
        .bg(Theme::PANEL_SURFACE)
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::MUTED)
                .child(title.to_uppercase()),
        )
        .child(div().mt_2().text_sm().child(content.to_owned()))
}

fn queue_state_label(state: EvalReviewQueueStateV1) -> &'static str {
    match state {
        EvalReviewQueueStateV1::Pending => "PENDING",
        EvalReviewQueueStateV1::Deferred => "DEFERRED",
        EvalReviewQueueStateV1::Accepted => "ACCEPTED",
        EvalReviewQueueStateV1::Rejected => "REJECTED",
        EvalReviewQueueStateV1::Superseded => "SUPERSEDED",
    }
}

fn queue_state_tint(state: EvalReviewQueueStateV1) -> gpui::Rgba {
    match state {
        EvalReviewQueueStateV1::Pending => Theme::AMBER,
        EvalReviewQueueStateV1::Deferred => Theme::PURPLE,
        EvalReviewQueueStateV1::Accepted => Theme::GREEN,
        EvalReviewQueueStateV1::Rejected => Theme::RED,
        EvalReviewQueueStateV1::Superseded => Theme::DIM,
    }
}

fn review_action_label(
    enabled_label: &'static str,
    mutating: bool,
    has_project_scope: bool,
) -> &'static str {
    if mutating {
        "Review action unavailable while the decision is being saved"
    } else if !has_project_scope {
        "Review action unavailable in all-projects scope"
    } else {
        enabled_label
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(16)).unwrap_or(value)
}

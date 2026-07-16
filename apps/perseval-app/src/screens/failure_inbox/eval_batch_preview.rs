use super::components::*;
use super::render::short_hash;
use super::*;
use crate::components::telemetry_gap_summary;
use perseval_service::EvalSelectionReasonV1;
use std::collections::BTreeSet;

impl FailureInbox {
    pub(super) fn toggle_eval_preview_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if !self.expanded_eval_group_ids.remove(&group_id) {
            self.expanded_eval_group_ids.insert(group_id);
        }
        cx.notify();
    }

    pub(super) fn render_eval_batch_preview(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let generation_running = self.generation_job.as_ref().is_some_and(|job| {
            matches!(
                job.status,
                CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
            )
        });
        let generation_retryable = self.generation_job.as_ref().is_some_and(|job| {
            matches!(
                job.status,
                CandidateGenerationJobStatusV1::Failed
                    | CandidateGenerationJobStatusV1::PartialSuccess
                    | CandidateGenerationJobStatusV1::Cancelled
            )
        });
        let mut body = div()
            .id("eval-batch-preview-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when(compact, |body| body.p_4())
            .when(!compact, |body| body.p_6());
        if let Some(preview) = &self.batch_preview {
            body = body
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_start()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .child(
                                    div()
                                        .text_xl()
                                        .font_weight(FontWeight::BOLD)
                                        .child("Review eval candidates"),
                                )
                                .child(
                                    div()
                                        .mt_2()
                                        .max_w(px(720.))
                                        .text_sm()
                                        .text_color(Theme::MUTED)
                                        .child(format!(
                                            "{} representative examples from {} failure groups. New definitions enter review; this does not activate or schedule a grader.",
                                            preview.items.len(),
                                            preview.selection_spec.group_ids.len()
                                        )),
                                ),
                        )
                        .child(tag("UNREVIEWED", Theme::AMBER)),
                )
                .child(
                    div()
                        .mt_4()
                        .p_3()
                        .rounded_sm()
                        .border_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::PANEL)
                        .text_xs()
                        .text_color(Theme::MUTED)
                        .child(format!(
                            "Project {} · bounded to {} candidates · selection {}",
                            preview.project_id,
                            preview.maximum_candidate_count,
                            short_hash(&preview.selection_hash)
                        )),
                );
            for (group_index, group_id) in preview.selection_spec.group_ids.iter().enumerate() {
                let items = preview
                    .items
                    .iter()
                    .filter(|item| &item.group_id == group_id)
                    .collect::<Vec<_>>();
                let exclusions = preview
                    .exclusions
                    .iter()
                    .filter(|exclusion| &exclusion.group_id == group_id)
                    .collect::<Vec<_>>();
                let proposed = items.iter().filter(|item| !item.already_exists).count();
                let duplicates = items.len().saturating_sub(proposed);
                let evidence_count = items
                    .iter()
                    .map(|item| item.evidence_packet.evidence.len())
                    .sum::<usize>();
                let telemetry_caveats = items
                    .iter()
                    .filter(|item| !item.telemetry_gaps.is_empty())
                    .count();
                let builds = items
                    .iter()
                    .filter_map(|item| item.build_id.as_deref())
                    .collect::<BTreeSet<_>>();
                let sessions = items
                    .iter()
                    .filter_map(|item| item.session_id.as_deref())
                    .collect::<BTreeSet<_>>();
                let expanded = self.expanded_eval_group_ids.contains(group_id);
                let title = self
                    .groups
                    .iter()
                    .find(|group| &group.group_id == group_id)
                    .and_then(|group| group.presentation.as_ref())
                    .map(|presentation| presentation.title.clone())
                    .unwrap_or_else(|| format!("Failure group {}", short_hash(group_id)));
                let toggle_group_id = group_id.clone();
                let mut group_card =
                    div()
                        .id(("eval-batch-group", group_index))
                        .mt_3()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::PANEL)
                        .child(
                            div()
                                .id(("toggle-eval-batch-group", group_index))
                                .role(Role::Button)
                                .aria_label(format!(
                                    "{}; {} representative examples; {} proposed; {} already exist; {} excluded",
                                    title,
                                    items.len(),
                                    proposed,
                                    duplicates,
                                    exclusions.len()
                                ))
                                .aria_expanded(expanded)
                                .tab_index(0)
                                .focus_visible(|style| {
                                    style.border_2().border_color(Theme::CYAN)
                                })
                                .p_4()
                                .flex()
                                .flex_wrap()
                                .items_start()
                                .justify_between()
                                .gap_4()
                                .cursor_pointer()
                                .hover(|style| style.bg(Theme::PANEL_ALT))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_eval_preview_group(toggle_group_id.clone(), cx)
                                }))
                                .child(
                                    div()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .mt_2()
                                                .text_xs()
                                                .text_color(Theme::MUTED)
                                                .child(format!(
                                                    "{} representatives · {} evidence refs · {} build{} · {} session{}",
                                                    items.len(),
                                                    evidence_count,
                                                    builds.len().max(1),
                                                    if builds.len() == 1 { "" } else { "s" },
                                                    sessions.len().max(1),
                                                    if sessions.len() == 1 { "" } else { "s" }
                                                )),
                                        )
                                        .when(telemetry_caveats > 0, |summary| {
                                            summary.child(
                                                div()
                                                    .mt_1()
                                                    .text_xs()
                                                    .text_color(Theme::AMBER)
                                                    .child(format!(
                                                        "{telemetry_caveats} representative{} need telemetry review",
                                                        if telemetry_caveats == 1 { "" } else { "s" }
                                                    )),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_2()
                                        .child(tag(
                                            &format!("{proposed} PROPOSED"),
                                            Theme::CYAN,
                                        ))
                                        .when(duplicates > 0, |badges| {
                                            badges.child(tag(
                                                &format!("{duplicates} EXISTING"),
                                                Theme::MUTED,
                                            ))
                                        })
                                        .when(!exclusions.is_empty(), |badges| {
                                            badges.child(tag(
                                                &format!("{} EXCLUDED", exclusions.len()),
                                                Theme::AMBER,
                                            ))
                                        })
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(Theme::DIM)
                                                .child(if expanded { "▾" } else { "▸" }),
                                        ),
                                ),
                        );
                if expanded {
                    for (item_index, item) in items.into_iter().enumerate() {
                        group_card = group_card.child(
                            div()
                                .id(("eval-batch-item", group_index * 1000 + item_index))
                                .px_4()
                                .py_3()
                                .border_t_1()
                                .border_color(Theme::BORDER)
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .text_xs()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(selection_reason_label(
                                                    item.selection_reason,
                                                )),
                                        )
                                        .child(if item.already_exists {
                                            tag("ALREADY EXISTS", Theme::MUTED)
                                        } else {
                                            tag("DRAFT", Theme::CYAN)
                                        }),
                                )
                                .child(div().mt_1().text_xs().text_color(Theme::MUTED).child(
                                    format!(
                                        "{} · build {} · session {} · revision {} · {:?}",
                                        item.run_title.as_deref().unwrap_or("Unnamed run"),
                                        item.build_id.as_deref().unwrap_or("unknown"),
                                        item.session_id.as_deref().unwrap_or("unknown"),
                                        item.revision,
                                        item.recovery
                                    ),
                                ))
                                .child(
                                    div()
                                        .mt_2()
                                        .text_xs()
                                        .child(item.candidate.proposed_rubric.clone()),
                                )
                                .when(!item.telemetry_gaps.is_empty(), |row| {
                                    row.child(
                                        div().mt_2().text_xs().text_color(Theme::AMBER).child(
                                            format!(
                                                "Telemetry: {}",
                                                telemetry_gap_summary(&item.telemetry_gaps)
                                            ),
                                        ),
                                    )
                                }),
                        );
                    }
                    for exclusion in exclusions {
                        group_card = group_card.child(
                            div()
                                .px_4()
                                .py_2()
                                .border_t_1()
                                .border_color(Theme::BORDER)
                                .text_xs()
                                .text_color(Theme::DIM)
                                .child(format!("Excluded: {}", exclusion.reason)),
                        );
                    }
                }
                body = body.child(group_card);
            }
        }
        if let Some(job) = &self.generation_job {
            body = body.child(
                div()
                    .mt_5()
                    .p_4()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(Theme::GREEN)
                    .bg(Theme::SUCCESS_SURFACE)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::GREEN)
                            .child(format!("Generation {:?}", job.status)),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(format!(
                                "{} of {} representatives completed. Candidates remain unreviewed until you explicitly review them.",
                                job.outcomes.len(),
                                self.batch_preview.as_ref().map_or(0, |preview| preview.items.len())
                            )),
                    ),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .child(body)
            .child(
                div()
                    .px_5()
                    .flex()
                    .when(compact, |footer| {
                        footer.flex_col().items_start().gap_2().py_3()
                    })
                    .when(!compact, |footer| {
                        footer.h(px(58.)).items_center().justify_between()
                    })
                    .border_t_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .child(
                        button(
                            if generation_running {
                                "Cancel generation"
                            } else {
                                "Close"
                            },
                            false,
                        )
                        .id("cancel-eval-batch")
                        .role(Role::Button)
                        .aria_label(if generation_running {
                            "Cancel eval candidate generation"
                        } else {
                            "Close eval batch preview"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_generation_job(cx))),
                    )
                    .child(if generation_running {
                        button_state("Generating…", true, false)
                            .id("generation-progress")
                            .role(Role::Status)
                            .aria_label("Generating eval candidates")
                    } else if generation_retryable {
                        button("Retry incomplete", true)
                            .id("retry-eval-batch")
                            .role(Role::Button)
                            .aria_label("Retry incomplete eval candidates")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_generation_job(cx)))
                    } else if self.generation_job.is_some() {
                        button("Open eval review", true)
                            .id("open-eval-review")
                            .role(Role::Button)
                            .aria_label("Open eval review queue")
                            .on_click(cx.listener(|this, _, _, cx| this.open_eval_queue(cx)))
                    } else {
                        let draft_count = self.batch_preview.as_ref().map_or(0, |preview| {
                            preview
                                .items
                                .iter()
                                .filter(|item| !item.already_exists)
                                .count()
                        });
                        let draft_label = if self.batch_loading {
                            "Creating…".to_string()
                        } else if draft_count == 0 {
                            "No new draft evals".to_string()
                        } else {
                            format!("Create {draft_count} draft evals")
                        };
                        button_state(&draft_label, true, !self.batch_loading && draft_count > 0)
                            .id("create-eval-batch")
                            .role(Role::Button)
                            .aria_label(if self.batch_loading {
                                "Creating eval candidates"
                            } else {
                                "Create draft evals for review; this does not activate a grader"
                            })
                            .when(!self.batch_loading, |button| {
                                button.on_click(
                                    cx.listener(|this, _, _, cx| this.create_eval_batch(cx)),
                                )
                            })
                    }),
            )
    }
}

fn selection_reason_label(reason: EvalSelectionReasonV1) -> &'static str {
    match reason {
        EvalSelectionReasonV1::CanonicalMember => "Canonical example",
        EvalSelectionReasonV1::NewestUnrecovered => "Newest unresolved example",
        EvalSelectionReasonV1::RecoveredExample => "Recovered example",
        EvalSelectionReasonV1::DistinctAgentBuild => "Distinct agent build",
        EvalSelectionReasonV1::DistinctExecutionShape => "Distinct execution shape",
    }
}

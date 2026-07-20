use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Div, Entity, EventEmitter, FontWeight, IntoElement, Render,
    Role, Window, div, prelude::*, px,
};
use perseval_service::{
    AnnotationLabelV1, LiveTraceService, ReviewAdjudicationPacketV1, ReviewModeV1,
    ReviewTaskPresentationV1, ReviewTaskStatusV1, ReviewTaskV1,
};

use crate::components::{TextInput, button_state, tag};
use crate::design::Theme;

#[derive(Debug, Clone)]
pub(crate) enum HumanReviewEvent {
    SourceTrace {
        project_id: String,
        logical_trace_id: String,
        revision: u64,
        selected_span_id: Option<String>,
    },
}

pub(crate) struct HumanReviewScreen {
    service: Arc<LiveTraceService>,
    project_id: Option<String>,
    mode: ReviewModeV1,
    tasks: Vec<ReviewTaskV1>,
    selected_task_id: Option<String>,
    blind_selected_task_id: Option<String>,
    visible_selected_task_id: Option<String>,
    presentation: Option<ReviewTaskPresentationV1>,
    adjudication: Option<ReviewAdjudicationPacketV1>,
    label: Option<AnnotationLabelV1>,
    evidence_keys: Vec<String>,
    explanation: Entity<TextInput>,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    request_generation: u64,
}

impl EventEmitter<HumanReviewEvent> for HumanReviewScreen {}

impl HumanReviewScreen {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let explanation = cx.new(|cx| TextInput::new("Why is this the correct answer?", 4_096, cx));
        let mut this = Self {
            service,
            project_id,
            mode: ReviewModeV1::BlindCalibration,
            tasks: Vec::new(),
            selected_task_id: None,
            blind_selected_task_id: None,
            visible_selected_task_id: None,
            presentation: None,
            adjudication: None,
            label: None,
            evidence_keys: Vec::new(),
            explanation,
            busy: false,
            error: None,
            notice: None,
            request_generation: 0,
        };
        this.reload(cx);
        this
    }

    pub(crate) fn set_project_scope(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        if self.project_id == project_id {
            return;
        }
        self.project_id = project_id;
        self.selected_task_id = None;
        self.blind_selected_task_id = None;
        self.visible_selected_task_id = None;
        self.presentation = None;
        self.adjudication = None;
        self.reload(cx);
    }

    fn set_mode(&mut self, mode: ReviewModeV1, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        match self.mode {
            ReviewModeV1::BlindCalibration => {
                self.blind_selected_task_id = self.selected_task_id.clone()
            }
            ReviewModeV1::VisibleTriage => {
                self.visible_selected_task_id = self.selected_task_id.clone()
            }
        }
        self.mode = mode;
        self.selected_task_id = match mode {
            ReviewModeV1::BlindCalibration => self.blind_selected_task_id.clone(),
            ReviewModeV1::VisibleTriage => self.visible_selected_task_id.clone(),
        };
        self.presentation = None;
        self.adjudication = None;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.error = None;
        let Some(project_id) = self.project_id.clone() else {
            self.tasks.clear();
            cx.notify();
            return;
        };
        self.busy = true;
        let service = self.service.clone();
        let mode = self.mode;
        let task =
            cx.background_spawn(async move { service.list_review_tasks(&project_id, Some(mode)) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(tasks) => {
                        this.tasks = tasks;
                        if let Some(index) = this.selected_task_id.as_deref().and_then(|task_id| {
                            this.tasks.iter().position(|task| task.task_id == task_id)
                        }) {
                            this.select_task(index, cx);
                        } else {
                            this.selected_task_id = None;
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn select_task(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.get(index) else {
            return;
        };
        let task_id = task.task_id.clone();
        let adjudicating = task.status == ReviewTaskStatusV1::AwaitingAdjudication;
        self.selected_task_id = Some(task_id.clone());
        match self.mode {
            ReviewModeV1::BlindCalibration => self.blind_selected_task_id = Some(task_id),
            ReviewModeV1::VisibleTriage => self.visible_selected_task_id = Some(task_id),
        }
        self.presentation = None;
        self.adjudication = None;
        self.label = None;
        self.evidence_keys.clear();
        self.notice = None;
        self.error = None;
        self.explanation
            .update(cx, |input, cx| input.set_text("", cx));
        if adjudicating {
            self.load_adjudication(cx);
        } else {
            self.load_presentation(false, cx);
        }
    }

    fn load_adjudication(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected_task_id.clone() else {
            return;
        };
        let expected_task_id = task_id.clone();
        self.busy = true;
        let service = self.service.clone();
        let task = cx.background_spawn(async move { service.review_adjudication_packet(&task_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.selected_task_id.as_deref() != Some(expected_task_id.as_str()) {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(packet) => this.adjudication = Some(packet),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_presentation(&mut self, report_unassigned: bool, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected_task_id.clone() else {
            return;
        };
        let expected_task_id = task_id.clone();
        self.busy = true;
        let service = self.service.clone();
        let task = cx.background_spawn(async move { service.review_task_for_reviewer(&task_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.selected_task_id.as_deref() != Some(expected_task_id.as_str()) {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(presentation) => {
                        if let Some(annotation) =
                            presentation_latest_annotation(Some(&presentation)).cloned()
                        {
                            this.label = Some(annotation.label);
                            this.evidence_keys = annotation.evidence_keys;
                            this.explanation
                                .update(cx, |input, cx| input.set_text(annotation.explanation, cx));
                        }
                        this.presentation = Some(presentation);
                        this.notice = None;
                    }
                    Err(error)
                        if !report_unassigned && error.to_string().contains("not assigned") =>
                    {
                        this.presentation = None;
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn claim(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected_task_id.clone() else {
            return;
        };
        let service = self.service.clone();
        let expected_task_id = task_id.clone();
        self.busy = true;
        self.error = None;
        let task = cx.background_spawn(async move { service.assign_review_task(&task_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.selected_task_id.as_deref() != Some(expected_task_id.as_str()) {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(_) => {
                        this.notice = Some(
                            "Case claimed. The model answer remains sealed until you submit."
                                .into(),
                        );
                        this.load_presentation(true, cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_queue(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            service.create_review_queue_from_completed_assessments(&project_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((_, task_count)) => {
                        this.notice = Some(format!(
                            "Created a group-safe blind queue with {task_count} exact trace revisions."
                        ));
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_label(&mut self, label: AnnotationLabelV1, cx: &mut Context<Self>) {
        self.label = Some(label);
        if label == AnnotationLabelV1::Abstain {
            self.evidence_keys.clear();
        }
        cx.notify();
    }

    fn toggle_evidence(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(index) = self.evidence_keys.iter().position(|value| value == key) {
            self.evidence_keys.remove(index);
        } else {
            self.evidence_keys.push(key.to_owned());
        }
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(task_id) = self.selected_task_id.clone() else {
            return;
        };
        let Some(label) = self.label else { return };
        let explanation = self.explanation.read(cx).text().trim().to_owned();
        let expected_head = self
            .adjudication
            .as_ref()
            .and_then(|packet| packet.latest_adjudication.as_ref())
            .map(|adjudication| adjudication.revision_id.clone())
            .or_else(|| {
                presentation_latest_annotation(self.presentation.as_ref())
                    .map(|annotation| annotation.revision_id.clone())
            });
        let service = self.service.clone();
        let evidence = self.evidence_keys.clone();
        let adjudication_revision_ids = self.adjudication.as_ref().map(|packet| {
            packet
                .annotation_revisions
                .iter()
                .map(|annotation| annotation.revision_id.clone())
                .collect::<Vec<_>>()
        });
        self.busy = true;
        self.error = None;
        let task = cx.background_spawn(async move {
            if let Some(annotation_revision_ids) = adjudication_revision_ids {
                service
                    .adjudicate_review_task(
                        &task_id,
                        &annotation_revision_ids,
                        expected_head.as_deref(),
                        label,
                        &explanation,
                        &evidence,
                    )
                    .map(|adjudication| (adjudication.adjudication_revision, true))
            } else {
                service
                    .submit_annotation_revision(
                        &task_id,
                        expected_head.as_deref(),
                        label,
                        &explanation,
                        &evidence,
                    )
                    .map(|annotation| (annotation.annotation_revision, false))
            }
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((revision, adjudicated)) => {
                        this.notice = Some(if adjudicated {
                            format!(
                                "Disagreement resolved as adjudication revision {revision}. The judge stayed sealed during the decision."
                            )
                        } else {
                            format!(
                                "Answer saved as annotation revision {revision}. The original remains immutable."
                            )
                        });
                        if adjudicated {
                            this.adjudication = None;
                            this.selected_task_id = None;
                        } else {
                            this.load_presentation(true, cx);
                        }
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_trace(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task() else {
            return;
        };
        cx.emit(HumanReviewEvent::SourceTrace {
            project_id: task.project_id.clone(),
            logical_trace_id: task.logical_trace_id.clone(),
            revision: task.revision,
            selected_span_id: self
                .evidence_keys
                .first()
                .and_then(|key| key.strip_prefix("span:"))
                .map(str::to_owned),
        });
    }

    fn selected_task(&self) -> Option<&ReviewTaskV1> {
        let id = self.selected_task_id.as_deref()?;
        self.tasks.iter().find(|task| task.task_id == id)
    }

    fn render_task_list(&self, cx: &mut Context<Self>) -> Div {
        let random_audit_count = self
            .tasks
            .iter()
            .filter(|task| {
                task.selection_reason == perseval_service::ReviewSelectionReasonV1::RandomAudit
            })
            .count();
        let active_selection_count = self
            .tasks
            .iter()
            .filter(|task| {
                task.selection_reason == perseval_service::ReviewSelectionReasonV1::ActiveLearning
            })
            .count();
        let rows =
            self.tasks
                .iter()
                .enumerate()
                .fold(div().flex().flex_col(), |rows, (index, task)| {
                    let selected = self.selected_task_id.as_deref() == Some(task.task_id.as_str());
                    rows.child(
                        div()
                            .id(("human-review-task", index))
                            .role(Role::Button)
                            .aria_label(format!(
                                "Review trace {} revision {}; {}; {}; {} reviewers; {}",
                                task.logical_trace_id,
                                task.revision,
                                split_label(task),
                                selection_label(task),
                                task.required_reviewers,
                                status_label(task.status)
                            ))
                            .tab_index(0)
                            .focus_visible(|style| style.border_2().border_color(Theme::FOCUS_RING))
                            .cursor_pointer()
                            .px_3()
                            .py_3()
                            .border_b_1()
                            .border_color(Theme::BORDER)
                            .when(selected, |row| row.bg(Theme::SELECTED))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.select_task(index, cx)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                                        format!(
                                            "Trace {} · r{}",
                                            short_id(&task.logical_trace_id),
                                            task.revision
                                        ),
                                    ))
                                    .child(tag(
                                        status_label(task.status),
                                        status_tint(task.status),
                                    )),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(Theme::MUTED)
                                    .child(format!(
                                        "{} · {} · {} reviewers",
                                        split_label(task),
                                        selection_label(task),
                                        task.required_reviewers
                                    )),
                            ),
                    )
                });
        div()
            .w(px(330.))
            .flex_none()
            .border_r_1()
            .border_color(Theme::BORDER)
            .child(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child(format!(
                        "{} cases · {} random audit · {} active",
                        self.tasks.len(),
                        random_audit_count,
                        active_selection_count
                    )),
            )
            .child(
                div()
                    .id("human-review-task-scroll")
                    .max_h_full()
                    .overflow_y_scroll()
                    .child(rows),
            )
            .when(self.mode == ReviewModeV1::BlindCalibration, |list| {
                list.child(
                    div().p_3().border_t_1().border_color(Theme::BORDER).child(
                        button_state("Resume blind calibration queue", false, !self.busy)
                            .id("resume-human-review-queue")
                            .role(Role::Button)
                            .aria_label("Resume blind calibration queue")
                            .on_click(cx.listener(|this, _, _, cx| this.create_queue(cx))),
                    ),
                )
            })
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(_task) = self.selected_task() else {
            let empty = self.tasks.is_empty();
            let blind_mode = self.mode == ReviewModeV1::BlindCalibration;
            let empty_label = if empty {
                if blind_mode {
                    "No blind calibration cases exist for this project. Start from completed learned assessments. Perseval freezes trace-level leakage groups and keeps a held-out test split."
                } else {
                    "No visible triage cases exist for this project. Visible triage is deliberately excluded from calibration and agreement. Operator-opened cases appear here. Uncertainty-selected blind cases stay in Blind calibration and remain separate from the random-audit population."
                }
            } else {
                "Choose a case to review its trace evidence."
            };
            return div()
                .id("human-review-empty-state")
                .role(Role::Status)
                .aria_label(empty_label)
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(Theme::MUTED)
                .child(if empty {
                    if blind_mode {
                        "No blind calibration cases exist for this project."
                    } else {
                        "No visible triage cases exist for this project."
                    }
                } else {
                    "Choose a case to review its trace evidence."
                })
                .when(empty && blind_mode, |view| {
                    view.child(
                        div()
                            .mt_2()
                            .max_w(px(520.))
                            .text_center()
                            .child("Start from completed learned assessments. Perseval freezes trace-level leakage groups and keeps a held-out test split."),
                    )
                    .child(
                        button_state("Create blind review queue", true, !self.busy)
                            .id("create-human-review-queue")
                            .role(Role::Button)
                            .aria_label("Create blind calibration review queue")
                            .mt_4()
                            .on_click(cx.listener(|this, _, _, cx| this.create_queue(cx))),
                    )
                })
                .when(empty && !blind_mode, |view| {
                    view.child(
                        div()
                            .mt_2()
                            .max_w(px(520.))
                            .text_center()
                            .child(
                                "Visible triage is deliberately excluded from calibration and agreement. Operator-opened cases appear here. Uncertainty-selected blind cases stay in Blind calibration and remain separate from the random-audit population.",
                            ),
                    )
                })
                .into_any_element();
        };
        let mode_blind = self.mode == ReviewModeV1::BlindCalibration;
        let adjudicating = self.adjudication.is_some();
        let detail_heading = if adjudicating {
            "Resolve reviewer disagreement"
        } else if !mode_blind {
            "Triage automated review"
        } else {
            "Independent human answer"
        };
        let detail_guidance = if adjudicating {
            "Compare the two human answers. The learned judge remains sealed until adjudication is committed."
        } else if mode_blind {
            "Judge output and peer answers stay sealed until submission."
        } else {
            "Visible triage is excluded from calibration and agreement."
        };
        let mut detail = div()
            .id("human-review-detail-scroll")
            .flex_1()
            .min_w_0()
            .p_6()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(
                        div()
                            .id("human-review-detail-heading")
                            .role(Role::Group)
                            .aria_label(format!("{detail_heading}. {detail_guidance}"))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(detail_heading),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .text_color(Theme::MUTED)
                                    .child(detail_guidance),
                            ),
                    )
                    .child(
                        button_state("Open exact trace", false, !self.busy)
                            .id("open-human-review-trace")
                            .role(Role::Button)
                            .aria_label("Open the exact frozen trace revision for this case")
                            .on_click(cx.listener(|this, _, _, cx| this.open_trace(cx))),
                    ),
            );

        if let Some(error) = &self.error {
            detail = detail.child(
                div()
                    .mt_4()
                    .p_3()
                    .rounded_sm()
                    .bg(Theme::DANGER_SURFACE)
                    .text_sm()
                    .text_color(Theme::RED)
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.notice {
            detail = detail.child(
                div()
                    .mt_4()
                    .p_3()
                    .rounded_sm()
                    .bg(Theme::SUCCESS_SURFACE)
                    .text_sm()
                    .text_color(Theme::GREEN)
                    .child(notice.clone()),
            );
        }
        if let Some(schema) = self
            .adjudication
            .as_ref()
            .map(|packet| &packet.annotation_schema)
            .or_else(|| self.presentation.as_ref().map(presentation_schema))
        {
            detail = detail.child(
                div()
                    .mt_5()
                    .id("human-review-rubric")
                    .role(Role::Group)
                    .aria_label(format!(
                        "Review rubric. {} Positive class: {}. Schema {}.",
                        schema.instructions, schema.positive_class, schema.schema_version
                    ))
                    .p_4()
                    .rounded_sm()
                    .border_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL_SURFACE)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::MUTED)
                            .child("REVIEW RUBRIC"),
                    )
                    .child(div().mt_2().text_sm().child(schema.instructions.clone()))
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(format!(
                                "Positive class: {} · schema {}",
                                schema.positive_class, schema.schema_version
                            )),
                    ),
            );
        }
        if self.presentation.is_none() && self.adjudication.is_none() {
            return detail.child(div().mt_8().max_w(px(560.)).child(div().text_sm()
                    .child("Claiming binds this case to your reviewer identity. Another independent reviewer must use a different identity."))
                .child(button_state(if mode_blind { "Claim blind review" } else { "Claim triage case" }, true, !self.busy)
                    .id("claim-human-review")
                    .role(Role::Button)
                    .aria_label(if mode_blind { "Claim blind review" } else { "Claim visible triage case" })
                    .mt_4()
                    .on_click(cx.listener(|this, _, _, cx| this.claim(cx))))).into_any_element();
        }

        let available_evidence = self.adjudication.as_ref().map_or_else(
            || {
                presentation_evidence_keys(
                    self.presentation
                        .as_ref()
                        .expect("presentation or adjudication"),
                )
            },
            |packet| packet.evidence_keys.as_slice(),
        );
        let latest = presentation_latest_annotation(self.presentation.as_ref());
        let blind_answer_locked = self.mode == ReviewModeV1::BlindCalibration
            && matches!(
                self.presentation.as_ref(),
                Some(ReviewTaskPresentationV1::Revealed(_))
            )
            && !adjudicating;
        if let Some(packet) = &self.adjudication {
            detail = detail
                .child(
                    div()
                        .mt_6()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(Theme::MUTED)
                        .child("INDEPENDENT ANSWERS"),
                )
                .child(
                    div()
                        .mt_2()
                        .children(packet.annotation_revisions.iter().map(|annotation| {
                            div()
                                .py_3()
                                .border_b_1()
                                .border_color(Theme::BORDER)
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!(
                                            "Reviewer {}",
                                            short_id(&annotation.reviewer_id)
                                        ))
                                        .child(label_name(annotation.label)),
                                )
                                .child(
                                    div()
                                        .mt_1()
                                        .text_sm()
                                        .text_color(Theme::MUTED)
                                        .child(annotation.explanation.clone()),
                                )
                        })),
                );
        }
        detail = detail
            .child(
                div()
                    .mt_6()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::MUTED)
                    .child("YOUR ANSWER"),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .id("human-review-answer-options")
                    .role(Role::RadioGroup)
                    .aria_label("Human review answer")
                    .children(
                        [
                            AnnotationLabelV1::Completed,
                            AnnotationLabelV1::Partial,
                            AnnotationLabelV1::Failed,
                            AnnotationLabelV1::Abstain,
                        ]
                        .into_iter()
                        .map(|label| {
                            let selected = self.label == Some(label);
                            button_state(
                                label_name(label),
                                selected,
                                !self.busy && !blind_answer_locked,
                            )
                            .id(("human-answer-label", label_ordinal(label)))
                            .role(Role::RadioButton)
                            .aria_label(format!(
                                "Answer {}{}",
                                label_name(label),
                                if selected { ", selected" } else { "" }
                            ))
                            .aria_selected(selected)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.choose_label(label, cx)),
                            )
                        }),
                    ),
            )
            .child(
                div()
                    .mt_4()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::MUTED)
                    .child("EXPLANATION"),
            )
            .child(div().mt_2().child(self.explanation.clone()))
            .child(
                div()
                    .mt_5()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::MUTED)
                    .child("CITE TRACE EVIDENCE"),
            )
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("Select one or more locations from the frozen safe projection."),
            )
            .child(
                div().mt_2().flex().flex_wrap().gap_2().children(
                    available_evidence
                        .iter()
                        .take(40)
                        .enumerate()
                        .map(|(index, key)| {
                            let selected = self.evidence_keys.iter().any(|value| value == key);
                            let key = key.clone();
                            let evidence_label = evidence_key_label(&key);
                            button_state(
                                &evidence_label,
                                selected,
                                !self.busy && !blind_answer_locked,
                            )
                            .id(("review-evidence", index))
                            .role(Role::CheckBox)
                            .aria_label(format!(
                                "Evidence {evidence_label}{}",
                                if selected { ", selected" } else { "" }
                            ))
                            .aria_toggled(if selected {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_evidence(&key, cx)),
                            )
                        }),
                ),
            );

        if let Some(ReviewTaskPresentationV1::Revealed(view)) = self.presentation.as_ref() {
            let automated_accessible_label = view.assessment.evaluation.as_ref().map_or_else(
                || {
                    format!(
                        "Automated output revealed after your answer. Provider run status {:?}. No learned-evaluator output was reported.",
                        view.assessment.status
                    )
                },
                |evaluation| {
                    format!(
                        "Automated output revealed after your answer. Provider run status {:?}. Learned verdict {}. Raw score {}.",
                        view.assessment.status,
                        evaluation.label.as_deref().unwrap_or("not reported"),
                        evaluation
                            .score
                            .map_or_else(|| "not reported".into(), |score| format!("{score:.3}"))
                    )
                },
            );
            detail = detail.child(
                div()
                    .mt_6()
                    .pt_5()
                    .id("human-review-automated-output")
                    .role(Role::Group)
                    .aria_label(automated_accessible_label)
                    .border_t_1()
                    .border_color(Theme::BORDER)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::MUTED)
                            .child("AUTOMATED OUTPUT · REVEALED AFTER YOUR ANSWER"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .child(format!("Provider run: {:?}", view.assessment.status)),
                    )
                    .child(div().mt_1().text_sm().text_color(Theme::MUTED).child(
                        view.assessment.evaluation.as_ref().map_or_else(
                            || "No learned-evaluator output was reported.".into(),
                            |evaluation| {
                                format!(
                                    "{} · raw score {}",
                                    evaluation.label.as_deref().unwrap_or("not reported"),
                                    evaluation.score.map_or_else(
                                        || "not reported".into(),
                                        |score| format!("{score:.3}")
                                    ),
                                )
                            },
                        ),
                    )),
            );
        }

        let explanation_ready = !self.explanation.read(cx).text().trim().is_empty();
        let evidence_ready =
            self.label == Some(AnnotationLabelV1::Abstain) || !self.evidence_keys.is_empty();
        let enabled = !self.busy
            && !blind_answer_locked
            && self.label.is_some()
            && explanation_ready
            && evidence_ready;
        let answer_status = if adjudicating {
            self.adjudication
                .as_ref()
                .and_then(|packet| packet.latest_adjudication.as_ref())
                .map_or_else(
                    || "No adjudication committed yet.".into(),
                    |adjudication| {
                        format!(
                            "Prior adjudication revision {} is stale and retained",
                            adjudication.adjudication_revision
                        )
                    },
                )
        } else {
            latest.map_or_else(
                || "No answer submitted yet.".into(),
                |annotation| {
                    format!(
                        "Current immutable revision {} · {}",
                        annotation.annotation_revision,
                        label_name(annotation.label)
                    )
                },
            )
        };
        detail
            .child(
                div()
                    .mt_6()
                    .pt_4()
                    .border_t_1()
                    .border_color(Theme::BORDER)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("human-review-answer-status")
                            .role(Role::Status)
                            .aria_label(answer_status.clone())
                            .child(answer_status),
                    )
                    .child(
                        button_state(
                            if adjudicating {
                                "Submit adjudication"
                            } else if blind_answer_locked {
                                "Answer locked after reveal"
                            } else if latest.is_some() {
                                "Save correction"
                            } else {
                                "Submit answer"
                            },
                            true,
                            enabled,
                        )
                        .id("submit-human-answer")
                        .role(Role::Button)
                        .aria_label(if adjudicating {
                            "Submit adjudication"
                        } else if blind_answer_locked {
                            "Answer locked after automated output reveal"
                        } else if latest.is_some() {
                            "Save review correction"
                        } else {
                            "Submit independent human answer"
                        })
                        .when(enabled, |button| {
                            button.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
                        }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for HumanReviewScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .h(px(58.))
            .flex_none()
            .px_5()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Review Queue"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child("Build independent truth from exact trace revisions"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .id("review-mode-options")
                    .role(Role::RadioGroup)
                    .aria_label("Review queue mode")
                    .child(
                        button_state(
                            "Blind calibration",
                            self.mode == ReviewModeV1::BlindCalibration,
                            !self.busy,
                        )
                        .id("review-mode-blind")
                        .role(Role::RadioButton)
                        .aria_label("Blind calibration; automated output and peer answers remain sealed until submission")
                        .aria_selected(self.mode == ReviewModeV1::BlindCalibration)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_mode(ReviewModeV1::BlindCalibration, cx)
                        })),
                    )
                    .child(
                        button_state(
                            "Visible triage",
                            self.mode == ReviewModeV1::VisibleTriage,
                            !self.busy,
                        )
                        .id("review-mode-visible")
                        .role(Role::RadioButton)
                        .aria_label("Visible triage; excluded from calibration and agreement")
                        .aria_selected(self.mode == ReviewModeV1::VisibleTriage)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_mode(ReviewModeV1::VisibleTriage, cx)
                        })),
                    ),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_task_list(cx))
                    .child(self.render_detail(cx)),
            )
    }
}

fn presentation_evidence_keys(presentation: &ReviewTaskPresentationV1) -> &[String] {
    match presentation {
        ReviewTaskPresentationV1::Blind(view) => &view.evidence_keys,
        ReviewTaskPresentationV1::Revealed(view) => &view.evidence_keys,
    }
}

fn evidence_key_label(key: &str) -> String {
    if let Some(span_id) = key.strip_prefix("span:") {
        return format!("Trace span {}", short_id(span_id));
    }
    if let Some(index) = key.strip_prefix("terminal-output:") {
        return index
            .parse::<usize>()
            .map(|index| format!("Terminal output {}", index + 1))
            .unwrap_or_else(|_| "Terminal output".into());
    }
    if let Some(tool_call) = key.strip_prefix("tool-call:") {
        return format!("Tool call {}", short_id(tool_call));
    }
    key.chars()
        .map(|character| match character {
            ':' | '-' | '_' => ' ',
            other => other,
        })
        .collect()
}

fn presentation_schema(
    presentation: &ReviewTaskPresentationV1,
) -> &perseval_service::AnnotationSchemaReleaseV1 {
    match presentation {
        ReviewTaskPresentationV1::Blind(view) => &view.annotation_schema,
        ReviewTaskPresentationV1::Revealed(view) => &view.annotation_schema,
    }
}

fn presentation_latest_annotation(
    presentation: Option<&ReviewTaskPresentationV1>,
) -> Option<&perseval_service::AnnotationRevisionV1> {
    match presentation? {
        ReviewTaskPresentationV1::Blind(view) => view.latest_annotation.as_ref(),
        ReviewTaskPresentationV1::Revealed(view) => view.latest_annotation.as_ref(),
    }
}

fn label_name(label: AnnotationLabelV1) -> &'static str {
    match label {
        AnnotationLabelV1::Completed => "Completed",
        AnnotationLabelV1::Partial => "Partial",
        AnnotationLabelV1::Failed => "Failed",
        AnnotationLabelV1::Abstain => "Abstain",
    }
}

fn label_ordinal(label: AnnotationLabelV1) -> u32 {
    match label {
        AnnotationLabelV1::Completed => 0,
        AnnotationLabelV1::Partial => 1,
        AnnotationLabelV1::Failed => 2,
        AnnotationLabelV1::Abstain => 3,
    }
}

fn status_label(status: ReviewTaskStatusV1) -> &'static str {
    match status {
        ReviewTaskStatusV1::Pending => "PENDING",
        ReviewTaskStatusV1::InReview => "IN REVIEW",
        ReviewTaskStatusV1::AwaitingAdjudication => "ADJUDICATE",
        ReviewTaskStatusV1::Completed => "COMPLETE",
        ReviewTaskStatusV1::Cancelled => "CANCELLED",
    }
}

fn status_tint(status: ReviewTaskStatusV1) -> gpui::Rgba {
    match status {
        ReviewTaskStatusV1::Pending => Theme::DIM,
        ReviewTaskStatusV1::InReview => Theme::AMBER,
        ReviewTaskStatusV1::AwaitingAdjudication => Theme::RED,
        ReviewTaskStatusV1::Completed => Theme::GREEN,
        ReviewTaskStatusV1::Cancelled => Theme::MUTED,
    }
}

fn selection_label(task: &ReviewTaskV1) -> &'static str {
    use perseval_service::ReviewSelectionReasonV1::*;
    match task.selection_reason {
        RandomAudit => "random audit",
        ActiveLearning => "active selection",
        Manual => "manual",
    }
}

fn split_label(task: &ReviewTaskV1) -> &'static str {
    match task.split {
        perseval_service::CalibrationDataSplitV1::Train => "fit",
        perseval_service::CalibrationDataSplitV1::Calibration => "calibration",
        perseval_service::CalibrationDataSplitV1::Test => "held-out test",
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(12)).unwrap_or(value)
}

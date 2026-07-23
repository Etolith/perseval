use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AppContext, Context, Div, Entity, FontWeight, IntoElement, Render, Role, Window, div,
    prelude::*, px,
};
use perseval_service::analysis::{
    EvaluationImplementationV1, TaskCompletionContentPolicyV1, TaxonomyDimensionV1,
};
use perseval_service::{
    ASSESSMENT_SAMPLING_POLICY_SCHEMA_VERSION, AssessmentBackfillPreviewV1, AssessmentJobV1,
    AssessmentSamplingPolicyV1, LiveTraceService, LocalTaskCompletionModelV1, ReviewAuthorityV1,
    TaskCompletionExecutionRouteV1, TaskCompletionQualityCheckDraftV1,
    TaskCompletionQualityCheckV1, TaxonomyGovernanceSummaryV1,
};

use crate::components::{TextInput, button_state, tag};
use crate::design::Theme;

pub(crate) struct QualityCheckStudio {
    service: Arc<LiveTraceService>,
    project_id: Option<String>,
    reviewer_ref: String,
    quality_checks: Vec<TaskCompletionQualityCheckV1>,
    selected_release_id: Option<String>,
    jobs: Vec<AssessmentJobV1>,
    preview: Option<AssessmentBackfillPreviewV1>,
    taxonomy: TaxonomyGovernanceSummaryV1,
    execution_route: TaskCompletionExecutionRouteV1,
    local_model: Option<LocalTaskCompletionModelV1>,
    name: Entity<TextInput>,
    criteria: Entity<TextInput>,
    model: Entity<TextInput>,
    pricing_version: Entity<TextInput>,
    input_rate: Entity<TextInput>,
    output_rate: Entity<TextInput>,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    request_generation: u64,
}

impl QualityCheckStudio {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        project_id: Option<String>,
        reviewer_ref: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let local_model = service.local_task_completion_model();
        let execution_route = if local_model.is_some() {
            TaskCompletionExecutionRouteV1::LocalOnnx
        } else {
            TaskCompletionExecutionRouteV1::HostedOpenAi
        };
        let name = input("Task completion", "Quality check name", 160, cx);
        let criteria = input(
            "Return completed, partial, failed, or abstain. Evaluate each declared success criterion and cite every decision to exact observed trace evidence.",
            "Review criteria",
            4_096,
            cx,
        );
        let model = input("gpt-4.1-mini", "Provider model", 160, cx);
        let pricing_version = input("", "Rate-card version", 160, cx);
        let input_rate = input("", "Input $/1M tokens in micros", 24, cx);
        let output_rate = input("", "Output $/1M tokens in micros", 24, cx);
        let mut studio = Self {
            service,
            project_id,
            reviewer_ref,
            quality_checks: Vec::new(),
            selected_release_id: None,
            jobs: Vec::new(),
            preview: None,
            taxonomy: TaxonomyGovernanceSummaryV1::default(),
            execution_route,
            local_model,
            name,
            criteria,
            model,
            pricing_version,
            input_rate,
            output_rate,
            busy: false,
            error: None,
            notice: None,
            request_generation: 0,
        };
        studio.reload(cx);
        studio
    }

    pub(crate) fn set_project_scope(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        if self.project_id == project_id {
            return;
        }
        self.project_id = project_id;
        self.selected_release_id = None;
        self.preview = None;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.error = None;
        let Some(project_id) = self.project_id.clone() else {
            self.quality_checks.clear();
            self.jobs.clear();
            self.taxonomy = TaxonomyGovernanceSummaryV1::default();
            cx.notify();
            return;
        };
        self.busy = true;
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            let checks = service.list_task_completion_quality_checks(&project_id)?;
            let jobs = service.list_assessment_jobs(&project_id, None, 0, 100)?;
            let taxonomy = service.taxonomy_governance_summary(&project_id)?;
            let context = service.agent_context_governance_summary(&project_id)?;
            Ok::<_, perseval_service::LiveServiceError>((checks, jobs, taxonomy, context))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.busy = false;
                match result {
                    Ok((checks, jobs, taxonomy, context)) => {
                        this.quality_checks = checks;
                        this.jobs = jobs;
                        this.taxonomy = taxonomy;
                        if this.selected_release_id.as_ref().is_none_or(|selected| {
                            !this
                                .quality_checks
                                .iter()
                                .any(|check| check.config.evaluator_release_id == *selected)
                        }) {
                            this.selected_release_id = this
                                .quality_checks
                                .first()
                                .map(|check| check.config.evaluator_release_id.clone());
                        }
                        if context.latest_context_release_id.is_none() {
                            this.notice = Some(
                                "Set up the agent in Settings before creating a quality check."
                                    .into(),
                            );
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

    fn selected_check(&self) -> Option<&TaskCompletionQualityCheckV1> {
        let release_id = self.selected_release_id.as_deref()?;
        self.quality_checks
            .iter()
            .find(|check| check.config.evaluator_release_id == release_id)
    }

    fn select_check(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(check) = self.quality_checks.get(index) else {
            return;
        };
        self.selected_release_id = Some(check.config.evaluator_release_id.clone());
        self.preview = None;
        self.notice = None;
        cx.notify();
    }

    fn select_execution_route(
        &mut self,
        route: TaskCompletionExecutionRouteV1,
        cx: &mut Context<Self>,
    ) {
        if route == TaskCompletionExecutionRouteV1::LocalOnnx && self.local_model.is_none() {
            self.error = Some(
                "Configure a verified local model artifact in Settings, then restart Perseval."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.execution_route = route;
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(project_id) = self.project_id.clone() else {
            self.error = Some("Choose one project before publishing a quality check.".into());
            cx.notify();
            return;
        };
        let service = self.service.clone();
        let reviewer_ref = self.reviewer_ref.clone();
        let name = self.name.read(cx).text().trim().to_string();
        let execution_route = self.execution_route;
        let (review_criteria, requested_model, pricing_version, input_rate, output_rate) =
            match execution_route {
                TaskCompletionExecutionRouteV1::LocalOnnx => {
                    (String::new(), String::new(), String::new(), 0, 0)
                }
                TaskCompletionExecutionRouteV1::HostedOpenAi => {
                    let input_rate = self.input_rate.read(cx).text().trim().parse::<u64>();
                    let output_rate = self.output_rate.read(cx).text().trim().parse::<u64>();
                    let (Ok(input_rate), Ok(output_rate)) = (input_rate, output_rate) else {
                        self.error = Some(
                        "Enter the exact input and output rate-card amounts in micros per million tokens."
                            .into(),
                    );
                        cx.notify();
                        return;
                    };
                    (
                        self.criteria.read(cx).text().trim().to_string(),
                        self.model.read(cx).text().trim().to_string(),
                        self.pricing_version.read(cx).text().trim().to_string(),
                        input_rate,
                        output_rate,
                    )
                }
            };
        self.busy = true;
        self.error = None;
        self.notice = Some("Creating the quality check…".into());
        let taxonomy_node_ids = self
            .taxonomy
            .active_nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.dimension,
                    TaxonomyDimensionV1::Task
                        | TaxonomyDimensionV1::Capability
                        | TaxonomyDimensionV1::Policy
                )
            })
            .map(|node| node.node_id.clone())
            .collect::<BTreeSet<_>>();
        let task = cx.background_spawn(async move {
            let context = service.agent_context_governance_summary(&project_id)?;
            let context_release_id = context.latest_context_release_id.ok_or_else(|| {
                perseval_service::LiveServiceError::InvalidInput(
                    "approve an agent specification before publishing".into(),
                )
            })?;
            let draft = TaskCompletionQualityCheckDraftV1 {
                name,
                review_criteria,
                execution_route,
                requested_model,
                context_release_id,
                applicable_taxonomy_node_ids: taxonomy_node_ids,
                content_policy: TaskCompletionContentPolicyV1::PreRedactedSummaries,
                estimated_output_tokens_low: 96,
                estimated_output_tokens_high: 384,
                input_cost_micros_per_million_tokens: input_rate,
                output_cost_micros_per_million_tokens: output_rate,
                pricing_version,
            };
            service.publish_task_completion_quality_check(
                &project_id,
                &draft,
                &reviewer_ref,
                ReviewAuthorityV1::Human,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(release_id) => {
                        this.selected_release_id = Some(release_id.clone());
                        this.notice = Some(format!(
                            "Quality check {} is ready. Preview the traces before running it.",
                            short_id(&release_id)
                        ));
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn preview(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(project_id) = self.project_id.clone() else {
            return;
        };
        let Some(check) = self.selected_check() else {
            return;
        };
        let release_id = check.config.evaluator_release_id.clone();
        let context_release_id = check.config.context_release_id.clone();
        self.busy = true;
        self.error = None;
        self.notice = Some("Preparing the trace preview…".into());
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            let targets = service.preview_context_backfill(&project_id, &context_release_id)?;
            service.preview_assessment_backfill(
                &project_id,
                &release_id,
                &targets.affected_revisions,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(preview) => {
                        this.notice =
                            Some("Preview ready. Perseval will recheck it before the run.".into());
                        this.preview = Some(preview);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn run_preview(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (Some(project_id), Some(preview)) = (self.project_id.clone(), self.preview.clone())
        else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = Some("Starting the quality check…".into());
        let service = self.service.clone();
        let exact_revisions = preview
            .targets
            .iter()
            .map(|target| (target.logical_trace_id.clone(), target.revision))
            .collect::<Vec<_>>();
        let idempotency_key = format!(
            "studio:{}:{}",
            short_id(&preview.evaluator_release_id),
            preview.selection_hash
        );
        let task = cx.background_spawn(async move {
            service.enqueue_assessment_job_from_preview(
                &project_id,
                &preview.evaluator_release_id,
                &exact_revisions,
                &preview.selection_hash,
                &idempotency_key,
            )
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(job) => {
                        this.notice = Some(format!(
                            "Run {} started for {} trace(s).",
                            short_id(&job.job_id),
                            job.item_count
                        ));
                        this.reload(cx);
                        this.watch_job(job.job_id, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "The traces changed before the run. Preview them again: {error}"
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn watch_job(&mut self, job_id: String, cx: &mut Context<Self>) {
        let Some(project_id) = self.project_id.clone() else {
            return;
        };
        let service = self.service.clone();
        let executor = cx.background_executor().clone();
        let task = cx.background_spawn(async move {
            loop {
                let job = service
                    .assessment_job(&project_id, &job_id)?
                    .ok_or_else(|| {
                        perseval_service::LiveServiceError::InvalidInput(
                            "the assessment run disappeared before completion".into(),
                        )
                    })?;
                if job.terminal_count >= job.item_count {
                    return Ok::<_, perseval_service::LiveServiceError>(job);
                }
                executor.timer(Duration::from_millis(250)).await;
            }
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(job) => {
                        this.notice = Some(format!(
                            "Run {} finished: {} of {} traces reached a final result.",
                            short_id(&job.job_id),
                            job.terminal_count,
                            job.item_count
                        ));
                        this.reload(cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not refresh the quality check: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_sampling(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (Some(project_id), Some(check)) = (self.project_id.clone(), self.selected_check())
        else {
            return;
        };
        let enabled = !check
            .sampling_policy
            .as_ref()
            .is_some_and(|policy| policy.enabled);
        let release_id = check.config.evaluator_release_id.clone();
        let policy = AssessmentSamplingPolicyV1 {
            schema_version: ASSESSMENT_SAMPLING_POLICY_SCHEMA_VERSION.into(),
            project_id,
            evaluator_release_id: release_id,
            enabled,
            sample_basis_points: if enabled { 1_000 } else { 0 },
            maximum_targets_per_utc_day: if enabled { 100 } else { 0 },
            updated_by: self.reviewer_ref.clone(),
            updated_at_unix_ms: unix_ms_now(),
        };
        self.busy = true;
        self.error = None;
        let service = self.service.clone();
        let task = cx.background_spawn(async move {
            service.set_assessment_sampling_policy(&policy, ReviewAuthorityV1::Human)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.notice = Some(if enabled {
                            "Automatic 10% sampling is on, capped at 100 traces per day.".into()
                        } else {
                            "Automatic sampling is off. You can still run this check manually."
                                .into()
                        });
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_builder(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let hosted_ready = self.execution_route == TaskCompletionExecutionRouteV1::LocalOnnx
            || (!self.model.read(cx).text().trim().is_empty()
                && !self.pricing_version.read(cx).text().trim().is_empty()
                && self
                    .input_rate
                    .read(cx)
                    .text()
                    .trim()
                    .parse::<u64>()
                    .is_ok()
                && self
                    .output_rate
                    .read(cx)
                    .text()
                    .trim()
                    .parse::<u64>()
                    .is_ok());
        let ready = self.project_id.is_some()
            && !self.busy
            && !self.name.read(cx).text().trim().is_empty()
            && (self.execution_route == TaskCompletionExecutionRouteV1::LocalOnnx
                || !self.criteria.read(cx).text().trim().is_empty())
            && hosted_ready;
        let local_selected = self.execution_route == TaskCompletionExecutionRouteV1::LocalOnnx;
        let hosted_selected = self.execution_route == TaskCompletionExecutionRouteV1::HostedOpenAi;
        let local_model = self.local_model.clone();
        div()
            .p_4()
            .rounded(px(8.))
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("New quality check"),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("Uses the approved agent goals and success criteria."),
            )
            .child(form_row("Name", self.name.clone(), compact))
            .child(
                div()
                    .mt_3()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("Execution"),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .gap_2()
                    .child(
                        button_state("On this Mac", local_selected, local_model.is_some())
                            .id("task-completion-route-local")
                            .role(Role::Button)
                            .aria_label("Use the verified local task-completion model")
                            .when(local_model.is_some(), |button| {
                                button.on_click(cx.listener(|this, _, _, cx| {
                                    this.select_execution_route(
                                        TaskCompletionExecutionRouteV1::LocalOnnx,
                                        cx,
                                    )
                                }))
                            }),
                    )
                    .child(
                        button_state("OpenAI", hosted_selected, true)
                            .id("task-completion-route-openai")
                            .role(Role::Button)
                            .aria_label("Use a hosted OpenAI task-completion judge")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.select_execution_route(
                                    TaskCompletionExecutionRouteV1::HostedOpenAi,
                                    cx,
                                )
                            })),
                    ),
            )
            .when(local_selected, |builder| {
                builder.when_some(local_model, |builder, _model| {
                    builder
                        .child(
                            div()
                                .mt_3()
                                .p_3()
                                .rounded_sm()
                                .bg(Theme::PANEL_SURFACE)
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child("Ready"),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_xs()
                                .text_color(Theme::AMBER)
                                .child("Development model · $0 provider cost"),
                        )
                })
            })
            .when(hosted_selected, |builder| {
                builder
                    .child(form_row("Review criteria", self.criteria.clone(), compact))
                    .child(form_row("Provider model", self.model.clone(), compact))
                    .child(form_row(
                        "Rate-card version",
                        self.pricing_version.clone(),
                        compact,
                    ))
                    .child(form_row(
                        "Input micros / 1M tokens",
                        self.input_rate.clone(),
                        compact,
                    ))
                    .child(form_row(
                        "Output micros / 1M tokens",
                        self.output_rate.clone(),
                        compact,
                    ))
            })
            .child(
                div()
                    .mt_3()
                    .text_xs()
                    .text_color(Theme::DIM)
                    .child(if local_selected {
                        "Trace evidence stays on this Mac. Traces with missing context are skipped."
                    } else {
                        "Sends redacted summaries to OpenAI. Traces with missing context are skipped."
                    }),
            )
            .child(
                div().mt_4().child(
                    button_state("Create quality check", true, ready)
                        .id("publish-task-completion-check")
                        .role(Role::Button)
                        .aria_label(if ready {
                            "Publish immutable task-completion quality check"
                        } else if local_selected && self.local_model.is_none() {
                            "Publishing unavailable until a verified local model is configured in Settings"
                        } else {
                            "Publishing unavailable until project, specification, model, and exact rate card are complete"
                        })
                        .when(ready, |button| {
                            button.on_click(cx.listener(|this, _, _, cx| this.publish(cx)))
                        }),
                ),
            )
    }

    fn render_checks(&self, cx: &mut Context<Self>) -> Div {
        let mut list = div().mt_3().flex().flex_col().gap_2();
        if self.quality_checks.is_empty() {
            list = list.child(
                div()
                    .p_3()
                    .rounded_sm()
                    .bg(Theme::PANEL_SURFACE)
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("No quality checks yet."),
            );
        }
        for (index, check) in self.quality_checks.iter().enumerate() {
            let selected = self.selected_release_id.as_deref()
                == Some(check.config.evaluator_release_id.as_str());
            let sampling = check
                .sampling_policy
                .as_ref()
                .is_some_and(|policy| policy.enabled);
            let model_label = match &check.evaluator.implementation {
                EvaluationImplementationV1::PromptJudge {
                    requested_model, ..
                } => format!("OpenAI {requested_model}"),
                EvaluationImplementationV1::LocalClassifier {
                    model_artifact_id, ..
                } => self
                    .local_model
                    .as_ref()
                    .filter(|model| model.model_artifact_id.as_str() == model_artifact_id.as_str())
                    .map_or_else(
                        || format!("Local {}", short_id(model_artifact_id)),
                        |model| format!("Local {}", model.model_id),
                    ),
                _ => "Unsupported runtime".into(),
            };
            let route_label = match &check.evaluator.implementation {
                EvaluationImplementationV1::PromptJudge { .. } => "OpenAI",
                EvaluationImplementationV1::LocalClassifier { .. } => "On this Mac",
                _ => "Unavailable",
            };
            list = list.child(
                div()
                    .id(("quality-check-release", index))
                    .role(Role::Button)
                    .aria_label(format!(
                        "Quality check {}, release {}, model {}, sampling {}",
                        check.evaluator.name,
                        short_id(&check.config.evaluator_release_id),
                        model_label,
                        if sampling { "enabled" } else { "disabled" }
                    ))
                    .tab_index(0)
                    .cursor_pointer()
                    .p_3()
                    .rounded_sm()
                    .border_1()
                    .border_color(if selected { Theme::CYAN } else { Theme::BORDER })
                    .bg(if selected {
                        Theme::ROW_SELECTED
                    } else {
                        Theme::PANEL_SURFACE
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_check(index, cx)))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(check.evaluator.name.clone()),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(format!(
                                "{} · {}",
                                route_label,
                                if sampling {
                                    "automatic 10% sampling"
                                } else {
                                    "manual runs"
                                }
                            )),
                    ),
            );
        }
        div()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::MUTED)
                    .child("PUBLISHED RELEASES"),
            )
            .child(list)
    }

    fn render_selected(&self, cx: &mut Context<Self>) -> Div {
        let Some(check) = self.selected_check() else {
            return div()
                .p_5()
                .rounded(px(8.))
                .border_1()
                .border_color(Theme::BORDER)
                .bg(Theme::PANEL)
                .text_sm()
                .text_color(Theme::MUTED)
                .child("Create or select a quality check to preview traces.");
        };
        let rubric = match &check.evaluator.implementation {
            EvaluationImplementationV1::PromptJudge { rubric, .. } => rubric.as_str(),
            EvaluationImplementationV1::LocalClassifier { .. } => {
                "Checks completion against the approved success criteria."
            }
            _ => "This release does not use a prompt rubric.",
        };
        let execution = match &check.evaluator.implementation {
            EvaluationImplementationV1::PromptJudge {
                requested_model, ..
            } => format!(
                "OpenAI · requested {} · {} · output estimate {}–{} tokens",
                requested_model,
                check.config.pricing_version,
                check.config.estimated_output_tokens_low,
                check.config.estimated_output_tokens_high
            ),
            EvaluationImplementationV1::LocalClassifier { .. } => {
                "On this Mac · $0 provider cost".into()
            }
            _ => "Unsupported evaluator implementation".into(),
        };
        let sampling_enabled = check
            .sampling_policy
            .as_ref()
            .is_some_and(|policy| policy.enabled);
        let preview_enabled = !self.busy;
        let run_enabled = !self.busy && self.preview.is_some();
        let mut panel =
            div()
                .p_5()
                .rounded(px(8.))
                .border_1()
                .border_color(Theme::BORDER)
                .bg(Theme::PANEL)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(check.evaluator.name.clone()),
                                )
                                .child(div().mt_1().text_xs().text_color(Theme::MUTED).child(
                                    format!(
                                        "Approved version {}",
                                        short_id(&check.config.evaluator_release_id)
                                    ),
                                )),
                        )
                        .child(tag("TASK COMPLETION", Theme::CYAN)),
                )
                .child(detail_row("Decision", rubric))
                .child(detail_row("Runs", &execution))
                .child(detail_row(
                    "Data",
                    if matches!(
                        &check.evaluator.implementation,
                        EvaluationImplementationV1::LocalClassifier { .. }
                    ) {
                        "Stays on this Mac"
                    } else {
                        "Redacted summaries are sent to OpenAI"
                    },
                ))
                .child(detail_row(
                    "Scope",
                    &format!(
                        "{} task type(s)",
                        check.evaluator.applicable_taxonomy_node_ids.len()
                    ),
                ))
                .child(
                    div()
                        .mt_4()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            button_state("Preview traces", true, preview_enabled)
                                .id("preview-task-completion-backfill")
                                .role(Role::Button)
                                .when(preview_enabled, |button| {
                                    button.on_click(cx.listener(|this, _, _, cx| this.preview(cx)))
                                }),
                        )
                        .child(
                            button_state(
                                if sampling_enabled {
                                    "Disable 10% sampling"
                                } else {
                                    "Enable 10% sampling"
                                },
                                false,
                                !self.busy,
                            )
                            .id("toggle-task-completion-sampling")
                            .role(Role::Button)
                            .when(!self.busy, |button| {
                                button.on_click(
                                    cx.listener(|this, _, _, cx| this.toggle_sampling(cx)),
                                )
                            }),
                        ),
                );
        if let Some(preview) = &self.preview {
            panel = panel.child(
                div()
                    .mt_4()
                    .p_4()
                    .rounded_sm()
                    .border_1()
                    .border_color(Theme::CYAN)
                    .bg(Theme::ROW_SELECTED)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Ready to run"),
                    )
                    .child(detail_row(
                        "Traces",
                        &format!(
                            "{} selected · {} ready · {} skipped",
                            preview.target_count,
                            preview.executable_count,
                            preview.non_executable_count
                        ),
                    ))
                    .child(detail_row(
                        "Estimated input",
                        &format!(
                            "{}–{} tokens",
                            preview.estimated_input_tokens_low, preview.estimated_input_tokens_high
                        ),
                    ))
                    .child(detail_row(
                        "Estimated cost",
                        &format!(
                            "{}–{}",
                            format_cost(preview.estimated_cost_micros_low),
                            format_cost(preview.estimated_cost_micros_high)
                        ),
                    ))
                    .child(
                        div().mt_3().child(
                            button_state("Run quality check", true, run_enabled)
                                .id("run-task-completion-preview")
                                .role(Role::Button)
                                .aria_label(
                                    "Run the exact preview after rechecking its selection hash",
                                )
                                .when(run_enabled, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| this.run_preview(cx)),
                                    )
                                }),
                        ),
                    ),
            );
        }
        let release_id = &check.config.evaluator_release_id;
        let related_jobs = self
            .jobs
            .iter()
            .filter(|job| job.evaluator_release_id == *release_id)
            .take(8)
            .collect::<Vec<_>>();
        panel.child(
            div()
                .mt_5()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(Theme::MUTED)
                        .child("RUNS"),
                )
                .when(related_jobs.is_empty(), |list| {
                    list.child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(Theme::DIM)
                            .child("No runs yet."),
                    )
                })
                .children(related_jobs.into_iter().map(|job| {
                    div()
                        .mt_2()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::PANEL_SURFACE)
                        .child(div().text_sm().child(format!(
                            "{:?} · {}/{} terminal",
                            job.status, job.terminal_count, job.item_count
                        )))
                        .child(div().mt_1().text_xs().text_color(Theme::DIM).child(format!(
                            "{} · selection {}",
                            short_id(&job.job_id),
                            short_id(&job.selection_hash)
                        )))
                })),
        )
    }
}

impl Render for QualityCheckStudio {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact =
            crate::design::Breakpoint::for_window(window) == crate::design::Breakpoint::Compact;
        let left = div()
            .when(compact, |column| column.w_full())
            .when(!compact, |column| column.w(px(380.)).flex_none())
            .child(self.render_checks(cx))
            .child(div().mt_5().child(self.render_builder(compact, cx)));
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
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .child("Quality checks"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(Theme::MUTED)
                            .child("Choose how to review a task, preview the traces, then run it."),
                    ),
            )
            .when_some(self.notice.clone(), |view, notice| {
                view.child(
                    div()
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
            .child(
                div()
                    .id("quality-check-studio-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_6()
                    .flex()
                    .when(compact, |columns| columns.flex_col())
                    .when(!compact, |columns| columns.items_start())
                    .gap_5()
                    .child(left)
                    .child(div().flex_1().min_w_0().child(self.render_selected(cx))),
            )
    }
}

fn input(
    value: &str,
    placeholder: &'static str,
    maximum_bytes: usize,
    cx: &mut Context<QualityCheckStudio>,
) -> Entity<TextInput> {
    let value = value.to_string();
    cx.new(|cx| {
        let mut input = TextInput::new(placeholder, maximum_bytes, cx);
        input.set_text(value, cx);
        input
    })
}

fn form_row(label: &str, input: Entity<TextInput>, compact: bool) -> Div {
    div()
        .mt_3()
        .flex()
        .when(compact, |row| row.flex_col().items_start())
        .when(!compact, |row| row.items_center())
        .gap_2()
        .child(
            div()
                .when(!compact, |label| label.w(px(150.)).flex_none())
                .text_xs()
                .text_color(Theme::MUTED)
                .child(label.to_string()),
        )
        .child(div().flex_1().min_w_0().child(input))
}

fn detail_row(label: &str, value: &str) -> Div {
    div()
        .mt_3()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::MUTED)
                .child(label.to_uppercase()),
        )
        .child(div().mt_1().text_sm().child(value.to_string()))
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(16)).unwrap_or(value)
}

fn format_cost(micros: u64) -> String {
    format!("${:.6}", micros as f64 / 1_000_000.0)
}

fn unix_ms_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

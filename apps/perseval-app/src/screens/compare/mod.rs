use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AppContext, Context, Div, EventEmitter, FontWeight, IntoElement, Render, Role, Window, div,
    prelude::*, px, uniform_list,
};
use perseval_service::{
    AlignmentRelation, ComparisonCancellationToken, LiveTraceService, RunComparisonRequestV1,
    TraceComparison,
};

use crate::components::{button, editor_empty_state, tag};
use crate::design::{Breakpoint, Theme};

#[derive(Debug, Clone)]
pub(crate) enum CompareEvent {
    ComparisonReady {
        project_id: String,
        comparison_id: String,
    },
    OpenRuns,
}

enum CompareState {
    Setup,
    Loading(RunComparisonRequestV1),
    Result(TraceComparison),
    Error {
        request: Option<RunComparisonRequestV1>,
        message: String,
    },
}

pub(crate) struct CompareScreen {
    service: Arc<LiveTraceService>,
    state: CompareState,
    request_generation: u64,
    selected_row: Option<usize>,
    current_cancellation: Option<ComparisonCancellationToken>,
}

impl EventEmitter<CompareEvent> for CompareScreen {}

impl CompareScreen {
    pub(crate) fn new(service: Arc<LiveTraceService>) -> Self {
        Self {
            service,
            state: CompareState::Setup,
            request_generation: 0,
            selected_row: None,
            current_cancellation: None,
        }
    }

    pub(crate) fn show_setup(&mut self, cx: &mut Context<Self>) {
        self.cancel_current_comparison();
        self.request_generation = self.request_generation.wrapping_add(1);
        self.state = CompareState::Setup;
        self.selected_row = None;
        cx.notify();
    }

    pub(crate) fn compare(&mut self, request: RunComparisonRequestV1, cx: &mut Context<Self>) {
        self.cancel_current_comparison();
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.state = CompareState::Loading(request.clone());
        self.selected_row = None;
        let service = self.service.clone();
        let cancellation = ComparisonCancellationToken::default();
        self.current_cancellation = Some(cancellation.clone());
        let task = cx.background_spawn({
            let request = request.clone();
            async move { service.compare_runs_cancellable(&request, &cancellation) }
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.current_cancellation = None;
                match result {
                    Ok(comparison) => {
                        let project_id = comparison.project_id.clone();
                        let comparison_id = comparison.comparison_id.clone();
                        this.state = CompareState::Result(comparison);
                        // Invalidate the loading frame before the shell records
                        // the stable comparison editor. Emitting first can let
                        // the parent navigation repaint while this child still
                        // has the loading display list cached.
                        cx.notify();
                        cx.emit(CompareEvent::ComparisonReady {
                            project_id,
                            comparison_id,
                        });
                    }
                    Err(error) => {
                        this.state = CompareState::Error {
                            request: Some(request),
                            message: error.to_string(),
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn show_comparison(&mut self, comparison_id: &str, cx: &mut Context<Self>) {
        if matches!(&self.state, CompareState::Result(result) if result.comparison_id == comparison_id)
        {
            return;
        }
        self.cancel_current_comparison();
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.state = CompareState::Loading(RunComparisonRequestV1 {
            schema_version: String::new(),
            scope: Default::default(),
            baseline_trace_id: String::new(),
            baseline_revision: 0,
            candidate_trace_id: String::new(),
            candidate_revision: 0,
        });
        let service = self.service.clone();
        let id = comparison_id.to_owned();
        let task = cx.background_spawn(async move { service.get_trace_comparison(&id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                match result {
                    Ok(Some(comparison)) => this.state = CompareState::Result(comparison),
                    Ok(None) => {
                        this.state = CompareState::Error {
                            request: None,
                            message: "This comparison is no longer available in the workspace. Select the two runs again."
                                .into(),
                        };
                    }
                    Err(error) => {
                        this.state = CompareState::Error {
                            request: None,
                            message: error.to_string(),
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_current_comparison();
        self.request_generation = self.request_generation.wrapping_add(1);
        self.state = CompareState::Setup;
        cx.notify();
    }

    fn cancel_current_comparison(&mut self) {
        if let Some(cancellation) = self.current_cancellation.take() {
            cancellation.cancel();
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let request = match &self.state {
            CompareState::Error {
                request: Some(request),
                ..
            } => Some(request.clone()),
            _ => None,
        };
        if let Some(request) = request {
            self.compare(request, cx);
        }
    }

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_row = Some(index);
        cx.notify();
    }

    fn render_result(
        &self,
        comparison: &TraceComparison,
        compact: bool,
        text_scale: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let divergence = comparison
            .first_meaningful_divergence
            .as_ref()
            .map(|divergence| {
                format!(
                    "First meaningful divergence after {} equivalent steps · {}",
                    comparison.common_prefix_steps, divergence.reason
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "No meaningful divergence found across {} aligned steps.",
                    comparison.common_prefix_steps
                )
            });
        let rows = comparison.rows.clone();
        let total = rows.len();
        let list = uniform_list(
            "comparison-rows",
            total,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| rows.get(index).map(|row| (index, row)))
                    .map(|(index, row)| {
                        let selected = this.selected_row == Some(index);
                        let tint = relation_tint(row.relation);
                        div()
                            .id(("comparison-row", index))
                            .role(Role::ListBoxOption)
                            .aria_label(format!(
                                "Aligned step {}; {:?}{}",
                                index + 1,
                                row.relation,
                                if selected { "; selected" } else { "" }
                            ))
                            .aria_selected(selected)
                            .tab_index(0)
                            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                            .w_full()
                            .h(px(if compact {
                                156. * text_scale
                            } else {
                                58. * text_scale
                            }))
                            .flex()
                            .when(compact, |row| {
                                row.flex_col().items_stretch().justify_center().py_3()
                            })
                            .when(!compact, |row| row.items_center())
                            .gap_3()
                            .px_4()
                            .border_b_1()
                            .border_color(Theme::BORDER)
                            .bg(if selected { Theme::SELECTED } else { Theme::BG })
                            .cursor_pointer()
                            .hover(|style| style.bg(Theme::PANEL_ALT))
                            .on_click(cx.listener(move |this, _, _, cx| this.select_row(index, cx)))
                            .child(
                                div()
                                    .min_w_0()
                                    .when(compact, |cell| cell.w_full())
                                    .when(!compact, |cell| cell.flex_1())
                                    .child(step_cell(
                                        compact.then_some("Baseline"),
                                        row.baseline.as_ref(),
                                        selected,
                                    )),
                            )
                            .child(
                                div()
                                    .when(compact, |cell| cell.w_full().items_start())
                                    .when(!compact, |cell| {
                                        cell.w(px(140.)).flex_none().items_center()
                                    })
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(tag(relation_label(row.relation), tint))
                                    .when_some(row.reason.clone(), |cell, reason| {
                                        cell.child(
                                            div()
                                                .text_xs()
                                                .text_color(if selected {
                                                    Theme::MUTED
                                                } else {
                                                    Theme::DIM
                                                })
                                                .child(reason),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .when(compact, |cell| cell.w_full())
                                    .when(!compact, |cell| cell.flex_1())
                                    .child(step_cell(
                                        compact.then_some("Candidate"),
                                        row.candidate.as_ref(),
                                        selected,
                                    )),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .w_full()
        .flex_1()
        .min_h_0();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .py_4()
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
                                            .child("Run comparison"),
                                    )
                                    .child(div().mt_1().text_sm().text_color(Theme::MUTED).child(
                                        format!(
                                            "{} r{} → {} r{}",
                                            short_id(&comparison.baseline_trace_id),
                                            comparison.baseline_revision,
                                            short_id(&comparison.candidate_trace_id),
                                            comparison.candidate_revision
                                        ),
                                    )),
                            )
                            .child(tag("PROJECT-SCOPED", Theme::CYAN)),
                    )
                    .child(
                        div()
                            .mt_4()
                            .p_3()
                            .rounded_sm()
                            .border_1()
                            .border_color(if comparison.first_meaningful_divergence.is_some() {
                                Theme::AMBER
                            } else {
                                Theme::GREEN
                            })
                            .bg(if comparison.first_meaningful_divergence.is_some() {
                                Theme::WARNING_SURFACE
                            } else {
                                Theme::SUCCESS_SURFACE
                            })
                            .text_sm()
                            .child(divergence),
                    ),
            )
            .when(!compact, |view| {
                view.child(
                    div()
                        .h(px(36.))
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .border_b_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::PANEL_ALT)
                        .text_xs()
                        .text_color(Theme::MUTED)
                        .child(div().min_w_0().flex_1().child("Baseline"))
                        .child(
                            div()
                                .w(px(140.))
                                .flex_none()
                                .text_center()
                                .child("Alignment"),
                        )
                        .child(div().min_w_0().flex_1().child("Candidate")),
                )
            })
            .child(div().flex_1().min_h_0().flex().flex_col().child(list))
            .when(comparison.truncated, |view| {
                view.child(
                    div()
                        .min_h(px(32.))
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .bg(Theme::PANEL)
                        .text_xs()
                        .text_color(Theme::AMBER)
                        .child("The comparison view is bounded around the first divergence."),
                )
            })
    }
}

impl Render for CompareScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = Breakpoint::for_window(window) == Breakpoint::Compact;
        let rem_size: f32 = window.rem_size().into();
        let text_scale = (rem_size / 16.).clamp(1., 2.);
        match &self.state {
            CompareState::Setup => div()
                .size_full()
                .child(editor_empty_state(
                    "Compare runs",
                    "Select exactly two finalized runs inside one project. Perseval aligns execution shape and keeps the first meaningful divergence visible.",
                    Some(
                        button("Open Runs", true)
                            .id("compare-open-runs")
                            .role(Role::Button)
                            .aria_label("Open Runs to select a comparison")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(CompareEvent::OpenRuns)
                            }))
                            .into_any_element(),
                    ),
                )),
            CompareState::Loading(request) => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Aligning finalized executions…"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(Theme::MUTED)
                        .child(format!(
                            "{} → {}",
                            short_id(&request.baseline_trace_id),
                            short_id(&request.candidate_trace_id)
                        )),
                )
                .child(
                    button("Cancel", false)
                        .id("cancel-comparison")
                        .role(Role::Button)
                        .aria_label("Cancel run comparison")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                ),
            CompareState::Result(comparison) => {
                self.render_result(comparison, compact, text_scale, cx)
            }
            CompareState::Error { request, message } => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                    "These runs cannot be compared",
                ))
                .child(
                    div()
                        .max_w(px(620.))
                        .text_center()
                        .text_sm()
                        .text_color(Theme::MUTED)
                        .child(message.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button("Back to Runs", false)
                                .id("compare-error-runs")
                                .role(Role::Button)
                                .aria_label("Back to Runs")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(CompareEvent::OpenRuns)
                                })),
                        )
                        .when(request.is_some(), |actions| {
                            actions.child(
                                button("Retry", true)
                                    .id("retry-comparison")
                                    .role(Role::Button)
                                    .aria_label("Retry run comparison")
                                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                            )
                        }),
                ),
        }
    }
}

fn step_cell(
    label: Option<&str>,
    step: Option<&perseval_service::ExecutionStep>,
    selected: bool,
) -> Div {
    let metadata_tint = if selected { Theme::MUTED } else { Theme::DIM };
    let Some(step) = step else {
        return div()
            .when_some(label, |cell, label| {
                cell.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(Theme::MUTED)
                        .child(label.to_uppercase()),
                )
            })
            .text_sm()
            .text_color(metadata_tint)
            .child("—");
    };
    div()
        .min_w_0()
        .when_some(label, |cell, label| {
            cell.child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(Theme::MUTED)
                    .child(label.to_uppercase()),
            )
        })
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(step.name.clone()),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(metadata_tint)
                .child(format!(
                    "{:?} · status {} · {:.1} ms{}",
                    step.kind,
                    step.status_code,
                    step.duration_nano as f64 / 1_000_000.,
                    step.agent_ref
                        .as_deref()
                        .map(|agent| format!(" · agent {agent}"))
                        .unwrap_or_default()
                )),
        )
}

fn relation_label(relation: AlignmentRelation) -> &'static str {
    match relation {
        AlignmentRelation::Exact => "EXACT",
        AlignmentRelation::Equivalent => "EQUIVALENT",
        AlignmentRelation::Changed => "CHANGED",
        AlignmentRelation::BaselineOnly => "REMOVED",
        AlignmentRelation::CandidateOnly => "ADDED",
    }
}

fn relation_tint(relation: AlignmentRelation) -> gpui::Rgba {
    match relation {
        AlignmentRelation::Exact | AlignmentRelation::Equivalent => Theme::GREEN,
        AlignmentRelation::Changed => Theme::AMBER,
        AlignmentRelation::BaselineOnly => Theme::RED,
        AlignmentRelation::CandidateOnly => Theme::CYAN,
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(14)).unwrap_or(value)
}

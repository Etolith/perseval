use gpui::{
    ClipboardItem, Context, Div, EventEmitter, FontWeight, IntoElement, Render, Window, div,
    prelude::*, px,
};
use perseval_service::SourceHealth;

use crate::design::{Breakpoint, ControlSize, Theme};

#[derive(Debug, Clone, Copy)]
pub(crate) enum WelcomeEvent {
    Sources,
    Runs,
    FailureInbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnboardingState {
    NoProject,
    ProjectWithoutSource,
    ReceiverDisabled,
    Listening,
    Projecting,
    Analyzing,
    NoFailures,
    Ready,
    IngestionError,
}

impl OnboardingState {
    fn copy(self) -> (&'static str, &'static str) {
        match self {
            Self::NoProject => (
                "Create your first project",
                "Projects keep unrelated agents, sessions, and evals isolated.",
            ),
            Self::ProjectWithoutSource => (
                "Choose how traces will arrive",
                "Send OTLP, import a trace file, or load the local demo.",
            ),
            Self::ReceiverDisabled => (
                "Connect a trace source",
                "Turn on the local receiver in Settings, or import a trace file.",
            ),
            Self::Listening => (
                "Waiting for the first trace",
                "Send an OTLP trace or import a file.",
            ),
            Self::Projecting => (
                "First trace received",
                "Perseval is processing the first trace.",
            ),
            Self::Analyzing => (
                "Looking for behavior failures",
                "You can inspect the trace while Perseval checks it for behavior failures.",
            ),
            Self::NoFailures => (
                "Runs analyzed — no failure groups found",
                "Inspect the run, send another agent version, or improve its telemetry.",
            ),
            Self::Ready => (
                "Failure groups are ready",
                "Open Failure Inbox to inspect evidence and create evals.",
            ),
            Self::IngestionError => (
                "Trace source needs attention",
                "Open Sources for the exact error and a contextual repair path.",
            ),
        }
    }
}

fn onboarding_state(
    project_count: usize,
    run_count: u64,
    has_failures: bool,
    health: &SourceHealth,
) -> OnboardingState {
    if project_count == 0 {
        OnboardingState::NoProject
    } else if health.last_error.is_some() {
        OnboardingState::IngestionError
    } else if health.journal_lag > 0 || health.projection_lag > 0 || health.queue_batches > 0 {
        OnboardingState::Projecting
    } else if health.analysis_pending > 0 || health.analysis_running > 0 {
        OnboardingState::Analyzing
    } else if run_count > 0 && has_failures {
        OnboardingState::Ready
    } else if run_count > 0 {
        OnboardingState::NoFailures
    } else if health.enabled {
        OnboardingState::Listening
    } else if health.accepted_spans > 0 || health.rejected_spans > 0 {
        OnboardingState::ReceiverDisabled
    } else {
        OnboardingState::ProjectWithoutSource
    }
}

pub(crate) struct WelcomeScreen {
    project_count: usize,
    run_count: u64,
    has_failures: bool,
    selected_project_id: Option<String>,
    endpoint: String,
    health: SourceHealth,
}

impl EventEmitter<WelcomeEvent> for WelcomeScreen {}

impl WelcomeScreen {
    pub(crate) fn new(
        project_count: usize,
        run_count: u64,
        has_failures: bool,
        selected_project_id: Option<String>,
        endpoint: String,
        health: SourceHealth,
    ) -> Self {
        Self {
            project_count,
            run_count,
            has_failures,
            selected_project_id,
            endpoint,
            health,
        }
    }

    pub(crate) fn update_context(
        &mut self,
        project_count: usize,
        run_count: u64,
        has_failures: bool,
        selected_project_id: Option<String>,
        health: SourceHealth,
        cx: &mut Context<Self>,
    ) {
        self.project_count = project_count;
        self.run_count = run_count;
        self.has_failures = has_failures;
        self.selected_project_id = selected_project_id;
        self.health = health;
        cx.notify();
    }

    fn copy_field(
        &self,
        value: String,
        action: &'static str,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let clipboard_value = value.clone();
        div()
            .mt_2()
            .h(px(ControlSize::DEFAULT))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .rounded_sm()
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::BG)
            .text_xs()
            .child(div().min_w_0().text_color(Theme::MUTED).child(value))
            .child(
                div()
                    .id(id)
                    .role(gpui::Role::Button)
                    .aria_label(format!("{action}: copy to clipboard"))
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .ml_3()
                    .text_color(Theme::CYAN)
                    .cursor_pointer()
                    .child(action)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(clipboard_value.clone()));
                    })),
            )
    }
}

impl Render for WelcomeScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = Breakpoint::for_window(window) == Breakpoint::Compact;
        let has_project = self.project_count > 0;
        // A durable imported/demo run is source activity even when the live OTLP
        // listener is intentionally disabled. Onboarding describes the user's
        // completed workflow, not just receiver configuration.
        let has_source_activity = self.run_count > 0
            || self.health.enabled
            || self.health.accepted_spans > 0
            || self.health.rejected_spans > 0;
        let completed_steps = usize::from(has_project)
            + usize::from(has_project && has_source_activity)
            + usize::from(self.run_count > 0);
        let setup_state = if self.run_count > 0 {
            format!("{} runs available for investigation", self.run_count)
        } else if self.health.enabled {
            "Listening for the first trace".into()
        } else if self.health.accepted_spans > 0 || self.health.rejected_spans > 0 {
            "Receiver disabled · file import available".into()
        } else {
            "No trace source activity yet".into()
        };
        let onboarding_state = onboarding_state(
            self.project_count,
            self.run_count,
            self.has_failures,
            &self.health,
        );
        let (state_title, state_detail) = onboarding_state.copy();
        let (primary_label, primary_event) = match onboarding_state {
            OnboardingState::NoProject => ("Create project", WelcomeEvent::Sources),
            OnboardingState::ProjectWithoutSource
            | OnboardingState::ReceiverDisabled
            | OnboardingState::Listening => ("Add traces", WelcomeEvent::Sources),
            OnboardingState::Projecting | OnboardingState::Analyzing if self.run_count > 0 => {
                ("View incoming runs", WelcomeEvent::Runs)
            }
            OnboardingState::Projecting | OnboardingState::Analyzing => {
                ("View source progress", WelcomeEvent::Sources)
            }
            OnboardingState::NoFailures => ("Inspect analyzed runs", WelcomeEvent::Runs),
            OnboardingState::Ready => ("Open Failure Inbox", WelcomeEvent::FailureInbox),
            OnboardingState::IngestionError => ("Repair source", WelcomeEvent::Sources),
        };
        let intro = div()
            .flex_1()
            .when(compact, |panel| panel.flex_none())
            .min_w_0()
            .flex()
            .flex_col()
            .justify_center()
            .gap_5()
            .p_8()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::CYAN)
                    .child("Local trace workbench"),
            )
            .child(
                div()
                    .max_w(px(620.))
                    .text_3xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Find recurring agent failures before they become regressions."),
            )
            .child(
                div()
                    .max_w(px(620.))
                    .text_base()
                    .text_color(Theme::MUTED)
                    .child("Create a project, send one trace, and Perseval will organize the run into evidence-backed failure groups and reviewed eval candidates."),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        primary_button(primary_label)
                            .id("welcome-primary-action")
                            .role(gpui::Role::Button)
                            .aria_label(primary_label)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(primary_event)
                            })),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .rounded_sm()
                    .border_1()
                    .border_color(if onboarding_state == OnboardingState::IngestionError {
                        Theme::RED
                    } else {
                        Theme::BORDER
                    })
                    .bg(Theme::PANEL)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(state_title),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(state_detail),
                    )
                    .when_some(self.health.last_error.clone(), |card, error| {
                        card.child(
                            div()
                                .mt_2()
                                .text_xs()
                                .text_color(Theme::RED)
                                .child(error),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(Theme::DIM)
                    .child("Everything stays on this Mac. No account or cloud required."),
            );
        let setup = div()
            .flex_1()
            .when(compact, |panel| panel.flex_none())
            .min_w_0()
            .flex()
            .flex_col()
            .justify_center()
            .gap_3()
            .p_8()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("First trace setup"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(format!("{completed_steps} / 3")),
                    ),
            )
            .child(setup_step(
                "1",
                "Create a project",
                "Keeps runs and agent versions isolated.",
                !has_project,
            ))
            .child(
                setup_step(
                    "2",
                    "Connect a trace source",
                    if self.health.enabled {
                        "OTLP/HTTP JSON or protobuf"
                    } else if self.run_count > 0 {
                        "Local demo or imported traces"
                    } else {
                        "OTLP, file import, or local demo"
                    },
                    has_project && !has_source_activity,
                )
                .when(has_project && self.health.enabled, |step| {
                    step.child(self.copy_field(
                        self.endpoint.clone(),
                        "Copy endpoint",
                        "copy-otlp-endpoint",
                        cx,
                    ))
                    .child(self.copy_field(
                        format!(
                            "OTEL_RESOURCE_ATTRIBUTES=perseval.project.id={}",
                            self.selected_project_id.as_deref().unwrap_or("<project-id>")
                        ),
                        "Copy",
                        "copy-project-attribute",
                        cx,
                    ))
                }),
            )
            .child(
                setup_step(
                    "3",
                    "Send the first trace",
                    "Receiving → analyzing",
                    has_project && has_source_activity && self.run_count == 0,
                )
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(Theme::MUTED)
                        .child(setup_state),
                ),
            );
        div()
            .id("welcome-scroll")
            .size_full()
            .min_h_0()
            .flex()
            .when(compact, |root| root.flex_col().overflow_y_scroll())
            .child(intro)
            .child(setup)
    }
}

fn primary_button(label: &str) -> Div {
    div()
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .min_h(px(ControlSize::PRIMARY))
        .py_2()
        .px_4()
        .flex()
        .items_center()
        .rounded(px(5.))
        .bg(Theme::CYAN)
        .text_color(Theme::TEXT_ON_ACCENT)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .child(label.to_string())
}

fn setup_step(number: &str, title: &str, detail: &str, active: bool) -> Div {
    div()
        .p_4()
        .rounded(px(6.))
        .border_1()
        .border_color(if active { Theme::CYAN } else { Theme::BORDER })
        .bg(Theme::PANEL)
        .child(
            div()
                .flex()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .size(px(24.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(if active { Theme::CYAN } else { Theme::BORDER })
                        .text_xs()
                        .child(number.to_string()),
                )
                .child(
                    div()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title.to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(detail.to_string()),
                        ),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_state_matrix_prioritizes_durable_progress_and_errors() {
        assert_eq!(
            onboarding_state(0, 0, false, &SourceHealth::default()),
            OnboardingState::NoProject
        );
        assert_eq!(
            onboarding_state(1, 0, false, &SourceHealth::default()),
            OnboardingState::ProjectWithoutSource
        );
        let listening = SourceHealth {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            onboarding_state(1, 0, false, &listening),
            OnboardingState::Listening
        );
        let projecting = SourceHealth {
            enabled: true,
            journal_lag: 1,
            ..Default::default()
        };
        assert_eq!(
            onboarding_state(1, 0, false, &projecting),
            OnboardingState::Projecting
        );
        let analyzing = SourceHealth {
            enabled: true,
            analysis_pending: 1,
            ..Default::default()
        };
        assert_eq!(
            onboarding_state(1, 1, false, &analyzing),
            OnboardingState::Analyzing
        );
        assert_eq!(
            onboarding_state(1, 1, false, &listening),
            OnboardingState::NoFailures
        );
        assert_eq!(
            onboarding_state(1, 1, true, &listening),
            OnboardingState::Ready
        );
        let disabled = SourceHealth {
            accepted_spans: 1,
            ..Default::default()
        };
        assert_eq!(
            onboarding_state(1, 0, false, &disabled),
            OnboardingState::ReceiverDisabled
        );
        let failed = SourceHealth {
            last_error: Some("disk full".into()),
            journal_lag: 1,
            ..Default::default()
        };
        assert_eq!(
            onboarding_state(1, 1, true, &failed),
            OnboardingState::IngestionError
        );
    }
}

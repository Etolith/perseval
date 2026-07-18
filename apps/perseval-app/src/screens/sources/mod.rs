use std::sync::Arc;

use gpui::{
    AppContext, ClipboardItem, Context, Div, Entity, EventEmitter, FontWeight, IntoElement,
    PathPromptOptions, Render, ScrollHandle, Window, div, prelude::*, px,
};
use perseval_service::{CreateProjectV1, LiveTraceService, ProjectV1, SourceHealth};

use crate::components::TextInput;
use crate::design::{Breakpoint, ControlSize, Theme};

#[derive(Debug, Clone)]
pub(crate) enum SourcesEvent {
    ProjectCreated(ProjectV1),
    TraceImported,
}

pub(crate) struct SourcesScreen {
    service: Arc<LiveTraceService>,
    projects: Vec<ProjectV1>,
    health: SourceHealth,
    endpoint: String,
    project_name: Entity<TextInput>,
    selected_project_id: Option<String>,
    project_creation_open: bool,
    creating: bool,
    importing: bool,
    sample_confirmation_pending: bool,
    loading_sample: bool,
    project_error: Option<String>,
    project_confirmation: Option<String>,
    import_error: Option<String>,
    import_confirmation: Option<String>,
    sample_error: Option<String>,
    sample_confirmation: Option<String>,
    content_scroll: ScrollHandle,
}

impl EventEmitter<SourcesEvent> for SourcesScreen {}

impl SourcesScreen {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        projects: Vec<ProjectV1>,
        health: SourceHealth,
        endpoint: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let project_name = cx.new(|cx| TextInput::new("e.g. Checkout agent", 120, cx));
        cx.observe(&project_name, |_, _, cx| cx.notify()).detach();
        let selected_project_id = projects.first().map(|project| project.project_id.clone());
        let project_creation_open = projects.is_empty();
        Self {
            service,
            projects,
            health,
            endpoint,
            project_name,
            selected_project_id,
            project_creation_open,
            creating: false,
            importing: false,
            sample_confirmation_pending: false,
            loading_sample: false,
            project_error: None,
            project_confirmation: None,
            import_error: None,
            import_confirmation: None,
            sample_error: None,
            sample_confirmation: None,
            content_scroll: ScrollHandle::new(),
        }
    }

    pub(crate) fn update_health(&mut self, health: SourceHealth, cx: &mut Context<Self>) {
        if let Some(address) = health.effective_address.as_deref() {
            self.endpoint = format!("http://{address}/v1/traces");
        }
        self.health = health;
        cx.notify();
    }

    pub(crate) fn select_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self
            .projects
            .iter()
            .any(|project| project.project_id == project_id)
        {
            self.selected_project_id = Some(project_id.to_string());
            self.import_error = None;
            self.sample_error = None;
            cx.notify();
        }
    }

    pub(crate) fn show_project_creation(&mut self, cx: &mut Context<Self>) {
        self.project_creation_open = true;
        self.project_error = None;
        cx.notify();
    }

    fn toggle_project_creation(&mut self, cx: &mut Context<Self>) {
        self.project_creation_open = !self.project_creation_open;
        self.project_error = None;
        cx.notify();
    }

    fn import_trace_file(&mut self, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let Some(project_id) = self.selected_project_id.clone() else {
            self.import_error =
                Some("Create and select a project before importing a trace file.".into());
            cx.notify();
            return;
        };
        self.importing = true;
        self.import_error = None;
        self.import_confirmation = Some("Choose an OTLP JSON or protobuf trace file…".into());
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import trace".into()),
        });
        let service = self.service.clone();
        cx.spawn(async move |weak, cx| {
            let selected = picker.await;
            let path = match selected {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.importing = false;
                        this.import_confirmation = None;
                        this.import_error =
                            Some(format!("Could not open the file picker: {error}"));
                        cx.notify();
                    });
                    return;
                }
                Err(_) => None,
            };
            let Some(path) = path else {
                let _ = weak.update(cx, |this, cx| {
                    this.importing = false;
                    this.import_confirmation = None;
                    cx.notify();
                });
                return;
            };
            let task =
                cx.background_spawn(async move { service.import_otlp_file(&project_id, &path) });
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.importing = false;
                match result {
                    Ok(result) => {
                        this.health = this.service.source_health().unwrap_or(this.health.clone());
                        this.import_error = None;
                        this.import_confirmation = Some(if result.duplicate_request {
                            format!(
                                "{} was already imported; nothing changed.",
                                result.file_name
                            )
                        } else {
                            format!(
                                "Imported {} spans from {} into {}{}.",
                                result.accepted_spans,
                                result.file_name,
                                result.project_id,
                                if result.rejected_spans > 0 {
                                    format!(
                                        "; {} malformed spans were rejected",
                                        result.rejected_spans
                                    )
                                } else {
                                    String::new()
                                }
                            )
                        });
                        cx.emit(SourcesEvent::TraceImported);
                    }
                    Err(error) => {
                        this.import_confirmation = None;
                        this.import_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_local_demo(&mut self, cx: &mut Context<Self>) {
        if self.loading_sample {
            return;
        }
        if self.selected_project_id.is_none() {
            self.sample_error =
                Some("Create and select a project before loading demo data.".into());
            cx.notify();
            return;
        }
        self.sample_confirmation_pending = true;
        self.sample_error = None;
        self.sample_confirmation = None;
        cx.notify();
    }

    fn cancel_local_demo(&mut self, cx: &mut Context<Self>) {
        self.sample_confirmation_pending = false;
        cx.notify();
    }

    fn load_local_demo(&mut self, cx: &mut Context<Self>) {
        if self.loading_sample {
            return;
        }
        let Some(project_id) = self.selected_project_id.clone() else {
            self.sample_error =
                Some("Create and select a project before loading demo data.".into());
            cx.notify();
            return;
        };
        self.sample_confirmation_pending = false;
        self.loading_sample = true;
        self.sample_error = None;
        self.sample_confirmation = Some(
            "Durably importing 3 demo runs and 33 spans; analysis will continue in the background…"
                .into(),
        );
        let service = self.service.clone();
        let task = cx.background_spawn(async move { service.load_local_demo(&project_id) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.loading_sample = false;
                match result {
                    Ok(result) => {
                        this.health = this.service.source_health().unwrap_or(this.health.clone());
                        this.sample_error = None;
                        this.sample_confirmation = Some(if result.duplicate_request {
                            "The local demo is already present in this project; no spans were duplicated."
                                .into()
                        } else {
                            format!(
                                "Loaded {} demo spans across 3 runs. Finalization and failure analysis continue in the background.",
                                result.accepted_spans
                            )
                        });
                        cx.emit(SourcesEvent::TraceImported);
                    }
                    Err(error) => {
                        this.sample_confirmation = None;
                        this.sample_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn create_project(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let display_name = self.project_name.read(cx).text().trim().to_string();
        let project_id = slugify(&display_name);
        if display_name.is_empty() {
            self.project_error = Some("Enter a project name first.".into());
            cx.notify();
            return;
        }
        if project_id.is_empty() {
            self.project_error = Some(
                "The project name needs at least one Latin letter or number for its stable ID."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.creating = true;
        self.project_error = None;
        self.project_confirmation = None;
        let service = self.service.clone();
        let request = CreateProjectV1 {
            project_id: project_id.clone(),
            display_name,
            artifact_namespace: project_id,
        };
        let task = cx.background_spawn(async move { service.create_project(request) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(project) => {
                        this.selected_project_id = Some(project.project_id.clone());
                        this.project_creation_open = false;
                        this.projects.push(project.clone());
                        this.projects.sort_by_key(|project| {
                            (
                                project.display_name.to_lowercase(),
                                project.project_id.clone(),
                            )
                        });
                        this.project_confirmation = Some(format!(
                            "Created {}. Copy its project attribute before sending traces.",
                            project.display_name
                        ));
                        this.project_name
                            .update(cx, |input, cx| input.set_text("", cx));
                        cx.emit(SourcesEvent::ProjectCreated(project));
                    }
                    Err(error) => {
                        this.project_error = Some(project_error_message(&error.to_string()))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_health(&self, cx: &mut Context<Self>) -> Div {
        let (label, tint, detail) = if self.health.backpressured {
            (
                "Backpressured",
                Theme::RED,
                "The bounded ingest queue is full. Producers receive an explicit retry response.",
            )
        } else if self.health.enabled {
            (
                "Listening",
                Theme::GREEN,
                "Ready to receive local OTLP traces.",
            )
        } else {
            (
                "Receiver disabled",
                Theme::AMBER,
                "Turn on the local receiver in Settings.",
            )
        };
        section("Current source")
            .child(
                div()
                    .mt_3()
                    .p_4()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::BG)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(7.)).rounded_full().bg(tint))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label),
                            ),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(detail),
                    )
                    .when_some(self.health.last_error.clone(), |card, error| {
                        card.child(message(error, Theme::RED))
                    }),
            )
            .when(self.health.enabled, |card| {
                card.child(copyable_field(
                    "OTLP/HTTP endpoint",
                    &self.endpoint,
                    "copy-source-endpoint",
                    cx,
                ))
                .when_some(
                    self.selected_project_id.as_deref(),
                    |card, project_id| {
                        card.child(copyable_field(
                            "Project resource attribute",
                            &format!("OTEL_RESOURCE_ATTRIBUTES=perseval.project.id={project_id}"),
                            "copy-source-project-attribute",
                            cx,
                        ))
                    },
                )
            })
    }

    fn render_import(&self, cx: &mut Context<Self>) -> Div {
        let selected_name = self
            .selected_project_id
            .as_deref()
            .and_then(|id| {
                self.projects
                    .iter()
                    .find(|project| project.project_id == id)
            })
            .map(|project| project.display_name.as_str());
        let enabled = selected_name.is_some() && !self.importing;
        section("Import a trace file")
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("Import OTLP JSON, protobuf, or gzip files into the selected project."),
            )
            .child(read_only_field(
                "Destination project",
                selected_name.unwrap_or("Select or create a project first"),
            ))
            .child(
                div()
                    .mt_3()
                    .p_3()
                    .rounded(px(5.))
                    .bg(Theme::WARNING_SURFACE)
                    .text_xs()
                    .text_color(Theme::AMBER)
                    .child("Perseval tags imported spans with the selected project. Oversized files are rejected."),
            )
            .when_some(self.import_error.clone(), |form, error| {
                form.child(message(error, Theme::RED))
            })
            .when_some(self.import_confirmation.clone(), |form, confirmation| {
                form.child(message(confirmation, Theme::GREEN))
            })
            .child(
                div().mt_4().flex().justify_end().child(
                    div()
                        .id("import-trace-file")
                        .role(gpui::Role::Button)
                        .aria_label(if self.importing {
                            "Importing trace file"
                        } else if enabled {
                            "Choose trace file to import"
                        } else {
                            "Trace import unavailable until a project is selected"
                        })
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                        .h(px(ControlSize::DEFAULT))
                        .px_4()
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(if enabled { Theme::CYAN } else { Theme::PANEL_ALT })
                        .text_color(if enabled { Theme::TEXT_ON_ACCENT } else { Theme::DIM })
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(enabled, |button| {
                            button.cursor_pointer().on_click(
                                cx.listener(|this, _, _, cx| this.import_trace_file(cx)),
                            )
                        })
                        .child(if self.importing {
                            "Importing…"
                        } else {
                            "Choose trace file…"
                        }),
                ),
            )
    }

    fn render_setup(&self, cx: &mut Context<Self>) -> Div {
        let name = self.project_name.read(cx).text().trim().to_string();
        let project_id = slugify(&name);
        section("Create project")
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("A project is one repository, application, or agent system. It is not a trace session."),
            )
            .child(field_label("Project name"))
            .child(self.project_name.clone())
            .child(read_only_field(
                "Stable project ID",
                if project_id.is_empty() {
                    "Generated from the project name"
                } else {
                    &project_id
                },
            ))
            .when_some(self.project_error.clone(), |form, error| {
                form.child(message(error, Theme::RED))
            })
            .when_some(self.project_confirmation.clone(), |form, confirmation| {
                form.child(message(confirmation, Theme::GREEN))
            })
            .child(
                div().mt_4().flex().justify_end().child(
                    div()
                        .id("create-project")
                        .role(gpui::Role::Button)
                        .aria_label(if self.creating {
                            "Creating project"
                        } else if name.is_empty() {
                            "Create project unavailable until a name is entered"
                        } else {
                            "Create project"
                        })
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                        .h(px(ControlSize::DEFAULT))
                        .px_4()
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(if name.is_empty() || self.creating {
                            Theme::PANEL_ALT
                        } else {
                            Theme::CYAN
                        })
                        .text_color(if name.is_empty() || self.creating {
                            Theme::DIM
                        } else {
                            Theme::TEXT_ON_ACCENT
                        })
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(!name.is_empty() && !self.creating, |button| {
                            button.cursor_pointer().on_click(cx.listener(
                                |this, _, _, cx| this.create_project(cx),
                            ))
                        })
                        .child(if self.creating {
                            "Creating…"
                        } else {
                            "Create project"
                        }),
                ),
            )
    }

    fn render_local_demo(&self, cx: &mut Context<Self>) -> Div {
        let enabled = self.selected_project_id.is_some() && !self.loading_sample;
        section("Local demo data")
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child("Load 3 offline agent runs with a repeated browser failure."),
            )
            .child(
                div()
                    .mt_3()
                    .p_3()
                    .rounded(px(5.))
                    .bg(Theme::INFO_SURFACE)
                    .text_xs()
                    .text_color(Theme::CYAN)
                    .child("Local only. No network or model calls. Loading twice does not create duplicates."),
            )
            .when(self.sample_confirmation_pending, |card| {
                card.child(
                    div()
                        .mt_3()
                        .p_3()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(Theme::AMBER)
                        .child(
                            div()
                                .text_xs()
                                .text_color(Theme::AMBER)
                                .child("Load the sample into the selected project? It will appear beside real runs and is tagged perseval.sample=true."),
                        )
                        .child(
                            div()
                                .mt_3()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("cancel-local-demo")
                                        .role(gpui::Role::Button)
                                        .aria_label("Cancel loading local demo data")
                                        .tab_index(0)
                                        .focus_visible(|style| {
                                            style.border_2().border_color(Theme::CYAN)
                                        })
                                        .px_3()
                                        .py_2()
                                        .text_xs()
                                        .cursor_pointer()
                                        .child("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| this.cancel_local_demo(cx))),
                                )
                                .child(
                                    div()
                                        .id("confirm-local-demo")
                                        .role(gpui::Role::Button)
                                        .aria_label("Load 3 local demo runs")
                                        .tab_index(0)
                                        .focus_visible(|style| {
                                            style.border_2().border_color(Theme::CYAN)
                                        })
                                        .px_3()
                                        .py_2()
                                        .rounded(px(5.))
                                        .bg(Theme::CYAN)
                                        .text_color(Theme::TEXT_ON_ACCENT)
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .cursor_pointer()
                                        .child("Load 3 demo runs")
                                        .on_click(cx.listener(|this, _, _, cx| this.load_local_demo(cx))),
                                ),
                        ),
                )
            })
            .when_some(self.sample_error.clone(), |card, error| {
                card.child(message(error, Theme::RED))
            })
            .when_some(self.sample_confirmation.clone(), |card, confirmation| {
                card.child(message(confirmation, Theme::GREEN))
            })
            .when(!self.sample_confirmation_pending, |card| {
                card.child(
                    div().mt_4().flex().justify_end().child(
                        div()
                            .id("load-local-demo")
                            .role(gpui::Role::Button)
                            .aria_label(if enabled {
                                "Load local demo data"
                            } else {
                                "Local demo unavailable until a project is selected"
                            })
                            .tab_index(0)
                            .focus_visible(|style| {
                                style.border_2().border_color(Theme::CYAN)
                            })
                            .h(px(ControlSize::DEFAULT))
                            .px_4()
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(Theme::BORDER)
                            .text_color(if enabled { Theme::TEXT } else { Theme::DIM })
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .when(enabled, |button| {
                                button.cursor_pointer().on_click(
                                    cx.listener(|this, _, _, cx| this.request_local_demo(cx)),
                                )
                            })
                            .child(if self.loading_sample {
                                "Loading demo…"
                            } else {
                                "Load local demo data"
                            }),
                    ),
                )
            })
    }
}

impl Render for SourcesScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = Breakpoint::for_window(window) == Breakpoint::Compact;
        let selected_project = self.selected_project_id.as_deref().and_then(|id| {
            self.projects
                .iter()
                .find(|project| project.project_id == id)
        });
        let page_title = selected_project
            .map(|project| format!("Connect traces to {}", project.display_name))
            .unwrap_or_else(|| "Create your first project".into());
        let page_description = if selected_project.is_some() {
            "Choose one way to add traces. Project creation is finished and stays out of the way."
        } else {
            "A project keeps one agent system, its builds, traces, findings, and evals in one scope."
        };
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .child(
                div()
                    .id("sources-main-scroll")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .child(
                        div()
                            .flex_none()
                            .min_w_0()
                            .when(compact, |content| content.w_full())
                            .when(!compact, |content| content.w(px(780.)))
                            .when(compact, |content| content.p_5())
                            .when(!compact, |content| content.p_8())
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .justify_between()
                                    .gap_4()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_xl()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(page_title),
                                            )
                                            .child(
                                                div()
                                                    .mt_2()
                                                    .text_sm()
                                                    .text_color(Theme::MUTED)
                                                    .child(page_description),
                                            ),
                                    )
                                    .when(!self.projects.is_empty(), |header| {
                                        header.child(
                                            div()
                                                .id("toggle-new-project")
                                                .role(gpui::Role::Button)
                                                .aria_label(if self.project_creation_open {
                                                    "Cancel creating a new project"
                                                } else {
                                                    "Create a new project"
                                                })
                                                .aria_expanded(self.project_creation_open)
                                                .tab_index(0)
                                                .focus_visible(|style| {
                                                    style.border_2().border_color(Theme::CYAN)
                                                })
                                                .h(px(ControlSize::DEFAULT))
                                                .px_4()
                                                .flex()
                                                .items_center()
                                                .rounded(px(5.))
                                                .border_1()
                                                .border_color(Theme::BORDER)
                                                .text_xs()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .cursor_pointer()
                                                .child(if self.project_creation_open {
                                                    "Cancel"
                                                } else {
                                                    "New project"
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_project_creation(cx)
                                                })),
                                        )
                                    }),
                            )
                            .when(
                                self.projects.is_empty() || self.project_creation_open,
                                |content| content.child(self.render_setup(cx)),
                            )
                            .when(selected_project.is_some(), |content| {
                                content
                                    .child(self.render_health(cx))
                                    .child(self.render_local_demo(cx))
                                    .child(self.render_import(cx))
                            }),
                    ),
            )
    }
}

fn section(title: &str) -> Div {
    div()
        .mt_6()
        .p_5()
        .rounded(px(7.))
        .border_1()
        .border_color(Theme::BORDER)
        .bg(Theme::PANEL)
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
}

fn field_label(label: &str) -> Div {
    div()
        .mt_4()
        .mb_2()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(label.to_string())
}

fn read_only_field(label: &str, value: &str) -> Div {
    div().mt_3().child(field_label(label)).child(
        div()
            .h(px(ControlSize::DEFAULT))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(5.))
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::BG)
            .text_xs()
            .text_color(Theme::MUTED)
            .child(value.to_string()),
    )
}

fn copyable_field(
    label: &str,
    value: &str,
    id: &'static str,
    cx: &mut Context<SourcesScreen>,
) -> Div {
    let clipboard_value = value.to_string();
    div().mt_3().child(field_label(label)).child(
        div()
            .h(px(ControlSize::DEFAULT))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(5.))
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::BG)
            .text_xs()
            .child(
                div()
                    .min_w_0()
                    .text_color(Theme::MUTED)
                    .child(value.to_string()),
            )
            .child(
                div()
                    .id(id)
                    .role(gpui::Role::Button)
                    .aria_label(format!("Copy {label} to clipboard"))
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .ml_3()
                    .text_color(Theme::CYAN)
                    .cursor_pointer()
                    .child("Copy")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(clipboard_value.clone()));
                    })),
            ),
    )
}

fn message(value: String, tint: gpui::Rgba) -> Div {
    div()
        .mt_3()
        .p_3()
        .rounded(px(5.))
        .border_1()
        .border_color(tint)
        .text_xs()
        .text_color(tint)
        .child(value)
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character);
            separator = false;
        } else {
            separator = true;
        }
        if slug.len() >= 80 {
            break;
        }
    }
    slug.trim_end_matches('-').to_string()
}

fn project_error_message(error: &str) -> String {
    if error.contains("UNIQUE constraint failed") {
        "A project with this generated ID or artifact namespace already exists. Use a more specific name."
            .into()
    } else {
        format!("Project creation failed: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn project_slug_is_stable_and_path_safe() {
        assert_eq!(slugify(" Checkout / Agent v2 "), "checkout-agent-v2");
        assert_eq!(slugify("é agent"), "agent");
        assert!(slugify("../").is_empty());
    }
}

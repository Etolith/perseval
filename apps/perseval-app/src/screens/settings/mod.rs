use std::path::{Path, PathBuf};

use gpui::{
    AccessibleAction, Context, Entity, EventEmitter, FontWeight, IntoElement, Render, Role,
    ScrollHandle, Toggled, Window, div, point, prelude::*, px,
};
use perseval_service::{OpenAiProviderHealthV1, PersevalConfigV1};

use crate::components::{TextInput, button};
use crate::design::{Breakpoint, Theme};
use crate::icons::{AppIcon, icon};
use crate::workbench::{AppearancePreferencesV1, TextScale};

mod components;

use components::{action_button, editable_row, notice, section, setting_row, switch_row};

#[derive(Debug, Clone)]
pub(crate) enum SettingsEvent {
    AppearanceChanged(AppearancePreferencesV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    WorkspaceStorage,
    PrivacyPayloads,
    AiFeatures,
    Appearance,
}

impl SettingsCategory {
    const ALL: [Self; 4] = [
        Self::WorkspaceStorage,
        Self::PrivacyPayloads,
        Self::AiFeatures,
        Self::Appearance,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::WorkspaceStorage => "Workspace & storage",
            Self::PrivacyPayloads => "Privacy & payloads",
            Self::AiFeatures => "AI features",
            Self::Appearance => "Appearance",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::WorkspaceStorage => {
                "Choose the durable workspace and configure local trace ingestion."
            }
            Self::PrivacyPayloads => {
                "Control bounded payload reveals and understand what remains on disk."
            }
            Self::AiFeatures => {
                "Tune deterministic grouping and explicitly opt into hosted analysis."
            }
            Self::Appearance => "Adjust reading scale and motion without restarting the workbench.",
        }
    }

    const fn icon(self) -> AppIcon {
        match self {
            Self::WorkspaceStorage => AppIcon::Database,
            Self::PrivacyPayloads => AppIcon::Shield,
            Self::AiFeatures => AppIcon::Sparkles,
            Self::Appearance => AppIcon::Accessibility,
        }
    }
}

pub(crate) struct SettingsScreen {
    runtime_config: PersevalConfigV1,
    saved_config: PersevalConfigV1,
    draft: PersevalConfigV1,
    workspace_id: Entity<TextInput>,
    workspace_dir: Entity<TextInput>,
    reviewer_ref: Entity<TextInput>,
    otlp_bind: Entity<TextInput>,
    inline_attribute_kib: Entity<TextInput>,
    default_preview_kib: Entity<TextInput>,
    maximum_reveal_mib: Entity<TextInput>,
    openai_embedding_model: Entity<TextInput>,
    openai_chat_model: Entity<TextInput>,
    openai_health: OpenAiProviderHealthV1,
    workspace_bytes: Option<u64>,
    size_error: Option<String>,
    saving: bool,
    save_notice: Option<(String, gpui::Rgba)>,
    appearance: AppearancePreferencesV1,
    selected_category: SettingsCategory,
    content_scroll: ScrollHandle,
}

impl SettingsScreen {
    pub(crate) fn new(
        config: PersevalConfigV1,
        appearance: AppearancePreferencesV1,
        openai_health: OpenAiProviderHealthV1,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_id = Self::input(&config.workspace_id, "Workspace ID", 120, cx);
        let workspace_dir = Self::input(
            &config.workspace_dir.display().to_string(),
            "Workspace directory",
            1_024,
            cx,
        );
        let reviewer_ref = Self::input(&config.reviewer_ref, "Reviewer reference", 120, cx);
        let otlp_bind = Self::input(&config.otlp.bind_addr.to_string(), "127.0.0.1:4318", 80, cx);
        let inline_attribute_kib = Self::input(
            &(config.blobs.inline_attribute_bytes / 1_024).to_string(),
            "4",
            12,
            cx,
        );
        let default_preview_kib = Self::input(
            &(config.query.blob_preview_bytes / 1_024).to_string(),
            "64",
            12,
            cx,
        );
        let maximum_reveal_mib = Self::input(
            &(config.blobs.maximum_local_reveal_bytes / (1_024 * 1_024)).to_string(),
            "16",
            12,
            cx,
        );
        let openai_embedding_model = Self::input(
            &config.analysis.openai.embedding_model,
            "text-embedding-3-small",
            120,
            cx,
        );
        let openai_chat_model =
            Self::input(&config.analysis.openai.chat_model, "gpt-5-mini", 120, cx);
        let workspace = config.workspace_dir.clone();
        let task = cx.background_spawn(async move { directory_size(&workspace) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(bytes) => this.workspace_bytes = Some(bytes),
                    Err(error) => this.size_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            runtime_config: config.clone(),
            saved_config: config.clone(),
            draft: config,
            workspace_id,
            workspace_dir,
            reviewer_ref,
            otlp_bind,
            inline_attribute_kib,
            default_preview_kib,
            maximum_reveal_mib,
            openai_embedding_model,
            openai_chat_model,
            openai_health,
            workspace_bytes: None,
            size_error: None,
            saving: false,
            save_notice: None,
            appearance,
            selected_category: SettingsCategory::WorkspaceStorage,
            content_scroll: ScrollHandle::new(),
        }
    }

    fn input(
        value: &str,
        placeholder: &'static str,
        maximum_bytes: usize,
        cx: &mut Context<Self>,
    ) -> Entity<TextInput> {
        let value = value.to_string();
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, maximum_bytes, cx);
            input.set_text(value, cx);
            input
        });
        cx.observe(&input, |this, _, cx| {
            this.save_notice = None;
            cx.notify();
        })
        .detach();
        input
    }

    fn candidate(&self, cx: &Context<Self>) -> Result<PersevalConfigV1, String> {
        let mut candidate = self.draft.clone();
        candidate.workspace_id = required_text(&self.workspace_id, "Workspace ID", cx)?;
        candidate.workspace_dir =
            required_text(&self.workspace_dir, "Workspace directory", cx)?.into();
        candidate.reviewer_ref = required_text(&self.reviewer_ref, "Reviewer reference", cx)?;
        candidate.otlp.bind_addr = required_text(&self.otlp_bind, "OTLP bind address", cx)?
            .parse()
            .map_err(|_| "OTLP bind address must look like 127.0.0.1:4318".to_string())?;
        candidate.blobs.inline_attribute_bytes = parse_size(
            &self.inline_attribute_kib,
            "Inline attribute limit",
            1_024,
            cx,
        )?;
        candidate.query.blob_preview_bytes = parse_size(
            &self.default_preview_kib,
            "Default payload preview",
            1_024,
            cx,
        )?;
        candidate.blobs.maximum_local_reveal_bytes = parse_size(
            &self.maximum_reveal_mib,
            "Maximum local reveal",
            1_024 * 1_024,
            cx,
        )?;
        candidate.analysis.openai.embedding_model =
            required_text(&self.openai_embedding_model, "OpenAI embedding model", cx)?;
        candidate.analysis.openai.chat_model =
            required_text(&self.openai_chat_model, "OpenAI chat model", cx)?;
        candidate.validate().map_err(|error| error.to_string())?;
        Ok(candidate)
    }

    fn toggle_otlp(&mut self, cx: &mut Context<Self>) {
        self.draft.otlp.enabled = !self.draft.otlp.enabled;
        self.save_notice = None;
        cx.notify();
    }

    fn toggle_feature_similarity(&mut self, cx: &mut Context<Self>) {
        self.draft.analysis.feature_similarity_enabled =
            !self.draft.analysis.feature_similarity_enabled;
        if !self.draft.analysis.feature_similarity_enabled {
            self.draft.analysis.openai.embeddings_enabled = false;
            self.draft.analysis.openai.cluster_labels_enabled = false;
        }
        self.save_notice = None;
        cx.notify();
    }

    fn toggle_openai(&mut self, cx: &mut Context<Self>) {
        self.draft.analysis.openai.enabled = !self.draft.analysis.openai.enabled;
        if !self.draft.analysis.openai.enabled {
            self.draft.analysis.openai.embeddings_enabled = false;
            self.draft.analysis.openai.cluster_labels_enabled = false;
            self.draft.analysis.openai.semantic_judge_enabled = false;
        }
        self.save_notice = None;
        cx.notify();
    }

    fn toggle_openai_embeddings(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.draft.analysis.openai.embeddings_enabled;
        self.draft.analysis.openai.embeddings_enabled = enabled;
        if enabled {
            self.draft.analysis.openai.enabled = true;
            self.draft.analysis.feature_similarity_enabled = true;
        } else {
            self.draft.analysis.openai.cluster_labels_enabled = false;
        }
        self.save_notice = None;
        cx.notify();
    }

    fn toggle_openai_cluster_labels(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.draft.analysis.openai.cluster_labels_enabled;
        self.draft.analysis.openai.cluster_labels_enabled = enabled;
        if enabled {
            self.draft.analysis.openai.enabled = true;
            self.draft.analysis.openai.embeddings_enabled = true;
            self.draft.analysis.feature_similarity_enabled = true;
        }
        self.save_notice = None;
        cx.notify();
    }

    fn toggle_openai_semantic_judge(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.draft.analysis.openai.semantic_judge_enabled;
        self.draft.analysis.openai.semantic_judge_enabled = enabled;
        if enabled {
            self.draft.analysis.openai.enabled = true;
        }
        self.save_notice = None;
        cx.notify();
    }

    pub(crate) fn update_openai_health(
        &mut self,
        health: OpenAiProviderHealthV1,
        cx: &mut Context<Self>,
    ) {
        self.openai_health = health;
        cx.notify();
    }

    fn toggle_larger_reveal(&mut self, cx: &mut Context<Self>) {
        self.draft.blobs.allow_larger_local_reveal = !self.draft.blobs.allow_larger_local_reveal;
        self.save_notice = None;
        cx.notify();
    }

    fn save_configuration(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let candidate = match self.candidate(cx) {
            Ok(candidate) if candidate != self.saved_config => candidate,
            Ok(_) => return,
            Err(error) => {
                self.save_notice = Some((error, Theme::RED));
                cx.notify();
                return;
            }
        };
        self.saving = true;
        self.save_notice = None;
        let task = cx.background_spawn({
            let candidate = candidate.clone();
            async move { candidate.save() }
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(path) => {
                        this.saved_config = candidate;
                        this.save_notice = Some((
                            format!(
                                "Saved to {}. Restart Perseval to apply runtime changes.",
                                path.display()
                            ),
                            Theme::GREEN,
                        ));
                    }
                    Err(error) => {
                        this.save_notice = Some((error.to_string(), Theme::RED));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn discard_changes(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.draft = self.saved_config.clone();
        let values = [
            (
                self.workspace_id.clone(),
                self.saved_config.workspace_id.clone(),
            ),
            (
                self.workspace_dir.clone(),
                self.saved_config.workspace_dir.display().to_string(),
            ),
            (
                self.reviewer_ref.clone(),
                self.saved_config.reviewer_ref.clone(),
            ),
            (
                self.otlp_bind.clone(),
                self.saved_config.otlp.bind_addr.to_string(),
            ),
            (
                self.inline_attribute_kib.clone(),
                (self.saved_config.blobs.inline_attribute_bytes / 1_024).to_string(),
            ),
            (
                self.default_preview_kib.clone(),
                (self.saved_config.query.blob_preview_bytes / 1_024).to_string(),
            ),
            (
                self.maximum_reveal_mib.clone(),
                (self.saved_config.blobs.maximum_local_reveal_bytes / (1_024 * 1_024)).to_string(),
            ),
            (
                self.openai_embedding_model.clone(),
                self.saved_config.analysis.openai.embedding_model.clone(),
            ),
            (
                self.openai_chat_model.clone(),
                self.saved_config.analysis.openai.chat_model.clone(),
            ),
        ];
        for (input, value) in values {
            input.update(cx, |input, cx| input.set_text(value, cx));
        }
        self.save_notice = None;
        cx.notify();
    }

    fn set_text_scale(&mut self, text_scale: TextScale, cx: &mut Context<Self>) {
        self.appearance.text_scale = text_scale;
        cx.emit(SettingsEvent::AppearanceChanged(self.appearance.clone()));
        cx.notify();
    }

    fn toggle_reduced_motion(&mut self, cx: &mut Context<Self>) {
        self.appearance.reduced_motion = !self.appearance.reduced_motion;
        cx.emit(SettingsEvent::AppearanceChanged(self.appearance.clone()));
        cx.notify();
    }

    fn select_category(&mut self, category: SettingsCategory, cx: &mut Context<Self>) {
        self.selected_category = category;
        self.content_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn category_navigation(&self, compact: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let mut navigation = div()
            .id("settings-categories")
            .role(Role::TabList)
            .aria_label("Settings categories")
            .flex()
            .gap_2()
            .when(compact, |navigation| {
                navigation.flex_wrap().p_4().border_b_1()
            })
            .when(!compact, |navigation| {
                navigation
                    .h_full()
                    .min_h_0()
                    .w(px(220.))
                    .flex_none()
                    .flex_col()
                    .overflow_y_scroll()
                    .p_3()
                    .border_r_1()
            })
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL);

        for (index, category) in SettingsCategory::ALL.into_iter().enumerate() {
            let selected = self.selected_category == category;
            let entity = cx.entity().clone();
            navigation = navigation.child(
                div()
                    .id(("settings-category", index))
                    .role(Role::Tab)
                    .aria_label(category.label())
                    .aria_selected(selected)
                    .tab_index(0)
                    .min_h(px(40.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(compact, |item| item.flex_1().justify_center())
                    .when(!compact, |item| item.w_full())
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if selected { Theme::CYAN } else { Theme::PANEL })
                    .bg(if selected {
                        Theme::ACCENT_MUTED
                    } else {
                        Theme::PANEL
                    })
                    .text_xs()
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if selected { Theme::TEXT } else { Theme::MUTED })
                    .cursor_pointer()
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        entity.update(cx, |this, cx| this.select_category(category, cx));
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_category(category, cx)))
                    .child(icon(category.icon(), 16., selected))
                    .child(category.label()),
            );
        }
        navigation
    }

    fn category_sections(&self, stack_setting_rows: bool, cx: &mut Context<Self>) -> gpui::Div {
        match self.selected_category {
            SettingsCategory::WorkspaceStorage => div()
                .child(self.runtime_section(stack_setting_rows, cx))
                .child(self.storage_section(stack_setting_rows)),
            SettingsCategory::PrivacyPayloads => {
                div().child(self.privacy_section(stack_setting_rows, cx))
            }
            SettingsCategory::AiFeatures => {
                div().child(self.analysis_section(stack_setting_rows, cx))
            }
            SettingsCategory::Appearance => div().child(self.appearance_section(cx)),
        }
    }

    fn storage_section(&self, stack_rows: bool) -> impl IntoElement {
        section(
            "Current storage",
            "The workspace open in this process. A saved workspace change takes effect after restart.",
        )
            .child(setting_row(
                "Workspace",
                self.runtime_config.workspace_dir.display().to_string(),
                stack_rows,
            ))
            .child(setting_row(
                "Current size",
                self.workspace_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "Calculating…".into()),
                stack_rows,
            ))
            .when_some(self.size_error.clone(), |section, error| {
                section.child(notice(
                    "Workspace size unavailable",
                    error,
                    Theme::AMBER,
                ))
            })
            .child(setting_row(
                "Control and journal",
                self.runtime_config.workspace_dir.join("control.sqlite3").display().to_string(),
                stack_rows,
            ))
            .child(setting_row(
                "Analytical projection",
                self.runtime_config
                    .workspace_dir
                    .join("analytics/traces.duckdb")
                    .display()
                    .to_string(),
                stack_rows,
            ))
            .child(setting_row(
                "Content-addressed payloads",
                self.runtime_config.workspace_dir.join("blobs").display().to_string(),
                stack_rows,
            ))
            .child(setting_row(
                "Retention",
                "Keep all revisions until an explicit cleanup policy exists".into(),
                stack_rows,
            ))
            .child(notice(
                "Cleanup is intentionally unavailable",
                "Perseval will not delete trace or payload data behind your back. Retention and orphan-blob cleanup need a reviewed policy before destructive controls are enabled.".into(),
                Theme::AMBER,
            ))
            .child(setting_row(
                "Migration status",
                format!(
                    "Configuration schema v{} · forward-only store migrations applied",
                    self.runtime_config.schema_version
                ),
                stack_rows,
            ))
    }

    fn runtime_section(&self, stack_rows: bool, cx: &mut Context<Self>) -> impl IntoElement {
        section(
            "Workspace and ingestion",
            "Saved changes apply on the next launch because the runtime owns the database and OTLP listener.",
        )
        .child(editable_row(
            "Workspace ID",
            "Stable identity stored with projects and traces",
            self.workspace_id.clone(),
            stack_rows,
        ))
        .child(editable_row(
            "Workspace directory",
            "Durable SQLite, DuckDB, and payload location",
            self.workspace_dir.clone(),
            stack_rows,
        ))
        .child(editable_row(
            "Reviewer reference",
            "Identity attached to review decisions and eval drafts",
            self.reviewer_ref.clone(),
            stack_rows,
        ))
        .child(switch_row(
            "OTLP/HTTP receiver",
            "Accept local JSON and protobuf at /v1/traces",
            if self.draft.otlp.enabled { "On" } else { "Off" },
            self.draft.otlp.enabled,
            "toggle-otlp",
            cx.listener(|this, _, _, cx| this.toggle_otlp(cx)),
            stack_rows,
        ))
        .child(editable_row(
            "OTLP bind address",
            "Loopback addresses only",
            self.otlp_bind.clone(),
            stack_rows,
        ))
    }

    fn privacy_section(&self, stack_rows: bool, cx: &mut Context<Self>) -> impl IntoElement {
        section(
            "Privacy and payloads",
            "Set bounded defaults; payload bodies still require an explicit reveal.",
        )
        .child(editable_row(
            "Inline attribute limit (KiB)",
            "Larger and known-sensitive attributes become blob references",
            self.inline_attribute_kib.clone(),
            stack_rows,
        ))
        .child(editable_row(
            "Default payload preview (KiB)",
            "Maximum bytes read by the first reveal action",
            self.default_preview_kib.clone(),
            stack_rows,
        ))
        .child(switch_row(
            "Larger local reveal",
            "Allow an explicit second reveal above the default preview",
            if self.draft.blobs.allow_larger_local_reveal { "Allowed" } else { "Blocked" },
            self.draft.blobs.allow_larger_local_reveal,
            "toggle-larger-reveal",
            cx.listener(|this, _, _, cx| this.toggle_larger_reveal(cx)),
            stack_rows,
        ))
        .child(editable_row(
            "Maximum local reveal (MiB)",
            "Hard upper bound when larger reveal is allowed",
            self.maximum_reveal_mib.clone(),
            stack_rows,
        ))
        .child(setting_row(
            "Known sensitive payloads",
            "Prompts, messages, reasoning, source code, tool payloads, inputs, and outputs are externalized".into(),
            stack_rows,
        ))
        .child(notice(
            "Externalization is not redaction",
            "Payload bodies are hidden behind an explicit bounded reveal, but Perseval does not yet remove secrets or personal data from stored content. Configure telemetry at the producer accordingly.".into(),
            Theme::RED,
        ))
    }

    fn analysis_section(&self, stack_rows: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let provider_status = if !self.openai_health.enabled {
            "Hosted analysis is not active in this process. Saved changes apply after restart."
                .to_string()
        } else if !self.openai_health.configured {
            "OPENAI_API_KEY is not available to this process; deterministic analysis is still active."
                .to_string()
        } else if self.openai_health.running_jobs > 0 {
            format!(
                "{} hosted analysis job(s) running · {} completed",
                self.openai_health.running_jobs, self.openai_health.successful_jobs
            )
        } else if self.openai_health.degraded {
            self.openai_health.last_error.clone().unwrap_or_else(|| {
                "Hosted analysis is degraded; deterministic analysis is still active.".into()
            })
        } else {
            format!(
                "Ready · {} hosted jobs completed",
                self.openai_health.successful_jobs
            )
        };
        let provider_tint = if self.openai_health.degraded {
            Theme::AMBER
        } else {
            Theme::CYAN
        };
        section(
            "Analysis",
            "Control deterministic and hosted augmentations without changing stored trace data.",
        )
        .child(switch_row(
            "Feature similarity",
            "Use local trace features to improve failure-group similarity",
            if self.draft.analysis.feature_similarity_enabled { "On" } else { "Off" },
            self.draft.analysis.feature_similarity_enabled,
            "toggle-feature-similarity",
            cx.listener(|this, _, _, cx| this.toggle_feature_similarity(cx)),
            stack_rows,
        ))
        .child(notice(
            "Local and deterministic",
            "With OpenAI embeddings off, this uses the local signed-feature hash and makes no network calls. Hosted embeddings remain a separate explicit opt-in below.".into(),
            Theme::CYAN,
        ))
        .child(switch_row(
            "OpenAI augmentations",
            "Reveal and enable hosted features; no request occurs until a subfeature is on",
            if self.draft.analysis.openai.enabled { "On" } else { "Off" },
            self.draft.analysis.openai.enabled,
            "toggle-openai",
            cx.listener(|this, _, _, cx| this.toggle_openai(cx)),
            stack_rows,
        ))
        .when(self.draft.analysis.openai.enabled, |hosted| {
            hosted
                .child(switch_row(
                    "OpenAI embeddings",
                    "Embed only safe failure projections for secondary similarity cohorts",
                    if self.draft.analysis.openai.embeddings_enabled { "On" } else { "Off" },
                    self.draft.analysis.openai.embeddings_enabled,
                    "toggle-openai-embeddings",
                    cx.listener(|this, _, _, cx| this.toggle_openai_embeddings(cx)),
                    stack_rows,
                ))
                .child(switch_row(
                    "OpenAI cluster labels",
                    "Name locally fitted cohorts from their bounded representative cases",
                    if self.draft.analysis.openai.cluster_labels_enabled { "On" } else { "Off" },
                    self.draft.analysis.openai.cluster_labels_enabled,
                    "toggle-openai-cluster-labels",
                    cx.listener(|this, _, _, cx| this.toggle_openai_cluster_labels(cx)),
                    stack_rows,
                ))
                .child(switch_row(
                    "OpenAI semantic judge",
                    "Review structured behavior facts; raw payload bodies are never included",
                    if self.draft.analysis.openai.semantic_judge_enabled { "On" } else { "Off" },
                    self.draft.analysis.openai.semantic_judge_enabled,
                    "toggle-openai-semantic-judge",
                    cx.listener(|this, _, _, cx| this.toggle_openai_semantic_judge(cx)),
                    stack_rows,
                ))
                .child(editable_row(
                    "Embedding model",
                    "Used only when OpenAI embeddings are enabled",
                    self.openai_embedding_model.clone(),
                    stack_rows,
                ))
                .child(editable_row(
                    "Judge and label model",
                    "Used for structured semantic review and optional cohort labels",
                    self.openai_chat_model.clone(),
                    stack_rows,
                ))
                .child(notice("OpenAI runtime", provider_status, provider_tint))
                .child(notice(
                    "Privacy boundary",
                    "The API key stays in OPENAI_API_KEY and is never written to Perseval's config or databases. Hosted features receive only versioned safe projections; payload blobs, prompts, reasoning, source code, tool payloads, inputs, and outputs stay local.".into(),
                    Theme::GREEN,
                ))
        })
    }

    fn save_bar(
        &self,
        changed: bool,
        validation_error: Option<String>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let save_enabled = changed && validation_error.is_none() && !self.saving;
        let discard_enabled = changed && !self.saving;
        div()
            .flex_none()
            .w_full()
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(if changed { Theme::CYAN } else { Theme::BORDER })
            .bg(Theme::PANEL)
            .flex()
            .when(compact, |bar| bar.flex_col().items_start().gap_3())
            .when(!compact, |bar| bar.items_center().justify_between().gap_5())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if changed {
                                "Unsaved configuration changes"
                            } else {
                                "Configuration is saved"
                            }),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(validation_error.unwrap_or_else(|| {
                                "Appearance applies immediately; workspace, ingestion, privacy, and analysis settings apply after restart.".into()
                            })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap_2()
                    .child(action_button(
                        "Discard",
                        "Discard unsaved configuration changes",
                        discard_enabled,
                        false,
                        cx.listener(|this, _, _, cx| this.discard_changes(cx)),
                    ))
                    .child(action_button(
                        if self.saving { "Saving…" } else { "Save settings" },
                        "Save configuration settings",
                        save_enabled,
                        true,
                        cx.listener(|this, _, _, cx| this.save_configuration(cx)),
                    )),
            )
    }

    fn appearance_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut scales = div()
            .id("text-scale-options")
            .mt_3()
            .flex()
            .flex_wrap()
            .gap_2()
            .role(Role::RadioGroup)
            .aria_label("Text size");
        for (index, (scale, label)) in [
            (TextScale::Normal, "100%"),
            (TextScale::Large, "125%"),
            (TextScale::ExtraLarge, "150%"),
            (TextScale::Double, "200%"),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = self.appearance.text_scale == scale;
            let entity = cx.entity().clone();
            scales = scales.child(
                button(label, selected)
                    .id(("text-scale", index))
                    .role(Role::RadioButton)
                    .aria_label(format!("Text size {label}"))
                    .aria_selected(selected)
                    // GPUI's default accessible Click path synthesizes a mouse
                    // click at the node bounds. That does not activate controls
                    // outside the current scroll viewport, even though VoiceOver
                    // can still navigate to them. Handle the semantic action
                    // directly so activation never depends on a painted hitbox.
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        entity.update(cx, |this, cx| this.set_text_scale(scale, cx));
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_text_scale(scale, cx))),
            );
        }
        let entity = cx.entity().clone();
        section(
            "Appearance and motion",
            "Text scaling reflows the workbench; reduced motion keeps the same state cues without transitions.",
        )
        .child(
            div()
                .mt_4()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child("Text size"),
        )
        .child(scales)
        .child(
            button(
                if self.appearance.reduced_motion {
                    "Reduced motion on"
                } else {
                    "Reduced motion off"
                },
                self.appearance.reduced_motion,
            )
            .id("reduced-motion")
            .mt_4()
            .role(Role::Switch)
            .aria_label(if self.appearance.reduced_motion {
                "Reduced motion on"
            } else {
                "Reduced motion off"
            })
            .aria_toggled(if self.appearance.reduced_motion {
                Toggled::True
            } else {
                Toggled::False
            })
            .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                entity.update(cx, |this, cx| this.toggle_reduced_motion(cx));
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_reduced_motion(cx))),
        )
    }
}

impl EventEmitter<SettingsEvent> for SettingsScreen {}

impl Render for SettingsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = Breakpoint::for_window(window) == Breakpoint::Compact;
        let stack_setting_rows = compact && self.appearance.text_scale.factor() >= 1.5;
        let candidate = self.candidate(cx);
        let changed = candidate
            .as_ref()
            .is_ok_and(|candidate| candidate != &self.saved_config);
        let validation_error = candidate.err();
        let config_path = PersevalConfigV1::file_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|error| error.to_string());
        div()
            .id("settings")
            .role(gpui::Role::Document)
            .aria_label("Perseval workspace, storage, privacy, appearance, keyboard, and advanced settings")
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .px_6()
                    .py_5()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap_5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Settings"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .text_color(Theme::MUTED)
                                    .child("Workspace policy, privacy boundaries, AI providers, and accessibility preferences."),
                            ),
                    )
                    .when(!compact, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .max_w(px(420.))
                                .text_xs()
                                .text_right()
                                .text_color(Theme::DIM)
                                .child(format!(
                                    "Effective workspace: {} · {}",
                                    self.runtime_config.workspace_id, config_path
                                )),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .overflow_hidden()
                    .flex()
                    .when(compact, |body| body.flex_col())
                    .child(self.category_navigation(compact, cx))
                    .child(
                        div()
                            .id("settings-scroll")
                            .role(Role::TabPanel)
                            .aria_label(self.selected_category.label())
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.content_scroll)
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(
                                div()
                                    .flex_none()
                                    .w_full()
                                    .max_w(px(1_000.))
                                    .when(compact, |content| content.p_5())
                                    .when(!compact, |content| content.p_8())
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(self.selected_category.label()),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .max_w(px(680.))
                                            .text_sm()
                                            .text_color(Theme::MUTED)
                                            .child(self.selected_category.description()),
                                    )
                                    .when_some(self.save_notice.clone(), |content, (message, tint)| {
                                        content.child(notice("Settings", message, tint))
                                    })
                                    .when(environment_overrides_active(), |content| {
                                        content.child(notice(
                                            "Environment override active",
                                            "One or more launch environment variables currently override saved values. The TOML file is still updated, but the override wins until it is removed.".into(),
                                            Theme::AMBER,
                                        ))
                                    })
                                    .child(self.category_sections(stack_setting_rows, cx)),
                            ),
                    ),
            )
            .child(self.save_bar(changed, validation_error, compact, cx))
    }
}

fn required_text(
    input: &Entity<TextInput>,
    label: &str,
    cx: &Context<SettingsScreen>,
) -> Result<String, String> {
    let value = input.read(cx).text().trim().to_string();
    if value.is_empty() {
        Err(format!("{label} cannot be empty"))
    } else {
        Ok(value)
    }
}

fn parse_size(
    input: &Entity<TextInput>,
    label: &str,
    multiplier: usize,
    cx: &Context<SettingsScreen>,
) -> Result<usize, String> {
    let value = required_text(input, label, cx)?;
    let amount = value
        .parse::<usize>()
        .map_err(|_| format!("{label} must be a whole number"))?;
    if amount == 0 {
        return Err(format!("{label} must be greater than zero"));
    }
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("{label} is too large"))
}

fn environment_overrides_active() -> bool {
    [
        perseval_service::config::WORKSPACE_ENV,
        perseval_service::config::OTLP_ENABLED_ENV,
        perseval_service::config::OTLP_BIND_ENV,
        perseval_service::config::REVIEWER_REF_ENV,
        perseval_service::config::OPENAI_ENABLED_ENV,
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some())
}

fn directory_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    let mut pending = vec![PathBuf::from(root)];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(total)
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use gpui::{InputEvent, ScrollDelta, ScrollWheelEvent, TestAppContext, point, px};

    use super::*;

    #[test]
    fn byte_formatting_uses_binary_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[gpui::test]
    fn editable_settings_validate_and_update_the_draft(cx: &mut TestAppContext) {
        let screen = cx.new(|cx| {
            SettingsScreen::new(
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                cx,
            )
        });
        screen.update(cx, |screen, cx| {
            screen.toggle_otlp(cx);
            screen
                .reviewer_ref
                .update(cx, |input, cx| input.set_text("qa-reviewer", cx));
            let candidate = screen.candidate(cx).unwrap();
            assert!(candidate.otlp.enabled);
            assert_eq!(candidate.reviewer_ref, "qa-reviewer");

            screen
                .otlp_bind
                .update(cx, |input, cx| input.set_text("not-an-address", cx));
            assert!(screen.candidate(cx).is_err());
        });
    }

    #[gpui::test]
    fn hosted_feature_toggles_keep_dependencies_explicit(cx: &mut TestAppContext) {
        let screen = cx.new(|cx| {
            SettingsScreen::new(
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                cx,
            )
        });
        screen.update(cx, |screen, cx| {
            screen.toggle_openai_cluster_labels(cx);
            assert!(screen.draft.analysis.openai.enabled);
            assert!(screen.draft.analysis.openai.embeddings_enabled);
            assert!(screen.draft.analysis.openai.cluster_labels_enabled);
            assert!(screen.draft.analysis.feature_similarity_enabled);
            screen.candidate(cx).unwrap();

            screen.toggle_openai(cx);
            assert!(!screen.draft.analysis.openai.enabled);
            assert!(!screen.draft.analysis.openai.embeddings_enabled);
            assert!(!screen.draft.analysis.openai.cluster_labels_enabled);
            assert!(!screen.draft.analysis.openai.semantic_judge_enabled);
            screen.candidate(cx).unwrap();
        });
    }

    #[gpui::test]
    fn settings_categories_keep_each_workflow_focused(cx: &mut TestAppContext) {
        let screen = cx.new(|cx| {
            SettingsScreen::new(
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                cx,
            )
        });
        screen.update(cx, |screen, cx| {
            assert_eq!(screen.selected_category, SettingsCategory::WorkspaceStorage);
            screen.select_category(SettingsCategory::AiFeatures, cx);
            assert_eq!(screen.selected_category, SettingsCategory::AiFeatures);
            screen.select_category(SettingsCategory::Appearance, cx);
            assert_eq!(screen.selected_category, SettingsCategory::Appearance);
        });
    }

    #[gpui::test]
    fn settings_content_has_a_real_scroll_extent(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, cx| {
            SettingsScreen::new(
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                cx,
            )
        });
        cx.run_until_parked();
        let maximum = cx
            .read_window(&window, |screen, cx| {
                screen.read(cx).content_scroll.max_offset()
            })
            .expect("read settings scroll state");
        assert!(maximum.y > px(0.), "settings must overflow vertically");

        let viewport = cx
            .read_window(&window, |screen, cx| {
                screen.read(cx).content_scroll.bounds()
            })
            .expect("read settings viewport");
        window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    ScrollWheelEvent {
                        position: viewport.center(),
                        delta: ScrollDelta::Pixels(point(px(0.), px(-120.))),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .expect("dispatch settings scroll input");
        cx.run_until_parked();
        let offset = cx
            .read_window(&window, |screen, cx| {
                screen.read(cx).content_scroll.offset()
            })
            .expect("read scrolled settings offset");
        assert!(offset.y < px(0.), "wheel input must move settings content");
    }
}

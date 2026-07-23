use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AccessibleAction, AppContext, ClipboardItem, Context, Entity, EventEmitter, FontWeight,
    IntoElement, PathPromptOptions, Render, Role, ScrollHandle, Toggled, Window, div, point,
    prelude::*, px,
};
use perseval_service::{
    AgentContextGovernanceSummaryV1, AssessmentRuntimeHealthV1, ContextBackfillPreviewV1,
    LiveTraceService, OpenAiProviderHealthV1, PersevalConfigV1, ReviewAuthorityV1,
    TaskCompletionModelCatalogV1, TaskCompletionModelManager, TaxonomyGovernanceSummaryV1,
    inspect_managed_model,
};

use crate::components::{TextInput, button, button_state};
use crate::design::{Breakpoint, Theme};
use crate::icons::{AppIcon, icon};
use crate::workbench::{AppearancePreferencesV1, TextScale};

mod components;

use components::{
    action_button, editable_row, notice, review_row, section, setting_row, switch_row,
};

#[derive(Debug, Clone)]
pub(crate) enum SettingsEvent {
    AppearanceChanged(AppearancePreferencesV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    WorkspaceStorage,
    AgentSpecification,
    TasksIssueTypes,
    PrivacyPayloads,
    AiFeatures,
    Appearance,
}

impl SettingsCategory {
    const ALL: [Self; 6] = [
        Self::WorkspaceStorage,
        Self::AgentSpecification,
        Self::TasksIssueTypes,
        Self::PrivacyPayloads,
        Self::AiFeatures,
        Self::Appearance,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::WorkspaceStorage => "Workspace & collection",
            Self::AgentSpecification => "Agent specification",
            Self::TasksIssueTypes => "Tasks & issue types",
            Self::PrivacyPayloads => "Privacy & payloads",
            Self::AiFeatures => "AI features",
            Self::Appearance => "Appearance",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::WorkspaceStorage => "Choose where data lives and how local traces arrive.",
            Self::AgentSpecification => "Define what this agent should accomplish.",
            Self::TasksIssueTypes => "Define the tasks and issue types used in quality checks.",
            Self::PrivacyPayloads => "Control local payload previews and storage.",
            Self::AiFeatures => "Choose local or hosted analysis.",
            Self::Appearance => "Adjust reading scale and motion.",
        }
    }

    const fn icon(self) -> AppIcon {
        match self {
            Self::WorkspaceStorage => AppIcon::Database,
            Self::AgentSpecification => AppIcon::Evals,
            Self::TasksIssueTypes => AppIcon::Inbox,
            Self::PrivacyPayloads => AppIcon::Shield,
            Self::AiFeatures => AppIcon::Sparkles,
            Self::Appearance => AppIcon::Accessibility,
        }
    }
}

#[derive(Debug, Clone)]
enum ModelManagementState {
    Checking,
    Ready,
    Installing,
    Failed(String),
}

pub(crate) struct SettingsScreen {
    service: Option<Arc<LiveTraceService>>,
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
    local_model_artifact_dir: Entity<TextInput>,
    model_manager: Option<TaskCompletionModelManager>,
    model_catalog: Option<TaskCompletionModelCatalogV1>,
    model_management_state: ModelManagementState,
    openai_embedding_model: Entity<TextInput>,
    openai_chat_model: Entity<TextInput>,
    openai_health: OpenAiProviderHealthV1,
    assessment_health: AssessmentRuntimeHealthV1,
    selected_project_id: Option<String>,
    context_governance: AgentContextGovernanceSummaryV1,
    taxonomy_governance: TaxonomyGovernanceSummaryV1,
    preparing_context: bool,
    context_notice: Option<(String, gpui::Rgba)>,
    context_backfill_preview: Option<ContextBackfillPreviewV1>,
    preparing_taxonomy: bool,
    taxonomy_notice: Option<(String, gpui::Rgba)>,
    workspace_bytes: Option<u64>,
    size_error: Option<String>,
    saving: bool,
    save_notice: Option<(String, gpui::Rgba)>,
    appearance: AppearancePreferencesV1,
    selected_category: SettingsCategory,
    content_scroll: ScrollHandle,
}

impl SettingsScreen {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        service: Option<Arc<LiveTraceService>>,
        config: PersevalConfigV1,
        appearance: AppearancePreferencesV1,
        openai_health: OpenAiProviderHealthV1,
        assessment_health: AssessmentRuntimeHealthV1,
        selected_project_id: Option<String>,
        context_governance: AgentContextGovernanceSummaryV1,
        taxonomy_governance: TaxonomyGovernanceSummaryV1,
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
        let local_model_artifact_dir = Self::input(
            &config
                .assessments
                .local_model_artifact_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            "Directory containing manifest.json and model.onnx",
            2_048,
            cx,
        );
        let model_manager = TaskCompletionModelManager::production().ok();
        let model_management_state = if model_manager.is_some() {
            ModelManagementState::Checking
        } else {
            ModelManagementState::Failed(
                "Perseval could not locate its local model directory.".into(),
            )
        };
        if let Some(manager) = model_manager.clone() {
            let task = cx.background_spawn(async move { manager.latest_release() });
            cx.spawn(async move |weak, cx| {
                let result = task.await;
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(catalog) => {
                            this.model_catalog = Some(catalog);
                            this.model_management_state = ModelManagementState::Ready;
                        }
                        Err(error) => {
                            this.model_management_state =
                                ModelManagementState::Failed(error.to_string());
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
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
            service,
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
            local_model_artifact_dir,
            model_manager,
            model_catalog: None,
            model_management_state,
            openai_embedding_model,
            openai_chat_model,
            openai_health,
            assessment_health,
            selected_project_id,
            context_governance,
            taxonomy_governance,
            preparing_context: false,
            context_notice: None,
            context_backfill_preview: None,
            preparing_taxonomy: false,
            taxonomy_notice: None,
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
        let local_model_artifact_dir = self
            .local_model_artifact_dir
            .read(cx)
            .text()
            .trim()
            .to_string();
        candidate.assessments.local_model_artifact_dir =
            (!local_model_artifact_dir.is_empty()).then(|| PathBuf::from(local_model_artifact_dir));
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

    fn toggle_assessments(&mut self, cx: &mut Context<Self>) {
        self.draft.assessments.enabled = !self.draft.assessments.enabled;
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

    pub(crate) fn update_assessment_health(
        &mut self,
        health: AssessmentRuntimeHealthV1,
        cx: &mut Context<Self>,
    ) {
        self.assessment_health = health;
        cx.notify();
    }

    pub(crate) fn update_governance(
        &mut self,
        selected_project_id: Option<String>,
        context_governance: AgentContextGovernanceSummaryV1,
        taxonomy_governance: TaxonomyGovernanceSummaryV1,
        cx: &mut Context<Self>,
    ) {
        if self.selected_project_id != selected_project_id {
            self.context_backfill_preview = None;
            self.context_notice = None;
            self.taxonomy_notice = None;
        }
        self.selected_project_id = selected_project_id;
        self.context_governance = context_governance;
        self.taxonomy_governance = taxonomy_governance;
        cx.notify();
    }

    fn copy_context_preparation_request(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project_id.as_deref() else {
            self.context_notice = Some(("Choose one project first.".into(), Theme::AMBER));
            cx.notify();
            return;
        };
        let request = format!(
            "Prepare a sourced Perseval agent-specification draft for project {project_id}. Read only repository files I explicitly approve. Preserve field-level source snapshot, locator, sensitivity, provenance, and inference confidence. Mark conflicts and missing human decisions; do not mark inferred values as user-declared and do not activate any release."
        );
        cx.write_to_clipboard(ClipboardItem::new_string(request));
        self.context_notice = Some((
            "Copied a bounded Codex/MCP preparation request. Paste it into the coding agent that has access to the approved repository.".into(),
            Theme::GREEN,
        ));
        cx.notify();
    }

    fn prepare_context_from_repository(&mut self, cx: &mut Context<Self>) {
        if self.preparing_context {
            return;
        }
        let (Some(service), Some(project_id)) =
            (self.service.clone(), self.selected_project_id.clone())
        else {
            self.context_notice = Some(("Choose one project first.".into(), Theme::AMBER));
            cx.notify();
            return;
        };
        let reviewer = self.reviewer_ref.read(cx).text().trim().to_string();
        self.preparing_context = true;
        self.context_notice = Some((
            "Choose the repository directory whose documentation and manifests Perseval may read locally…".into(),
            Theme::CYAN,
        ));
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Prepare agent specification".into()),
        });
        cx.spawn(async move |weak, cx| {
            let selected = picker.await;
            let path = match selected {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.preparing_context = false;
                        this.context_notice = Some((
                            format!("Could not open the repository picker: {error}"),
                            Theme::RED,
                        ));
                        cx.notify();
                    });
                    return;
                }
            };
            let Some(path) = path else {
                let _ = weak.update(cx, |this, cx| {
                    this.preparing_context = false;
                    this.context_notice = None;
                    cx.notify();
                });
                return;
            };
            let task = cx.background_spawn({
                let service = service.clone();
                let project_id = project_id.clone();
                async move {
                    service.prepare_agent_context_from_repository(
                        &project_id,
                        &path,
                        &reviewer,
                    )?;
                    let context = service.agent_context_governance_summary(&project_id)?;
                    Ok::<_, perseval_service::LiveServiceError>(context)
                }
            });
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.preparing_context = false;
                match result {
                    Ok(context) => {
                        this.context_governance = context;
                        this.context_notice = Some((
                            "Prepared a local sourced draft. Review the purpose, source count, inferred ownership/risk, conflicts, and binding impact before activation.".into(),
                            Theme::GREEN,
                        ));
                    }
                    Err(error) => {
                        this.context_notice = Some((error.to_string(), Theme::RED));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn approve_context_draft(&mut self, cx: &mut Context<Self>) {
        if self.preparing_context {
            return;
        }
        let Some(service) = self.service.clone() else {
            self.context_notice = Some((
                "The local Perseval service is unavailable. Restart Perseval and try again.".into(),
                Theme::RED,
            ));
            cx.notify();
            return;
        };
        let Some(project_id) = self.selected_project_id.clone() else {
            self.context_notice = Some((
                "Choose a project before approving its agent specification.".into(),
                Theme::RED,
            ));
            cx.notify();
            return;
        };
        let Some(draft) = self.context_governance.latest_draft.clone() else {
            self.context_notice = Some((
                "This agent specification draft is no longer available. Prepare a new draft."
                    .into(),
                Theme::RED,
            ));
            cx.notify();
            return;
        };
        let reviewer = self.reviewer_ref.read(cx).text().trim().to_string();
        self.preparing_context = true;
        self.context_notice = Some((
            "Activating an immutable specification release…".into(),
            Theme::CYAN,
        ));
        let result = service
            .approve_agent_context_draft(&draft.draft_id, &reviewer, ReviewAuthorityV1::Human)
            .and_then(|release_id| {
                service
                    .agent_context_governance_summary(&project_id)
                    .map(|context| (release_id, context))
            });
        self.preparing_context = false;
        match result {
            Ok((release_id, context)) => {
                self.context_governance = context;
                self.context_notice = Some((
                    format!(
                        "Activated immutable release {}. Configure reviewed trace-binding rules before running context-dependent evaluators.",
                        short_identity(&release_id)
                    ),
                    Theme::GREEN,
                ));
            }
            Err(error) => {
                self.context_notice = Some((error.to_string(), Theme::RED));
            }
        }
        cx.notify();
    }

    fn preview_context_backfill(&mut self, cx: &mut Context<Self>) {
        let (Some(service), Some(project_id), Some(context_release_id)) = (
            self.service.clone(),
            self.selected_project_id.clone(),
            self.context_governance.latest_context_release_id.clone(),
        ) else {
            return;
        };
        self.preparing_context = true;
        self.context_notice = Some((
            "Calculating the exact finalized revisions affected by this reviewed default…".into(),
            Theme::CYAN,
        ));
        let task = cx.background_spawn(async move {
            service.preview_context_backfill(&project_id, &context_release_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.preparing_context = false;
                match result {
                    Ok(preview) => {
                        this.context_notice = Some((
                            format!(
                                "Preview ready: {} exact finalized revision(s), including {} currently unresolved. No historical binding or assessment has changed.",
                                preview.affected_revisions.len(),
                                preview.unresolved_revisions.len()
                            ),
                            Theme::GREEN,
                        ));
                        this.context_backfill_preview = Some(preview);
                    }
                    Err(error) => this.context_notice = Some((error.to_string(), Theme::RED)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_context_backfill(&mut self, cx: &mut Context<Self>) {
        let (Some(service), Some(preview)) =
            (self.service.clone(), self.context_backfill_preview.clone())
        else {
            return;
        };
        let reviewer = self.reviewer_ref.read(cx).text().trim().to_string();
        let project_id = preview.project_id.clone();
        let context_release_id = preview.context_release_id.clone();
        let selection_hash = preview.selection_hash.clone();
        self.preparing_context = true;
        self.context_notice = Some((
            "Applying the reviewed default to the exact previewed revisions…".into(),
            Theme::CYAN,
        ));
        let task = cx.background_spawn(async move {
            let result = service.apply_reviewed_default_context_backfill(
                &project_id,
                &context_release_id,
                &selection_hash,
                &reviewer,
                ReviewAuthorityV1::Human,
            )?;
            let governance = service.agent_context_governance_summary(&project_id)?;
            Ok::<_, perseval_service::LiveServiceError>((result, governance))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.preparing_context = false;
                match result {
                    Ok((result, governance)) => {
                        this.context_governance = governance;
                        this.context_backfill_preview = None;
                        this.context_notice = Some((
                            format!(
                                "Bound {} exact finalized revision(s) through reviewed rule {}. Existing historical bindings remain readable.",
                                result.bound_revisions.len(),
                                short_identity(&result.binding_rule_release_id)
                            ),
                            Theme::GREEN,
                        ));
                    }
                    Err(error) => this.context_notice = Some((error.to_string(), Theme::RED)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn prepare_taxonomy_from_specification(&mut self, cx: &mut Context<Self>) {
        let (Some(service), Some(project_id)) =
            (self.service.clone(), self.selected_project_id.clone())
        else {
            return;
        };
        let reviewer = self.reviewer_ref.read(cx).text().trim().to_string();
        self.preparing_taxonomy = true;
        self.taxonomy_notice = Some((
            "Preparing a sourced definition draft from the active agent specification…".into(),
            Theme::CYAN,
        ));
        let task = cx.background_spawn(async move {
            service.prepare_taxonomy_from_agent_context(&project_id, &reviewer)?;
            service.taxonomy_governance_summary(&project_id)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.preparing_taxonomy = false;
                match result {
                    Ok(governance) => {
                        this.taxonomy_governance = governance;
                        this.taxonomy_notice = Some((
                            "Prepared an additive definition draft. Review every task/capability name, source, privacy class, and release change before activation.".into(),
                            Theme::GREEN,
                        ));
                    }
                    Err(error) => this.taxonomy_notice = Some((error.to_string(), Theme::RED)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn approve_taxonomy_draft(&mut self, cx: &mut Context<Self>) {
        let (Some(service), Some(project_id), Some(draft_id)) = (
            self.service.clone(),
            self.selected_project_id.clone(),
            self.taxonomy_governance.latest_draft_id.clone(),
        ) else {
            return;
        };
        let reviewer = self.reviewer_ref.read(cx).text().trim().to_string();
        self.preparing_taxonomy = true;
        self.taxonomy_notice = Some((
            "Activating the reviewed immutable definition release…".into(),
            Theme::CYAN,
        ));
        let task = cx.background_spawn(async move {
            let release_id = service.approve_taxonomy_change_draft(
                &draft_id,
                &reviewer,
                ReviewAuthorityV1::Human,
            )?;
            let governance = service.taxonomy_governance_summary(&project_id)?;
            if governance.latest_release_id.as_deref() != Some(release_id.as_str())
                || governance.latest_draft_id.as_deref() == Some(draft_id.as_str())
            {
                return Err(perseval_service::LiveServiceError::Writer(
                    "the reviewed definition release did not become active; no success state was shown"
                        .into(),
                ));
            }
            Ok::<_, perseval_service::LiveServiceError>((release_id, governance))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.preparing_taxonomy = false;
                match result {
                    Ok((release_id, governance)) => {
                        this.taxonomy_governance = governance;
                        this.taxonomy_notice = Some((
                            format!(
                                "Activated immutable definition release {}. Prior releases and assignments remain readable.",
                                short_identity(&release_id)
                            ),
                            Theme::GREEN,
                        ));
                    }
                    Err(error) => this.taxonomy_notice = Some((error.to_string(), Theme::RED)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_larger_reveal(&mut self, cx: &mut Context<Self>) {
        self.draft.blobs.allow_larger_local_reveal = !self.draft.blobs.allow_larger_local_reveal;
        self.save_notice = None;
        cx.notify();
    }

    fn refresh_model_catalog(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.model_management_state,
            ModelManagementState::Checking | ModelManagementState::Installing
        ) {
            return;
        }
        let Some(manager) = self.model_manager.clone() else {
            self.model_management_state =
                ModelManagementState::Failed("The local model directory is unavailable.".into());
            cx.notify();
            return;
        };
        self.model_management_state = ModelManagementState::Checking;
        let task = cx.background_spawn(async move { manager.latest_release() });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(catalog) => {
                        this.model_catalog = Some(catalog);
                        this.model_management_state = ModelManagementState::Ready;
                    }
                    Err(error) => {
                        this.model_management_state =
                            ModelManagementState::Failed(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn install_latest_model(&mut self, cx: &mut Context<Self>) {
        if self.saving
            || matches!(
                self.model_management_state,
                ModelManagementState::Checking | ModelManagementState::Installing
            )
        {
            return;
        }
        let (Some(manager), Some(catalog)) =
            (self.model_manager.clone(), self.model_catalog.clone())
        else {
            self.model_management_state =
                ModelManagementState::Failed("No verified model release is available.".into());
            cx.notify();
            return;
        };
        self.model_management_state = ModelManagementState::Installing;
        self.save_notice = Some((
            "Downloading and verifying the local model…".into(),
            Theme::CYAN,
        ));
        let task = cx.background_spawn(async move { manager.install(&catalog) });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(model) => {
                        this.model_management_state = ModelManagementState::Ready;
                        this.draft.assessments.enabled = true;
                        this.draft.assessments.local_model_artifact_dir =
                            Some(model.artifact_dir.clone());
                        this.local_model_artifact_dir.update(cx, |input, cx| {
                            input.set_text(model.artifact_dir.display().to_string(), cx);
                        });
                        this.save_notice = Some((
                            "Model verified. Saving settings and restarting Perseval…".into(),
                            Theme::GREEN,
                        ));
                        this.save_and_restart(cx);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.model_management_state = ModelManagementState::Failed(message.clone());
                        this.save_notice = Some((message, Theme::RED));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_and_restart(&mut self, cx: &mut Context<Self>) {
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
        if let Err(error) =
            validate_local_model_folder(candidate.assessments.local_model_artifact_dir.as_deref())
        {
            self.save_notice = Some((error, Theme::RED));
            cx.notify();
            return;
        }
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
                    Ok(_) => {
                        this.saved_config = candidate;
                        this.save_notice =
                            Some(("Saved. Restarting Perseval…".into(), Theme::GREEN));
                        cx.restart();
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
                self.local_model_artifact_dir.clone(),
                self.saved_config
                    .assessments
                    .local_model_artifact_dir
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
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
            SettingsCategory::AgentSpecification => {
                div().child(self.agent_specification_section(stack_setting_rows, cx))
            }
            SettingsCategory::TasksIssueTypes => {
                div().child(self.tasks_issue_types_section(stack_setting_rows, cx))
            }
            SettingsCategory::PrivacyPayloads => {
                div().child(self.privacy_section(stack_setting_rows, cx))
            }
            SettingsCategory::AiFeatures => div()
                .child(self.learned_assessment_section(stack_setting_rows, cx))
                .child(self.analysis_section(stack_setting_rows, cx)),
            SettingsCategory::Appearance => div().child(self.appearance_section(cx)),
        }
    }

    fn agent_specification_section(
        &self,
        stack_rows: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let governance = &self.context_governance;
        let mut view = section(
            "Agent specification",
            "Review the draft before it guides quality checks.",
        );
        if self.selected_project_id.is_none() {
            return view.child(notice(
                "Choose one project",
                "Agent intent, source approval, releases, and trace bindings are project-scoped. All Projects stays read-only.".into(),
                Theme::AMBER,
            ));
        }
        if let Some(draft) = governance.latest_draft.as_ref() {
            let application = draft_context_text(draft, "/identity/application_name/value")
                .unwrap_or_else(|| "Application name needs review".into());
            let purpose = draft_context_text(draft, "/intent/purpose/value")
                .unwrap_or_else(|| "Purpose needs review".into());
            view = view
                .child(setting_row(
                    "Prepared draft",
                    format!("{} · {}", application, purpose),
                    stack_rows,
                ))
                .child(setting_row(
                    "Source review",
                    format!(
                        "{} approved snapshot(s) · prepared by {}",
                        governance.source_snapshot_count, draft.created_by
                    ),
                    stack_rows,
                ))
                .child(setting_row(
                    "Human decisions",
                    format!(
                        "{} unresolved · {} conflicting",
                        draft.unresolved_field_ids.len(),
                        draft.conflicting_field_ids.len()
                    ),
                    stack_rows,
                ));
            if let Some(files) = draft
                .source_manifest
                .get("files")
                .and_then(serde_json::Value::as_array)
            {
                for file in files {
                    let path = file
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("approved source");
                    let hash = file
                        .get("sha256")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    let bytes = file
                        .get("bytes")
                        .and_then(serde_json::Value::as_u64)
                        .map(|bytes| format!(" · {bytes} bytes"))
                        .unwrap_or_default();
                    view = view.child(review_row(
                        "Approved source",
                        path.to_string(),
                        format!("SHA-256 {hash}{bytes} · approved local input"),
                        stack_rows,
                    ));
                }
            }
            for item in context_review_items(&draft.proposed_context) {
                view = view.child(review_row(
                    &format!("Proposed {}", item.label.to_lowercase()),
                    item.value,
                    item.detail,
                    stack_rows,
                ));
            }
        } else if governance.active_release_count == 0 {
            view = view
                .child(notice(
                    "No agent specification yet",
                    "Choose the agent's repository. Perseval drafts its purpose, tasks, and success criteria locally.".into(),
                    Theme::AMBER,
                ))
                .child(
                    div()
                        .mt_4()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(action_button(
                            if self.preparing_context {
                                "Preparing…"
                            } else {
                                "Create draft from repository…"
                            },
                            "Read bounded repository docs locally",
                            !self.preparing_context,
                            true,
                            cx.listener(|this, _, _, cx| {
                                this.prepare_context_from_repository(cx)
                            }),
                        ))
                        .child(action_button(
                            "Ask Codex to fill this",
                            "Copy a safe context request",
                            !self.preparing_context,
                            false,
                            cx.listener(|this, _, _, cx| {
                                this.copy_context_preparation_request(cx)
                            }),
                        )),
                );
        } else {
            view = view.child(
                div()
                    .mt_4()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(action_button(
                        if self.preparing_context {
                            "Preparing update…"
                        } else {
                            "Update from repository…"
                        },
                        "Refresh from bounded local docs",
                        !self.preparing_context,
                        false,
                        cx.listener(|this, _, _, cx| this.prepare_context_from_repository(cx)),
                    ))
                    .child(action_button(
                        "Ask Codex to improve it",
                        "Copy a safe context request",
                        !self.preparing_context,
                        false,
                        cx.listener(|this, _, _, cx| this.copy_context_preparation_request(cx)),
                    )),
            );
        }
        if let Some(draft) = governance.latest_draft.as_ref() {
            let can_activate = draft.unresolved_field_ids.is_empty()
                && draft.conflicting_field_ids.is_empty()
                && !self.preparing_context;
            view = view.child(
                div()
                    .mt_4()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(action_button(
                        if self.preparing_context {
                            "Working…"
                        } else {
                            "Approve & use"
                        },
                        "Use this version for new quality checks",
                        can_activate,
                        true,
                        cx.listener(|this, _, _, cx| this.approve_context_draft(cx)),
                    ))
                    .child(action_button(
                        "Start over from repository…",
                        "Keep this draft and prepare another",
                        !self.preparing_context,
                        false,
                        cx.listener(|this, _, _, cx| this.prepare_context_from_repository(cx)),
                    )),
            );
        }
        view = view
        .child(setting_row(
            "Current version",
            match governance.latest_context_release_id.as_deref() {
                Some(release) => format!(
                    "{} version(s) · latest {}",
                    governance.active_release_count,
                    short_identity(release)
                ),
                None => "Not approved yet".into(),
            },
            stack_rows,
        ))
        .when_some(
            governance
                .latest_draft
                .is_none()
                .then_some(governance.latest_context_release.as_ref())
                .flatten(),
            |view, release| {
                let release_json = serde_json::to_value(release).unwrap_or_default();
                let mut view = view.child(notice(
                    "Current specification",
                    "Used for new quality checks. Existing results keep their original version.".into(),
                    Theme::CYAN,
                ));
                for item in context_review_items(&release_json) {
                    view = view.child(review_row(
                        &item.label,
                        item.value,
                        item.detail,
                        stack_rows,
                    ));
                }
                view
            },
        )
        .child(setting_row(
            "Trace links",
            format!(
                "{} resolved · {} unresolved · {} ambiguous exact revisions",
                governance.resolved_bindings,
                governance.unresolved_bindings,
                governance.ambiguous_bindings
            ),
            stack_rows,
        ))
        .child(setting_row(
            "Missing links",
            "Skipped until you link them; existing results never change".into(),
            stack_rows,
        ))
        .child(notice(
            "You approve changes",
            "Codex can prepare a draft, but cannot activate it.".into(),
            Theme::GREEN,
        ));
        if governance.latest_context_release_id.is_some() {
            view = view.child(
                div()
                    .mt_4()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(action_button(
                        if self.preparing_context {
                            "Checking…"
                        } else {
                            "Link existing traces"
                        },
                        "Preview which finalized traces will use this specification",
                        !self.preparing_context,
                        self.context_backfill_preview.is_none(),
                        cx.listener(|this, _, _, cx| this.preview_context_backfill(cx)),
                    ))
                    .when_some(
                        self.context_backfill_preview.as_ref(),
                        |actions, preview| {
                            actions.child(action_button(
                                &format!("Link {} trace(s)", preview.affected_revisions.len()),
                                "Apply the reviewed preview",
                                !self.preparing_context,
                                true,
                                cx.listener(|this, _, _, cx| this.apply_context_backfill(cx)),
                            ))
                        },
                    ),
            );
        }
        if let Some(preview) = self.context_backfill_preview.as_ref() {
            view = view
                .child(setting_row(
                    "Trace preview",
                    format!(
                        "{} finalized trace(s) · {} unlinked",
                        preview.affected_revisions.len(),
                        preview.unresolved_revisions.len()
                    ),
                    stack_rows,
                ))
                .child(setting_row(
                    "Selection identity",
                    short_identity(&preview.selection_hash).into(),
                    stack_rows,
                ));
        }
        if let Some((message, tint)) = self.context_notice.as_ref() {
            view = view.child(notice("Agent specification status", message.clone(), *tint));
        }
        view
    }

    fn tasks_issue_types_section(
        &self,
        stack_rows: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let governance = &self.taxonomy_governance;
        let mut view = section(
            "Tasks and issue types",
            "Review the task definitions Perseval will use.",
        );
        if self.selected_project_id.is_none() {
            return view.child(notice(
                "Choose one project",
                "Definition drafts and releases are project-scoped. All Projects stays read-only."
                    .into(),
                Theme::AMBER,
            ));
        }
        if governance.active_release_count == 0 && governance.drafts_in_review == 0 {
            view = view.child(notice(
                "No task definitions yet",
                "Create them from the approved agent specification.".into(),
                Theme::AMBER,
            ));
        }
        view = view.child(setting_row(
            "Status",
            match governance.latest_release_id.as_deref() {
                Some(release) => format!(
                    "{} version(s) · {} active definition(s) · latest {}",
                    governance.active_release_count,
                    governance.active_node_count,
                    short_identity(release)
                ),
                None => format!("{} draft(s) to review", governance.drafts_in_review),
            },
            stack_rows,
        ));
        view = view.child(setting_row(
            "Draft",
            governance
                .latest_draft_id
                .as_deref()
                .map(|draft| format!("Review {}", short_identity(draft)))
                .unwrap_or_else(|| "None".into()),
            stack_rows,
        ));
        if let Some((message, tint)) = self.taxonomy_notice.as_ref() {
            view = view.child(notice("Definition review status", message.clone(), *tint));
        }
        if let Some(draft) = governance.latest_draft.as_ref() {
            let source_release = draft
                .source_manifest
                .get("agent_context_release_id")
                .and_then(serde_json::Value::as_str)
                .map(short_identity)
                .unwrap_or("unknown");
            let task_count = draft
                .proposal
                .nodes
                .iter()
                .filter(|node| {
                    node.dimension == perseval_service::analysis::TaxonomyDimensionV1::Task
                })
                .count();
            let capability_count = draft
                .proposal
                .nodes
                .iter()
                .filter(|node| {
                    node.dimension == perseval_service::analysis::TaxonomyDimensionV1::Capability
                })
                .count();
            let issue_count = draft
                .proposal
                .nodes
                .iter()
                .filter(|node| {
                    matches!(
                        node.dimension,
                        perseval_service::analysis::TaxonomyDimensionV1::FailureMode
                            | perseval_service::analysis::TaxonomyDimensionV1::RootCause
                            | perseval_service::analysis::TaxonomyDimensionV1::Severity
                    )
                })
                .count();
            view = view
                .child(setting_row(
                    "Release review",
                    format!(
                        "{} definition(s) · {} change(s) · specification {}",
                        draft.proposal.nodes.len(),
                        draft.proposal.lineage.len(),
                        source_release
                    ),
                    stack_rows,
                ))
                .child(setting_row(
                    "Prepared by",
                    draft.created_by.clone(),
                    stack_rows,
                ))
                .child(setting_row(
                    "Includes",
                    format!(
                        "{task_count} task(s) · {capability_count} capability(s) · {issue_count} issue type(s)"
                    ),
                    stack_rows,
                ));
            if capability_count == 0 || issue_count == 0 {
                view = view.child(notice(
                    "Only supported definitions are included",
                    "Missing capabilities and issue types can be added later.".into(),
                    Theme::AMBER,
                ));
            }
            let mut definition_review = div()
                .id("definition-release-review")
                .role(Role::Group)
                .aria_label("Complete definition release review; scroll this list when it is long")
                .mt_3()
                .max_h(px(360.))
                .overflow_y_scroll()
                .pr_2();
            for node in &draft.proposal.nodes {
                definition_review = definition_review.child(review_row(
                    taxonomy_dimension_label(node.dimension),
                    format!("{}\n{}", node.name, node.description),
                    format!(
                        "Stable ID {} · provenance {} · sensitivity {} · state {:?}",
                        node.node_id, node.provenance, node.sensitivity, node.state
                    ),
                    stack_rows,
                ));
            }
            view = view.child(definition_review);
        } else {
            for node in &governance.active_nodes {
                view = view.child(review_row(
                    taxonomy_dimension_label(node.dimension),
                    format!("{}\n{}", node.name, node.description),
                    format!(
                        "Stable ID {} · provenance {} · sensitivity {} · state {:?}",
                        node.node_id, node.provenance, node.sensitivity, node.state
                    ),
                    stack_rows,
                ));
            }
        }
        if self.context_governance.latest_context_release_id.is_none() {
            view = view.child(notice(
                "Approve the agent specification first",
                "Then return here to create its task definitions.".into(),
                Theme::AMBER,
            ));
        } else {
            view = view.child(
                div()
                    .mt_4()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(action_button(
                        if self.preparing_taxonomy {
                            "Working…"
                        } else if governance.latest_draft.is_some() {
                            "Refresh from specification"
                        } else {
                            "Create task definitions"
                        },
                        "Use the approved agent specification",
                        !self.preparing_taxonomy,
                        governance.latest_draft.is_none(),
                        cx.listener(|this, _, _, cx| this.prepare_taxonomy_from_specification(cx)),
                    ))
                    .when(governance.latest_draft.is_some(), |actions| {
                        let can_activate = !self.preparing_taxonomy;
                        actions.child(
                            button_state("Approve & use", true, can_activate)
                                .id("approve-task-definitions")
                                .role(Role::Button)
                                .aria_label("Use these definitions for new quality checks")
                                .when(can_activate, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.approve_taxonomy_draft(cx)
                                        }),
                                    )
                                }),
                        )
                    }),
            );
        }
        view = view
            .child(setting_row(
                "Changes",
                "Renames, merges, splits, and retirements keep their history".into(),
                stack_rows,
            ))
            .child(setting_row(
                "Unknown outcomes",
                "Stay visible instead of being forced into a category".into(),
                stack_rows,
            ))
            .child(notice(
                "You approve changes",
                "Codex can propose definitions, but cannot activate them.".into(),
                Theme::CYAN,
            ));
        view
    }

    fn learned_assessment_section(
        &self,
        stack_rows: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let health = &self.assessment_health;
        let local_model = self
            .service
            .as_ref()
            .and_then(|service| service.local_task_completion_model());
        let local_status = local_model.as_ref().map_or_else(
            || {
                if self
                    .runtime_config
                    .assessments
                    .local_model_artifact_dir
                    .is_some()
                {
                    "Installed files could not be verified".into()
                } else {
                    "Not installed".into()
                }
            },
            |_| "Ready on this Mac".into(),
        );
        let installed_model_id = local_model.as_ref().map(|model| model.model_id.as_str());
        let update_available = self
            .model_catalog
            .as_ref()
            .is_some_and(|catalog| Some(catalog.model_id.as_str()) != installed_model_id);
        let management_status = match &self.model_management_state {
            ModelManagementState::Checking => "Checking for an available model…".into(),
            ModelManagementState::Ready => self.model_catalog.as_ref().map_or_else(
                || "No downloadable release was found.".into(),
                |catalog| {
                    if update_available {
                        format!("{} is available", catalog.release_version)
                    } else {
                        "Up to date".into()
                    }
                },
            ),
            ModelManagementState::Installing => "Downloading and verifying…".into(),
            ModelManagementState::Failed(error) => format!("Could not check: {error}"),
        };
        let can_install = self.model_catalog.is_some()
            && !self.saving
            && !matches!(
                self.model_management_state,
                ModelManagementState::Checking | ModelManagementState::Installing
            )
            && (update_available || local_model.is_none());
        let can_refresh = !matches!(
            self.model_management_state,
            ModelManagementState::Checking | ModelManagementState::Installing
        );
        let mut view = section(
            "Local reviews",
            "Check whether each run achieved what the user asked, without uploading trace evidence.",
        )
        .child(switch_row(
            "Review new runs",
            "Run checks in the background on this Mac",
            if self.draft.assessments.enabled {
                "On"
            } else {
                "Off"
            },
            self.draft.assessments.enabled,
            "toggle-assessment-worker",
            cx.listener(|this, _, _, cx| this.toggle_assessments(cx)),
            stack_rows,
        ))
        .child(setting_row("Model status", local_status, stack_rows))
        .child(setting_row("Available model", management_status, stack_rows))
        .child(setting_row(
            "Background work",
            if self.runtime_config.assessments.enabled {
                format!("{} running · {} waiting", health.running, health.pending)
            } else {
                "Paused".into()
            },
            stack_rows,
        ))
        .child(setting_row(
            "Reviews",
            format!(
                "{} finished · {} could not decide · {} failed · {} cancelled",
                health.terminal,
                health.abstained,
                health.failed,
                health.cancelled
            ),
            stack_rows,
        ))
        .child(setting_row(
            "Waiting for input",
            format!(
                "{} missing context · {} privacy-blocked · {} unavailable · {} not applicable",
                health.context_unresolved,
                health.privacy_blocked,
                health.provider_unavailable,
                health.not_applicable
            ),
            stack_rows,
        ))
        .child(notice(
            if local_model.is_some() {
                "Development model"
            } else if matches!(
                self.model_management_state,
                ModelManagementState::Installing
            ) {
                "Installing local model"
            } else {
                "Install once, then keep traces local"
            },
            if local_model.is_some() {
                "Ready for local testing. This preview is not release-certified yet.".into()
            } else if matches!(
                self.model_management_state,
                ModelManagementState::Installing
            ) {
                "Perseval verifies every downloaded file before it can be used.".into()
            } else {
                "Perseval downloads a hash-pinned development model, verifies every file locally, and restarts. It will not fall back to a hosted model.".into()
            },
            if local_model.is_some() {
                Theme::CYAN
            } else {
                Theme::AMBER
            },
        ));
        view = view.child(
            div()
                .mt_4()
                .flex()
                .flex_wrap()
                .gap_2()
                .when(update_available || local_model.is_none(), |actions| {
                    actions.child(action_button(
                        if matches!(
                            self.model_management_state,
                            ModelManagementState::Installing
                        ) {
                            "Installing…"
                        } else if local_model.is_some() {
                            "Update model & restart"
                        } else {
                            "Install model & restart"
                        },
                        "Download, verify, and use the latest local model",
                        can_install,
                        true,
                        cx.listener(|this, _, _, cx| this.install_latest_model(cx)),
                    ))
                })
                .child(action_button(
                    if matches!(self.model_management_state, ModelManagementState::Checking) {
                        "Checking…"
                    } else {
                        "Check for updates"
                    },
                    "Check the public model catalog",
                    can_refresh,
                    false,
                    cx.listener(|this, _, _, cx| this.refresh_model_catalog(cx)),
                )),
        );
        view
    }

    fn storage_section(&self, stack_rows: bool) -> impl IntoElement {
        section("Current storage", "Local data for the current workspace.")
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
                section.child(notice("Workspace size unavailable", error, Theme::AMBER))
            })
            .child(setting_row(
                "Retention",
                "Keep all data; automatic cleanup is not available yet".into(),
                stack_rows,
            ))
    }

    fn runtime_section(&self, stack_rows: bool, cx: &mut Context<Self>) -> impl IntoElement {
        section(
            "Workspace and collection",
            "Saving restarts Perseval so these changes take effect.",
        )
        .child(editable_row(
            "Workspace ID",
            "Stable identity stored with projects and traces",
            self.workspace_id.clone(),
            stack_rows,
        ))
        .child(editable_row(
            "Workspace directory",
            "Where Perseval stores local data",
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
            "Receive local traces over HTTP",
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
            "Control how much payload data a user can reveal.",
        )
        .child(editable_row(
            "Inline attribute limit (KiB)",
            "Store larger or sensitive attributes separately",
            self.inline_attribute_kib.clone(),
            stack_rows,
        ))
        .child(editable_row(
            "Default payload preview (KiB)",
            "Maximum size of the first preview",
            self.default_preview_kib.clone(),
            stack_rows,
        ))
        .child(switch_row(
            "Larger local reveal",
            "Allow a second, larger local preview",
            if self.draft.blobs.allow_larger_local_reveal { "Allowed" } else { "Blocked" },
            self.draft.blobs.allow_larger_local_reveal,
            "toggle-larger-reveal",
            cx.listener(|this, _, _, cx| this.toggle_larger_reveal(cx)),
            stack_rows,
        ))
        .child(editable_row(
            "Maximum local reveal (MiB)",
            "Maximum size of the larger preview",
            self.maximum_reveal_mib.clone(),
            stack_rows,
        ))
        .child(setting_row(
            "Known sensitive payloads",
            "Stored separately and hidden until you explicitly reveal them".into(),
            stack_rows,
        ))
        .child(notice(
            "Hidden is not redacted",
            "Perseval may still store secrets or personal data. Keep sensitive data out of telemetry at the source.".into(),
            Theme::RED,
        ))
    }

    fn analysis_section(&self, stack_rows: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let provider_status = if !self.openai_health.enabled {
            "OpenAI options are off. Saving changes restarts Perseval.".to_string()
        } else if !self.openai_health.configured {
            "OPENAI_API_KEY is missing. Local analysis stays available.".to_string()
        } else if self.openai_health.running_jobs > 0 {
            format!(
                "{} hosted analysis job(s) running · {} completed",
                self.openai_health.running_jobs, self.openai_health.successful_jobs
            )
        } else if self.openai_health.degraded {
            self.openai_health
                .last_error
                .clone()
                .unwrap_or_else(|| "OpenAI is unavailable. Local analysis stays available.".into())
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
            "Choose local or hosted analysis.",
        )
        .child(switch_row(
            "Feature similarity",
            "Group failures using local trace features",
            if self.draft.analysis.feature_similarity_enabled { "On" } else { "Off" },
            self.draft.analysis.feature_similarity_enabled,
            "toggle-feature-similarity",
            cx.listener(|this, _, _, cx| this.toggle_feature_similarity(cx)),
            stack_rows,
        ))
        .child(notice(
            "Runs locally",
            "No data leaves your Mac unless an OpenAI option below is on.".into(),
            Theme::CYAN,
        ))
        .child(switch_row(
            "OpenAI augmentations",
            "Show hosted options. Nothing is sent until one is enabled",
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
                    "Improve similarity groups using safe failure summaries",
                    if self.draft.analysis.openai.embeddings_enabled { "On" } else { "Off" },
                    self.draft.analysis.openai.embeddings_enabled,
                    "toggle-openai-embeddings",
                    cx.listener(|this, _, _, cx| this.toggle_openai_embeddings(cx)),
                    stack_rows,
                ))
                .child(switch_row(
                    "OpenAI cluster labels",
                    "Name similarity groups using safe failure summaries",
                    if self.draft.analysis.openai.cluster_labels_enabled { "On" } else { "Off" },
                    self.draft.analysis.openai.cluster_labels_enabled,
                    "toggle-openai-cluster-labels",
                    cx.listener(|this, _, _, cx| this.toggle_openai_cluster_labels(cx)),
                    stack_rows,
                ))
                .child(switch_row(
                    "OpenAI semantic judge",
                    "Review behavior facts without raw payloads",
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
                    "OPENAI_API_KEY is never saved. Hosted features receive safe summaries only; prompts, reasoning, code, tool data, inputs, and outputs stay local.".into(),
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
                                "Restart required"
                            } else {
                                "No unsaved changes"
                            }),
                    )
                    .child(div().mt_1().text_xs().text_color(Theme::MUTED).child(
                        validation_error.unwrap_or_else(|| {
                            if changed {
                                "Save to restart Perseval with these changes.".into()
                            } else if self.selected_category == SettingsCategory::Appearance {
                                "Appearance changes apply immediately.".into()
                            } else {
                                "Edit a setting to enable Save and restart.".into()
                            }
                        }),
                    )),
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
                        if self.saving {
                            "Saving…"
                        } else {
                            "Save and restart"
                        },
                        "Save settings and restart Perseval",
                        save_enabled,
                        true,
                        cx.listener(|this, _, _, cx| this.save_and_restart(cx)),
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
                                    .child("Collection, privacy, AI, and accessibility."),
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
                                    "Perseval {} · Workspace: {}",
                                    env!("CARGO_PKG_VERSION"),
                                    self.runtime_config.workspace_id
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

fn validate_local_model_folder(artifact_dir: Option<&Path>) -> Result<(), String> {
    let Some(artifact_dir) = artifact_dir else {
        return Ok(());
    };
    inspect_managed_model(artifact_dir)
        .map(|_| ())
        .map_err(|error| format!("Local model verification failed: {error}"))
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

fn short_identity(value: &str) -> &str {
    value.get(..value.len().min(18)).unwrap_or(value)
}

fn taxonomy_dimension_label(
    dimension: perseval_service::analysis::TaxonomyDimensionV1,
) -> &'static str {
    use perseval_service::analysis::TaxonomyDimensionV1::*;
    match dimension {
        Task => "Task",
        Capability => "Capability",
        SuccessCriterion => "Success criterion",
        NonGoal => "Non-goal",
        Policy => "Policy",
        Risk => "Risk",
        Escalation => "Escalation",
        FailureMode => "Issue type",
        RootCause => "Root cause",
        Severity => "Severity",
    }
}

fn draft_context_text(
    draft: &perseval_service::AgentContextDraftV1,
    pointer: &str,
) -> Option<String> {
    let value = draft.proposed_context.pointer(pointer)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| (!value.is_null()).then(|| value.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextReviewItem {
    label: String,
    value: String,
    detail: String,
}

fn context_review_items(context: &serde_json::Value) -> Vec<ContextReviewItem> {
    let mut items = Vec::new();
    collect_context_review_items(context, &mut Vec::new(), &mut items);
    items
}

fn collect_context_review_items(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    items: &mut Vec<ContextReviewItem>,
) {
    match value {
        serde_json::Value::Object(object) if object.get("field_id").is_some() => {
            let label = context_review_label(path, object);
            let visible_value = context_review_value(object);
            let mut details = Vec::new();
            for (label, key) in [
                ("Source", "source_locator"),
                ("Provenance", "provenance"),
                ("Review", "review_state"),
                ("Sensitivity", "sensitivity"),
                ("Field ID", "field_id"),
            ] {
                if let Some(value) = object.get(key).and_then(serde_json::Value::as_str) {
                    details.push(format!("{label}: {value}"));
                }
            }
            if let Some(confidence) = object
                .get("inference_confidence")
                .and_then(serde_json::Value::as_f64)
            {
                details.push(format!(
                    "Inference confidence: {confidence:.0}%",
                    confidence = confidence * 100.0
                ));
            }
            items.push(ContextReviewItem {
                label,
                value: visible_value,
                detail: details.join(" · "),
            });
        }
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                if matches!(key.as_str(), "schema_version" | "agent_id") {
                    continue;
                }
                path.push(key.clone());
                collect_context_review_items(child, path, items);
                path.pop();
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_context_review_items(child, path, items);
            }
        }
        _ => {}
    }
}

fn context_review_label(
    path: &[String],
    object: &serde_json::Map<String, serde_json::Value>,
) -> String {
    if object.get("capability_id").is_some() {
        return "Capability".into();
    }
    if object.get("criterion_id").is_some() {
        return "Success criterion".into();
    }
    let key = path.last().map(String::as_str).unwrap_or("context");
    match key {
        "application_name" => "Application name".into(),
        "owner" => "Owner".into(),
        "environment" => "Environment".into(),
        "risk_tier" => "Risk tier".into(),
        "purpose" => "Purpose".into(),
        "supported_tasks" => "Task".into(),
        "explicit_non_goals" => "Non-goal".into(),
        "acceptable_partial_completion" => "Acceptable partial completion".into(),
        "refusal_requirements" => "Refusal requirement".into(),
        "escalation_requirements" => "Escalation requirement".into(),
        "build_version_selectors" => "Build selector".into(),
        "entry_points" => "Entry point".into(),
        "user_personas" => "User persona".into(),
        "supported_domains" => "Supported domain".into(),
        "languages" => "Language".into(),
        "routers" => "Router".into(),
        "sub_agents" => "Sub-agent".into(),
        "memory" => "Memory".into(),
        "retrieval_data_sources" => "Retrieval data source".into(),
        "human_handoffs" => "Human handoff".into(),
        "external_services" => "External service".into(),
        "expected_causal_topology" => "Expected trace topology".into(),
        "data_classifications" => "Data classification".into(),
        "retention_rules" => "Retention rule".into(),
        "redaction_rules" => "Redaction rule".into(),
        "external_provider_permissions" => "Provider permission".into(),
        "compliance_constraints" => "Compliance constraint".into(),
        "learned_feature_content" => "Learned-feature content rule".into(),
        "reusable_rubrics" => "Reusable rubric".into(),
        "safe_positive_examples" => "Safe positive example".into(),
        "safe_negative_examples" => "Safe negative example".into(),
        "known_limitations" => "Known limitation".into(),
        "required_evidence_types" => "Required evidence type".into(),
        other => other.replace('_', " "),
    }
}

fn context_review_value(object: &serde_json::Map<String, serde_json::Value>) -> String {
    if let Some(value) = object.get("value") {
        return display_review_json(value);
    }
    if let Some(description) = object
        .get("description")
        .and_then(serde_json::Value::as_str)
    {
        let importance = object
            .get("importance")
            .and_then(serde_json::Value::as_str)
            .map(|importance| format!(" · {importance}"))
            .unwrap_or_default();
        return format!("{description}{importance}");
    }
    if let Some(name) = object.get("name").and_then(serde_json::Value::as_str) {
        let mut parts = vec![name.to_string()];
        for (label, key) in [
            ("Kind", "kind"),
            ("Effect", "effect"),
            ("Idempotency", "idempotency"),
        ] {
            if let Some(value) = object.get(key).and_then(serde_json::Value::as_str) {
                parts.push(format!("{label}: {value}"));
            }
        }
        if object
            .get("requires_approval")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            parts.push("Human approval required".into());
        }
        for (label, key) in [
            ("Permissions", "permissions"),
            ("Allowed operations", "allowed_operations"),
            ("Prohibited operations", "prohibited_operations"),
            ("Required preconditions", "required_preconditions"),
            ("Budgets", "budgets"),
        ] {
            if let Some(value) = object.get(key).filter(|value| match value {
                serde_json::Value::Array(values) => !values.is_empty(),
                serde_json::Value::Object(values) => !values.is_empty(),
                _ => !value.is_null(),
            }) {
                parts.push(format!("{label}: {}", display_review_json(value)));
            }
        }
        return parts.join(" · ");
    }
    display_review_json(&serde_json::Value::Object(object.clone()))
}

fn display_review_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(display_review_json)
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null => "Not specified".into(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
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

    #[test]
    fn local_model_folder_must_pass_full_runtime_verification() {
        assert!(validate_local_model_folder(None).is_ok());
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_local_model_folder(Some(directory.path())).is_err());

        std::fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "model_file": {"path": "model.onnx"},
                "tokenizer_file": {"path": "tokenizer.json"}
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(directory.path().join("model.onnx"), []).unwrap();
        assert!(validate_local_model_folder(Some(directory.path())).is_err());
        std::fs::write(directory.path().join("tokenizer.json"), []).unwrap();
        assert!(validate_local_model_folder(Some(directory.path())).is_err());
    }

    #[test]
    fn immutable_context_review_keeps_complete_values_and_provenance() {
        let long_task = "Inspect the complete customer request, verify the referenced order, explain every eligibility decision, and request human approval before any refund is issued.";
        let context = serde_json::json!({
            "intent": {
                "supported_tasks": [{
                    "field_id": "task.inspect-and-return",
                    "provenance": "config_import",
                    "source_snapshot_id": "sha256:source",
                    "source_locator": "README.md#tasks",
                    "captured_at": "2026-07-19T00:00:00Z",
                    "review_state": "approved",
                    "sensitivity": "public",
                    "inference_confidence": 0.91,
                    "value": long_task
                }]
            }
        });
        let items = context_review_items(&context);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Task");
        assert_eq!(items[0].value, long_task);
        assert!(items[0].detail.contains("README.md#tasks"));
        assert!(
            items[0]
                .detail
                .contains("Field ID: task.inspect-and-return")
        );
        assert!(items[0].detail.contains("Inference confidence: 91%"));
    }

    #[gpui::test]
    fn editable_settings_validate_and_update_the_draft(cx: &mut TestAppContext) {
        let screen = cx.new(|cx| {
            let mut config = PersevalConfigV1::default();
            config.otlp.enabled = false;
            SettingsScreen::new(
                None,
                config,
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                AssessmentRuntimeHealthV1::default(),
                None,
                AgentContextGovernanceSummaryV1::default(),
                TaxonomyGovernanceSummaryV1::default(),
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
                None,
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                AssessmentRuntimeHealthV1::default(),
                None,
                AgentContextGovernanceSummaryV1::default(),
                TaxonomyGovernanceSummaryV1::default(),
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
                None,
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                AssessmentRuntimeHealthV1::default(),
                None,
                AgentContextGovernanceSummaryV1::default(),
                TaxonomyGovernanceSummaryV1::default(),
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
                None,
                PersevalConfigV1::default(),
                AppearancePreferencesV1::default(),
                OpenAiProviderHealthV1::default(),
                AssessmentRuntimeHealthV1::default(),
                None,
                AgentContextGovernanceSummaryV1::default(),
                TaxonomyGovernanceSummaryV1::default(),
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

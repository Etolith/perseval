use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use perseval_store::{
    AgentContextDraftV1, AgentContextGovernanceSummaryV1, AssessmentBackfillPreviewV1,
    AssessmentJobExportV1, AssessmentJobV1, AssessmentRecordV1, AssessmentRuntimeHealthV1,
    AssessmentSamplingPolicyV1, ContextBackfillPreviewV1, ContextBackfillResultV1,
    ContextBindingRecordV1, ContextBindingRuleSetV1, ProjectAssessmentPolicyV1, ReviewAuthorityV1,
    TASK_COMPLETION_RELEASE_CONFIG_SCHEMA_VERSION, TaskCompletionQualityCheckV1,
    TaskCompletionReleaseConfigV1, TaxonomyGovernanceSummaryV1,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use traces_to_evals::{
    AGENT_CONTEXT_RELEASE_SCHEMA_VERSION, AGENT_TAXONOMY_RELEASE_SCHEMA_VERSION,
    AgentArchitectureContextV1, AgentCapabilityV1, AgentContextReleaseV1, AgentEvaluationContextV1,
    AgentIdentityContextV1, AgentIntentContextV1, AgentPolicyContextV1, AgentTaxonomyReleaseV1,
    CapabilityEffectV1, CapabilityKindV1, ContextFieldMetadataV1, ContextFieldProvenanceV1,
    ContextFieldV1, ContextProjectionClassV1, ContextProjectionV1, ContextReviewStateV1,
    ContextSensitivityV1, EvaluationImplementationV1, EvaluationInputBoundsV1,
    EvaluationTargetKind, EvaluatorReleaseSpecV1, IdempotencyClassV1, LearnedTaskKind,
    SuccessCriterionImportanceV1, SuccessCriterionV1, TASK_COMPLETION_EVIDENCE_SYSTEM_PROMPT_V2,
    TaskCompletionContentPolicyV1, TaskCompletionProjectorV1, TaxonomyDimensionV1,
    TaxonomyLineageOperationV1, TaxonomyNodeStateV1, TaxonomyNodeV1,
    task_completion_judgment_response_schema,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletionQualityCheckDraftV1 {
    pub name: String,
    pub review_criteria: String,
    pub requested_model: String,
    pub context_release_id: String,
    pub applicable_taxonomy_node_ids: BTreeSet<String>,
    pub content_policy: TaskCompletionContentPolicyV1,
    pub estimated_output_tokens_low: u64,
    pub estimated_output_tokens_high: u64,
    pub input_cost_micros_per_million_tokens: u64,
    pub output_cost_micros_per_million_tokens: u64,
    pub pricing_version: String,
}

impl LiveTraceService {
    pub fn publish_task_completion_quality_check(
        &self,
        project_id: &str,
        draft: &TaskCompletionQualityCheckDraftV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        if draft.name.trim().is_empty()
            || draft.review_criteria.trim().is_empty()
            || draft.requested_model.trim().is_empty()
            || draft.pricing_version.trim().is_empty()
        {
            return Err(LiveServiceError::InvalidInput(
                "quality check name, review criteria, model, and pricing version are required"
                    .into(),
            ));
        }
        if draft.estimated_output_tokens_low == 0
            || draft.estimated_output_tokens_high < draft.estimated_output_tokens_low
        {
            return Err(LiveServiceError::InvalidInput(
                "estimated output token range is invalid".into(),
            ));
        }
        let context = self
            .store
            .agent_context_release(project_id, &draft.context_release_id)?
            .ok_or_else(|| {
                LiveServiceError::InvalidInput(
                    "the selected immutable agent specification is unavailable".into(),
                )
            })?;
        let projection_class = match draft.content_policy {
            TaskCompletionContentPolicyV1::StructuredOnly => {
                ContextProjectionClassV1::StructuralOnly
            }
            TaskCompletionContentPolicyV1::PreRedactedSummaries => {
                ContextProjectionClassV1::HostedPreRedacted
            }
        };
        let included_field_ids = task_completion_context_field_ids(&context, projection_class);
        let applicable_taxonomy_release_id = if draft.applicable_taxonomy_node_ids.is_empty() {
            None
        } else {
            self.store
                .taxonomy_governance_summary(project_id)?
                .latest_release_id
                .ok_or_else(|| {
                    LiveServiceError::InvalidInput(
                        "stable taxonomy applicability requires an active immutable taxonomy release"
                            .into(),
                    )
                })?
                .into()
        };
        let context_projection = ContextProjectionV1 {
            context_release_id: draft.context_release_id.clone(),
            projection_class,
            projector_version: "perseval-context-projector-v1".into(),
            redaction_version: "perseval-redaction-v1".into(),
            included_field_ids,
        };
        let projector = TaskCompletionProjectorV1 {
            content_policy: draft.content_policy,
            max_tool_observations: 256,
            max_summary_bytes: 4_096,
        };
        let evaluator = EvaluatorReleaseSpecV1 {
            schema_version: traces_to_evals::EVALUATOR_RELEASE_SCHEMA_VERSION.into(),
            name: draft.name.trim().into(),
            task_kind: LearnedTaskKind::TaskCompletion,
            target_kind: EvaluationTargetKind::TraceRevision,
            implementation: EvaluationImplementationV1::PromptJudge {
                provider: "openai".into(),
                requested_model: draft.requested_model.trim().into(),
                system_prompt: TASK_COMPLETION_EVIDENCE_SYSTEM_PROMPT_V2.into(),
                rubric: draft.review_criteria.trim().into(),
                response_schema: task_completion_judgment_response_schema(),
                decoding_parameters: BTreeMap::new(),
                parser_version: "task-completion-parser-v1".into(),
                normalizer_version: "task-completion-normalizer-v1".into(),
            },
            projection_release_id: projector
                .release_id()
                .map_err(|error| LiveServiceError::InvalidInput(error.to_string()))?,
            context_projection_release_id: context_projection
                .release_id()
                .map_err(|error| LiveServiceError::InvalidInput(error.to_string()))?,
            applicable_taxonomy_release_id,
            applicable_taxonomy_node_ids: draft.applicable_taxonomy_node_ids.clone(),
            input_bounds: EvaluationInputBoundsV1 {
                max_subjects: 1,
                max_evidence_items: 512,
                max_input_bytes: 256_000,
                max_output_bytes: 16_000,
            },
            evidence_schema_version: "traceeval.evidence.v1".into(),
            abstention_policy: serde_json::json!({
                "unresolved_context": "abstain",
                "ambiguous_context": "abstain",
                "missing_success_criteria": "abstain",
                "truncated_projection": "abstain",
                "invalid_provider_output": "abstain"
            }),
            code_artifact_hash: content_hash("perseval-task-completion-quality-check-v1"),
        };
        let evaluator_release_id = evaluator
            .release_id()
            .map_err(|error| LiveServiceError::InvalidInput(error.to_string()))?;
        let config = TaskCompletionReleaseConfigV1 {
            schema_version: TASK_COMPLETION_RELEASE_CONFIG_SCHEMA_VERSION.into(),
            project_id: project_id.into(),
            evaluator_release_id,
            context_release_id: draft.context_release_id.clone(),
            context_projection,
            projector,
            requested_model: draft.requested_model.trim().into(),
            estimated_output_tokens_low: draft.estimated_output_tokens_low,
            estimated_output_tokens_high: draft.estimated_output_tokens_high,
            input_cost_micros_per_million_tokens: draft.input_cost_micros_per_million_tokens,
            output_cost_micros_per_million_tokens: draft.output_cost_micros_per_million_tokens,
            pricing_version: draft.pricing_version.trim().into(),
            activated_by: activated_by.into(),
            activated_at_unix_ms: unix_ms_now(),
        };
        self.activate_task_completion_evaluator_release(
            project_id,
            &evaluator,
            &config,
            activated_by,
            authority,
        )
    }
    /// Prepare a bounded, local-only specification draft from a directory the
    /// user explicitly selected. The scanner reads documentation and package
    /// manifests only; it skips fixtures, benchmark outputs, credentials, and
    /// hidden files. No content leaves the machine.
    pub fn prepare_agent_context_from_repository(
        &self,
        project_id: &str,
        repository_path: &Path,
        created_by: &str,
    ) -> Result<AgentContextDraftV1, LiveServiceError> {
        let repository_path = repository_path
            .canonicalize()
            .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        if !repository_path.is_dir() {
            return Err(LiveServiceError::Writer(
                "agent specification source must be a directory".into(),
            ));
        }
        let repository_label = repository_path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("selected-repository")
            .to_owned();
        let sources = collect_context_sources(&repository_path)
            .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        if sources.is_empty() {
            return Err(LiveServiceError::Writer(
                "no README, AGENTS, package manifest, or bounded docs were found".into(),
            ));
        }
        let mut content_hasher = Sha256::new();
        let mut files = Vec::with_capacity(sources.len());
        for source in &sources {
            content_hasher.update(source.relative_path.as_bytes());
            content_hasher.update([0]);
            content_hasher.update(&source.bytes);
            files.push(serde_json::json!({
                "path": source.relative_path,
                "sha256": format!("sha256:{}", hex::encode(Sha256::digest(&source.bytes))),
                "bytes": source.bytes.len(),
            }));
        }
        let manifest = serde_json::json!({
            "repository": repository_label.clone(),
            "files": files,
            "local_inputs": [
                {
                    "locator": "perseval.reviewer_ref",
                    "sha256": format!("sha256:{}", hex::encode(Sha256::digest(created_by.as_bytes()))),
                    "classification": "internal",
                },
                {
                    "locator": "perseval.project_scope",
                    "sha256": format!("sha256:{}", hex::encode(Sha256::digest(project_id.as_bytes()))),
                    "classification": "internal",
                }
            ],
            "selection_policy": "perseval.agent-context-source-selection.v1",
            "excluded": ["hidden files", "credentials", "fixtures", "benchmarks", "generated outputs"],
        });
        let content_hash = format!("sha256:{}", hex::encode(content_hasher.finalize()));
        let source_snapshot_id = self.store.record_context_source_snapshot(
            project_id,
            "approved_repository",
            &repository_label,
            &content_hash,
            "internal",
            &manifest,
        )?;
        let application_name = infer_application_name(&repository_path, &sources);
        let (purpose, purpose_source) = infer_purpose(&sources).unwrap_or_else(|| {
            (
                format!("Operate and evaluate {application_name}"),
                "directory-name fallback".into(),
            )
        });
        let mut tasks = infer_section_items(&sources, &["task", "use case", "workflow", "feature"]);
        if tasks.is_empty() {
            tasks.push((
                format!("Fulfill the declared purpose: {purpose}"),
                purpose_source.clone(),
            ));
        }
        let capabilities = infer_section_items(
            &sources,
            &["capabilit", "tool", "integration", "external service"],
        );
        let non_goals = infer_section_items(
            &sources,
            &["non-goal", "out of scope", "prohibited", "must not"],
        );
        let success_criteria = infer_section_items(
            &sources,
            &[
                "success criteria",
                "definition of done",
                "success",
                "requirements",
            ],
        );
        let escalation_requirements =
            infer_section_items(&sources, &["escalation", "human handoff", "approval"]);
        let captured_at = chrono::Utc::now().to_rfc3339();
        let field = |field_id: &str,
                     value: Value,
                     source_locator: String,
                     confidence: f64,
                     provenance: ContextFieldProvenanceV1|
         -> ContextFieldV1 {
            ContextFieldV1 {
                metadata: ContextFieldMetadataV1 {
                    field_id: field_id.into(),
                    provenance,
                    source_snapshot_id: source_snapshot_id.clone(),
                    source_locator: Some(source_locator),
                    captured_at: captured_at.clone(),
                    fresh_until: None,
                    review_state: ContextReviewStateV1::Unreviewed,
                    sensitivity: ContextSensitivityV1::Internal,
                    inference_confidence: Some(confidence),
                },
                value,
            }
        };
        let agent_id = format!("agent:{}", normalize_identifier(&application_name));
        let release = AgentContextReleaseV1 {
            schema_version: AGENT_CONTEXT_RELEASE_SCHEMA_VERSION.into(),
            agent_id: agent_id.clone(),
            identity: AgentIdentityContextV1 {
                application_name: field(
                    "identity.application_name",
                    serde_json::json!(application_name),
                    "package manifest or README heading".into(),
                    0.9,
                    ContextFieldProvenanceV1::SystemInferred,
                ),
                owner: field(
                    "identity.owner",
                    serde_json::json!(created_by),
                    "perseval.reviewer_ref".into(),
                    1.0,
                    ContextFieldProvenanceV1::ConfigImport,
                ),
                environment: field(
                    "identity.environment",
                    serde_json::json!("project-scoped traces"),
                    "Perseval project scope".into(),
                    1.0,
                    ContextFieldProvenanceV1::ConfigImport,
                ),
                build_version_selectors: Vec::new(),
                entry_points: Vec::new(),
                user_personas: Vec::new(),
                supported_domains: Vec::new(),
                languages: Vec::new(),
                risk_tier: field(
                    "identity.risk_tier",
                    serde_json::json!("review_required"),
                    "conservative local default".into(),
                    0.5,
                    ContextFieldProvenanceV1::SystemInferred,
                ),
            },
            intent: AgentIntentContextV1 {
                purpose: field(
                    "intent.purpose",
                    serde_json::json!(purpose),
                    purpose_source,
                    0.75,
                    ContextFieldProvenanceV1::SystemInferred,
                ),
                supported_tasks: tasks
                    .into_iter()
                    .take(12)
                    .enumerate()
                    .map(|(index, (task, source))| {
                        field(
                            &format!("intent.supported_task.{index}"),
                            serde_json::json!(task),
                            source,
                            0.65,
                            ContextFieldProvenanceV1::SystemInferred,
                        )
                    })
                    .collect(),
                explicit_non_goals: non_goals
                    .into_iter()
                    .take(12)
                    .enumerate()
                    .map(|(index, (value, source))| {
                        field(
                            &format!("intent.non_goal.{index}"),
                            serde_json::json!(value),
                            source,
                            0.65,
                            ContextFieldProvenanceV1::SystemInferred,
                        )
                    })
                    .collect(),
                success_criteria: success_criteria
                    .into_iter()
                    .take(12)
                    .enumerate()
                    .map(|(index, (description, source))| SuccessCriterionV1 {
                        metadata: field(
                            &format!("intent.success_criterion.{index}"),
                            serde_json::json!(description),
                            source,
                            0.6,
                            ContextFieldProvenanceV1::SystemInferred,
                        )
                        .metadata,
                        criterion_id: format!("criterion:{}", normalize_identifier(&description)),
                        description,
                        importance: SuccessCriterionImportanceV1::Must,
                        required_evidence_kinds: BTreeSet::new(),
                        business_impact_weight: None,
                    })
                    .collect(),
                acceptable_partial_completion: None,
                refusal_requirements: Vec::new(),
                escalation_requirements: escalation_requirements
                    .into_iter()
                    .take(12)
                    .enumerate()
                    .map(|(index, (value, source))| {
                        field(
                            &format!("intent.escalation.{index}"),
                            serde_json::json!(value),
                            source,
                            0.6,
                            ContextFieldProvenanceV1::SystemInferred,
                        )
                    })
                    .collect(),
            },
            capabilities: capabilities
                .into_iter()
                .take(24)
                .enumerate()
                .map(|(index, (name, source))| AgentCapabilityV1 {
                    metadata: field(
                        &format!("capability.{index}"),
                        serde_json::json!(name),
                        source,
                        0.6,
                        ContextFieldProvenanceV1::SystemInferred,
                    )
                    .metadata,
                    capability_id: format!("capability:{}", normalize_identifier(&name)),
                    requires_approval: name.to_lowercase().contains("approval"),
                    name,
                    kind: CapabilityKindV1::InternalOperation,
                    effect: CapabilityEffectV1::Unknown,
                    idempotency: IdempotencyClassV1::Unknown,
                    argument_schema_digest: None,
                    result_schema_digest: None,
                    permissions: BTreeSet::new(),
                    allowed_operations: BTreeSet::new(),
                    prohibited_operations: BTreeSet::new(),
                    required_preconditions: BTreeSet::new(),
                    budgets: BTreeMap::new(),
                })
                .collect(),
            architecture: AgentArchitectureContextV1::default(),
            policy: AgentPolicyContextV1::default(),
            evaluation_context: AgentEvaluationContextV1::default(),
        };
        Ok(self.store.create_agent_context_draft(
            project_id,
            &agent_id,
            &source_snapshot_id,
            serde_json::to_value(release)
                .map_err(|error| LiveServiceError::Writer(error.to_string()))?,
            Vec::new(),
            Vec::new(),
            created_by,
            ReviewAuthorityV1::Importer,
        )?)
    }

    pub fn agent_context_governance_summary(
        &self,
        project_id: &str,
    ) -> Result<AgentContextGovernanceSummaryV1, LiveServiceError> {
        Ok(self.store.agent_context_governance_summary(project_id)?)
    }

    pub fn taxonomy_governance_summary(
        &self,
        project_id: &str,
    ) -> Result<TaxonomyGovernanceSummaryV1, LiveServiceError> {
        Ok(self.store.taxonomy_governance_summary(project_id)?)
    }

    /// Prepare a human-reviewable task/capability definition release from the
    /// latest activated, sourced agent specification. This is additive and
    /// draft-only; it cannot activate taxonomy meaning.
    pub fn prepare_taxonomy_from_agent_context(
        &self,
        project_id: &str,
        created_by: &str,
    ) -> Result<String, LiveServiceError> {
        let (context_release_id, context) = self
            .store
            .latest_agent_context_release(project_id)?
            .ok_or_else(|| {
                LiveServiceError::Writer(
                    "activate an agent specification before preparing tasks and issue types".into(),
                )
            })?;
        let active = self.store.active_taxonomy_release(project_id)?;
        let (base_release_id, previous) = match active {
            Some((release_id, release)) => (Some(release_id), Some(release)),
            None => (None, None),
        };
        let mut proposed_nodes = previous
            .as_ref()
            .map(|release| {
                release
                    .nodes
                    .iter()
                    .cloned()
                    .map(|node| (node.node_id.clone(), node))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut lineage = Vec::new();
        let mut source_fields = Vec::new();
        let mut add_definition = |field_id: &str,
                                  dimension: TaxonomyDimensionV1,
                                  name: String,
                                  description: String,
                                  provenance: String,
                                  sensitivity: String| {
            if name.trim().is_empty() {
                return;
            }
            let node_id = taxonomy_node_id(project_id, dimension, field_id);
            if let Some(prior) = proposed_nodes.get(&node_id)
                && prior.name != name
            {
                lineage.push(TaxonomyLineageOperationV1::Rename {
                    node_id: node_id.clone(),
                    previous_name: prior.name.clone(),
                    new_name: name.clone(),
                });
            } else if !proposed_nodes.contains_key(&node_id) {
                lineage.push(TaxonomyLineageOperationV1::Create {
                    node_id: node_id.clone(),
                });
            }
            source_fields.push(serde_json::json!({
                "field_id": field_id,
                "context_release_id": context_release_id,
                "provenance": provenance,
                "sensitivity": sensitivity,
            }));
            proposed_nodes.insert(
                node_id.clone(),
                TaxonomyNodeV1 {
                    node_id,
                    dimension,
                    name,
                    description,
                    aliases: BTreeSet::new(),
                    parent_ids: BTreeSet::new(),
                    allowed_relation_types: BTreeSet::new(),
                    state: TaxonomyNodeStateV1::Active,
                    provenance,
                    sensitivity,
                    portable_base_term: None,
                },
            );
        };
        for task in &context.intent.supported_tasks {
            if let Some(name) = task.value.as_str() {
                add_definition(
                    &task.metadata.field_id,
                    TaxonomyDimensionV1::Task,
                    name.into(),
                    "Intended task sourced from the activated agent specification".into(),
                    context_provenance_name(task.metadata.provenance).into(),
                    context_sensitivity_name(task.metadata.sensitivity).into(),
                );
            }
        }
        for capability in &context.capabilities {
            add_definition(
                &capability.metadata.field_id,
                TaxonomyDimensionV1::Capability,
                capability.name.clone(),
                format!(
                    "{:?} capability · {:?} effect · approval {}",
                    capability.kind, capability.effect, capability.requires_approval
                ),
                context_provenance_name(capability.metadata.provenance).into(),
                context_sensitivity_name(capability.metadata.sensitivity).into(),
            );
        }
        for criterion in &context.intent.success_criteria {
            add_definition(
                &criterion.metadata.field_id,
                TaxonomyDimensionV1::SuccessCriterion,
                criterion.description.clone(),
                "Success criterion sourced from the activated agent specification".into(),
                context_provenance_name(criterion.metadata.provenance).into(),
                context_sensitivity_name(criterion.metadata.sensitivity).into(),
            );
        }
        for non_goal in &context.intent.explicit_non_goals {
            if let Some(name) = non_goal.value.as_str() {
                add_definition(
                    &non_goal.metadata.field_id,
                    TaxonomyDimensionV1::NonGoal,
                    name.into(),
                    "Explicit non-goal sourced from the activated agent specification".into(),
                    context_provenance_name(non_goal.metadata.provenance).into(),
                    context_sensitivity_name(non_goal.metadata.sensitivity).into(),
                );
            }
        }
        if proposed_nodes.is_empty() {
            return Err(LiveServiceError::Writer(
                "the active specification has no task, capability, success, or non-goal definitions to review".into(),
            ));
        }
        let proposal = AgentTaxonomyReleaseV1 {
            schema_version: AGENT_TAXONOMY_RELEASE_SCHEMA_VERSION.into(),
            taxonomy_id: format!("taxonomy:{project_id}:agent-quality"),
            previous_release_id: base_release_id.clone(),
            nodes: proposed_nodes.into_values().collect(),
            relations: BTreeSet::new(),
            lineage,
        };
        proposal
            .validate()
            .map_err(|error| LiveServiceError::Writer(error.to_string()))?;
        let source_manifest = serde_json::json!({
            "source_kind": "activated_agent_specification",
            "agent_context_release_id": context_release_id,
            "fields": source_fields,
            "preparation": "perseval.taxonomy-from-agent-context.v1",
        });
        self.create_taxonomy_change_draft(
            project_id,
            base_release_id.as_deref(),
            &serde_json::to_value(&proposal)
                .map_err(|error| LiveServiceError::Writer(error.to_string()))?,
            &source_manifest,
            created_by,
        )
    }

    pub fn approve_taxonomy_change_draft(
        &self,
        draft_id: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .approve_taxonomy_change_draft(draft_id, activated_by, authority)?)
    }

    pub fn record_context_source_snapshot(
        &self,
        project_id: &str,
        source_kind: &str,
        source_locator: &str,
        content_hash: &str,
        sensitivity: &str,
        manifest: &Value,
    ) -> Result<String, LiveServiceError> {
        Ok(self.store.record_context_source_snapshot(
            project_id,
            source_kind,
            source_locator,
            content_hash,
            sensitivity,
            manifest,
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_agent_context_draft(
        &self,
        project_id: &str,
        agent_id: &str,
        source_snapshot_id: &str,
        proposed_context: Value,
        unresolved_field_ids: Vec<String>,
        conflicting_field_ids: Vec<String>,
        created_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<AgentContextDraftV1, LiveServiceError> {
        Ok(self.store.create_agent_context_draft(
            project_id,
            agent_id,
            source_snapshot_id,
            proposed_context,
            unresolved_field_ids,
            conflicting_field_ids,
            created_by,
            authority,
        )?)
    }

    pub fn activate_agent_context_release(
        &self,
        draft_id: &str,
        release: &AgentContextReleaseV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .activate_agent_context_release(draft_id, release, activated_by, authority)?)
    }

    pub fn approve_agent_context_draft(
        &self,
        draft_id: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .approve_agent_context_draft(draft_id, activated_by, authority)?)
    }

    pub fn activate_context_binding_rules(
        &self,
        rules: &ContextBindingRuleSetV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .activate_context_binding_rules(rules, activated_by, authority)?)
    }

    pub fn bind_finalized_trace_context(
        &self,
        project_id: &str,
        logical_trace_id: &str,
        revision: u64,
        binding_rule_release_id: &str,
    ) -> Result<ContextBindingRecordV1, LiveServiceError> {
        Ok(self.store.bind_finalized_trace_context(
            project_id,
            logical_trace_id,
            revision,
            binding_rule_release_id,
            None,
        )?)
    }

    pub fn preview_context_backfill(
        &self,
        project_id: &str,
        context_release_id: &str,
    ) -> Result<ContextBackfillPreviewV1, LiveServiceError> {
        Ok(self
            .store
            .preview_context_backfill(project_id, context_release_id)?)
    }

    pub fn apply_reviewed_default_context_backfill(
        &self,
        project_id: &str,
        context_release_id: &str,
        expected_selection_hash: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<ContextBackfillResultV1, LiveServiceError> {
        Ok(self.store.apply_reviewed_default_context_backfill(
            project_id,
            context_release_id,
            expected_selection_hash,
            activated_by,
            authority,
        )?)
    }

    pub fn create_taxonomy_change_draft(
        &self,
        project_id: &str,
        base_release_id: Option<&str>,
        proposal: &Value,
        source_manifest: &Value,
        created_by: &str,
    ) -> Result<String, LiveServiceError> {
        Ok(self.store.create_taxonomy_change_draft(
            project_id,
            base_release_id,
            proposal,
            source_manifest,
            created_by,
        )?)
    }

    pub fn activate_taxonomy_release(
        &self,
        draft_id: &str,
        release: &AgentTaxonomyReleaseV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .activate_taxonomy_release(draft_id, release, activated_by, authority)?)
    }

    pub fn activate_evaluator_release(
        &self,
        project_id: &str,
        evaluator: &EvaluatorReleaseSpecV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .activate_evaluator_release(project_id, evaluator, activated_by, authority)?)
    }

    pub fn activate_task_completion_evaluator_release(
        &self,
        project_id: &str,
        evaluator: &EvaluatorReleaseSpecV1,
        config: &TaskCompletionReleaseConfigV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self.store.activate_task_completion_evaluator_release(
            project_id,
            evaluator,
            config,
            activated_by,
            authority,
        )?)
    }

    pub fn preview_assessment_backfill(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        exact_revisions: &[(String, u64)],
    ) -> Result<AssessmentBackfillPreviewV1, LiveServiceError> {
        Ok(self.store.preview_assessment_backfill(
            project_id,
            evaluator_release_id,
            exact_revisions,
        )?)
    }

    pub fn set_assessment_sampling_policy(
        &self,
        policy: &AssessmentSamplingPolicyV1,
        authority: ReviewAuthorityV1,
    ) -> Result<(), LiveServiceError> {
        Ok(self
            .store
            .set_assessment_sampling_policy(policy, authority)?)
    }

    pub fn assessment_sampling_policy(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
    ) -> Result<Option<AssessmentSamplingPolicyV1>, LiveServiceError> {
        Ok(self
            .store
            .assessment_sampling_policy(project_id, evaluator_release_id)?)
    }

    pub fn list_task_completion_quality_checks(
        &self,
        project_id: &str,
    ) -> Result<Vec<TaskCompletionQualityCheckV1>, LiveServiceError> {
        Ok(self.store.list_task_completion_quality_checks(project_id)?)
    }

    pub fn list_assessment_jobs(
        &self,
        project_id: &str,
        evaluator_release_id: Option<&str>,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<AssessmentJobV1>, LiveServiceError> {
        Ok(self
            .store
            .list_assessment_jobs(project_id, evaluator_release_id, offset, limit)?)
    }

    pub fn assessment_job(
        &self,
        project_id: &str,
        job_id: &str,
    ) -> Result<Option<AssessmentJobV1>, LiveServiceError> {
        Ok(self.store.assessment_job(project_id, job_id)?)
    }

    pub fn set_project_assessment_policy(
        &self,
        policy: &ProjectAssessmentPolicyV1,
        authority: ReviewAuthorityV1,
    ) -> Result<(), LiveServiceError> {
        Ok(self
            .store
            .set_project_assessment_policy(policy, authority)?)
    }

    pub fn project_assessment_policy(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectAssessmentPolicyV1>, LiveServiceError> {
        Ok(self.store.project_assessment_policy(project_id)?)
    }

    pub fn enqueue_assessment_job(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        exact_revisions: &[(String, u64)],
        idempotency_key: &str,
    ) -> Result<AssessmentJobV1, LiveServiceError> {
        Ok(self.store.enqueue_assessment_job(
            project_id,
            evaluator_release_id,
            exact_revisions,
            idempotency_key,
        )?)
    }

    pub fn enqueue_assessment_job_from_preview(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        exact_revisions: &[(String, u64)],
        expected_selection_hash: &str,
        idempotency_key: &str,
    ) -> Result<AssessmentJobV1, LiveServiceError> {
        Ok(self.store.enqueue_assessment_job_from_preview(
            project_id,
            evaluator_release_id,
            exact_revisions,
            expected_selection_hash,
            idempotency_key,
        )?)
    }

    pub fn cancel_assessment_job(&self, job_id: &str) -> Result<AssessmentJobV1, LiveServiceError> {
        Ok(self.store.cancel_assessment_job(job_id)?)
    }

    pub fn list_trace_assessments(
        &self,
        project_id: &str,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<Vec<AssessmentRecordV1>, LiveServiceError> {
        Ok(self
            .store
            .list_trace_assessments(project_id, logical_trace_id, revision)?)
    }

    pub fn export_assessment_job(
        &self,
        job_id: &str,
    ) -> Result<AssessmentJobExportV1, LiveServiceError> {
        Ok(self.store.export_assessment_job(job_id)?)
    }

    pub fn assessment_runtime_health(&self) -> Result<AssessmentRuntimeHealthV1, LiveServiceError> {
        Ok(self.store.assessment_runtime_health()?)
    }

    pub fn assessment_runtime_health_for_project(
        &self,
        project_id: Option<&str>,
    ) -> Result<AssessmentRuntimeHealthV1, LiveServiceError> {
        Ok(self
            .store
            .assessment_runtime_health_for_project(project_id)?)
    }
}

fn task_completion_context_field_ids(
    context: &AgentContextReleaseV1,
    projection_class: ContextProjectionClassV1,
) -> BTreeSet<String> {
    let mut field_ids = BTreeSet::new();
    let mut include = |metadata: &traces_to_evals::ContextFieldMetadataV1| {
        let safe = match projection_class {
            ContextProjectionClassV1::HostedPreRedacted => matches!(
                metadata.sensitivity,
                ContextSensitivityV1::Public | ContextSensitivityV1::HostedPreRedacted
            ),
            ContextProjectionClassV1::StructuralOnly | ContextProjectionClassV1::LocalContent => {
                !matches!(
                    metadata.sensitivity,
                    ContextSensitivityV1::Secret
                        | ContextSensitivityV1::Credential
                        | ContextSensitivityV1::HiddenLabel
                        | ContextSensitivityV1::ExpectedAnswer
                        | ContextSensitivityV1::Unclassified
                )
            }
        };
        if safe {
            field_ids.insert(metadata.field_id.clone());
        }
    };
    include(&context.intent.purpose.metadata);
    for field in context
        .intent
        .supported_tasks
        .iter()
        .chain(context.intent.explicit_non_goals.iter())
        .chain(context.intent.refusal_requirements.iter())
        .chain(context.intent.escalation_requirements.iter())
        .chain(context.intent.acceptable_partial_completion.iter())
        .chain(context.evaluation_context.required_evidence_types.iter())
    {
        include(&field.metadata);
    }
    for criterion in &context.intent.success_criteria {
        include(&criterion.metadata);
    }
    for capability in &context.capabilities {
        include(&capability.metadata);
    }
    field_ids
}

fn content_hash(value: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(value.as_bytes())))
}

fn unix_ms_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

struct ContextSource {
    relative_path: String,
    bytes: Vec<u8>,
}

fn collect_context_sources(repository: &Path) -> std::io::Result<Vec<ContextSource>> {
    const MAX_FILE_BYTES: u64 = 256 * 1024;
    const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
    let mut candidates = Vec::new();
    for entry in fs::read_dir(repository)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_lowercase();
        if entry.file_type()?.is_file()
            && matches!(
                lower.as_str(),
                "readme.md" | "agents.md" | "cargo.toml" | "package.json" | "pyproject.toml"
            )
        {
            candidates.push(entry.path());
        } else if entry.file_type()?.is_dir() && lower == "docs" {
            for document in fs::read_dir(entry.path())? {
                let document = document?;
                if document.file_type()?.is_file()
                    && document
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                {
                    candidates.push(document.path());
                }
            }
        }
    }
    candidates.sort();
    let mut total = 0_usize;
    let mut sources = Vec::new();
    for path in candidates {
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(repository) || fs::metadata(&canonical)?.len() > MAX_FILE_BYTES {
            continue;
        }
        let relative = canonical
            .strip_prefix(repository)
            .unwrap_or(&canonical)
            .to_string_lossy()
            .to_string();
        let lower = relative.to_lowercase();
        if [
            "secret",
            "credential",
            "benchmark",
            "fixture",
            "expected",
            "gold",
        ]
        .iter()
        .any(|token| lower.contains(token))
        {
            continue;
        }
        let bytes = fs::read(&canonical)?;
        if total.saturating_add(bytes.len()) > MAX_TOTAL_BYTES {
            break;
        }
        total += bytes.len();
        sources.push(ContextSource {
            relative_path: relative,
            bytes,
        });
    }
    Ok(sources)
}

fn infer_application_name(repository: &Path, sources: &[ContextSource]) -> String {
    for source in sources {
        if source.relative_path.eq_ignore_ascii_case("package.json")
            && let Ok(value) = serde_json::from_slice::<Value>(&source.bytes)
            && let Some(name) = value.get("name").and_then(Value::as_str)
            && !name.trim().is_empty()
        {
            return name.trim().to_string();
        }
        if source.relative_path.eq_ignore_ascii_case("cargo.toml")
            && let Some(value) = std::str::from_utf8(&source.bytes)
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(text).ok())
            && let Some(name) = value
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
        {
            return name.to_string();
        }
        if source.relative_path.eq_ignore_ascii_case("readme.md")
            && let Ok(text) = std::str::from_utf8(&source.bytes)
            && let Some(heading) = text.lines().find_map(|line| line.trim().strip_prefix("# "))
            && !heading.trim().is_empty()
        {
            return heading.trim().to_string();
        }
    }
    repository
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("agent")
        .to_string()
}

fn infer_purpose(sources: &[ContextSource]) -> Option<(String, String)> {
    for source in sources {
        if !source.relative_path.eq_ignore_ascii_case("readme.md") {
            continue;
        }
        let text = std::str::from_utf8(&source.bytes).ok()?;
        for paragraph in text.split("\n\n") {
            let normalized = paragraph
                .lines()
                .map(str::trim)
                .filter(|line| {
                    !line.is_empty()
                        && !line.starts_with('#')
                        && !line.starts_with('!')
                        && !line.starts_with('[')
                        && !line.starts_with("<")
                })
                .collect::<Vec<_>>()
                .join(" ");
            if normalized.len() >= 24 {
                return Some((
                    normalized.chars().take(500).collect(),
                    source.relative_path.clone(),
                ));
            }
        }
    }
    None
}

fn infer_section_items(
    sources: &[ContextSource],
    heading_tokens: &[&str],
) -> Vec<(String, String)> {
    let mut items = Vec::new();
    for source in sources {
        let Ok(text) = std::str::from_utf8(&source.bytes) else {
            continue;
        };
        let mut relevant_section = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                let heading = trimmed.trim_start_matches('#').trim().to_lowercase();
                relevant_section = heading_tokens.iter().any(|token| heading.contains(token));
                continue;
            }
            if relevant_section
                && let Some(task) = trimmed
                    .strip_prefix("- ")
                    .or_else(|| trimmed.strip_prefix("* "))
                && task.len() >= 8
                && !task.contains("http://")
                && !task.contains("https://")
            {
                items.push((
                    task.chars().take(300).collect(),
                    source.relative_path.clone(),
                ));
            }
        }
    }
    items.sort();
    items.dedup();
    items
}

fn normalize_identifier(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    normalized.trim_matches('-').to_string()
}

fn taxonomy_node_id(
    project_id: &str,
    dimension: TaxonomyDimensionV1,
    source_field_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"perseval.taxonomy-node.v1\0");
    hasher.update(project_id.as_bytes());
    hasher.update([0]);
    hasher.update(format!("{dimension:?}").as_bytes());
    hasher.update([0]);
    hasher.update(source_field_id.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn context_provenance_name(value: ContextFieldProvenanceV1) -> &'static str {
    match value {
        ContextFieldProvenanceV1::UserDeclared => "user_declared",
        ContextFieldProvenanceV1::ConfigImport => "config_import",
        ContextFieldProvenanceV1::ToolSchema => "tool_schema",
        ContextFieldProvenanceV1::TelemetryInferred => "telemetry_inferred",
        ContextFieldProvenanceV1::SystemInferred => "system_inferred",
    }
}

fn context_sensitivity_name(value: ContextSensitivityV1) -> &'static str {
    match value {
        ContextSensitivityV1::Public => "public",
        ContextSensitivityV1::Internal => "internal",
        ContextSensitivityV1::HostedPreRedacted => "hosted_pre_redacted",
        ContextSensitivityV1::SensitiveLocalOnly => "sensitive_local_only",
        ContextSensitivityV1::Secret => "secret",
        ContextSensitivityV1::Credential => "credential",
        ContextSensitivityV1::HiddenLabel => "hidden_label",
        ContextSensitivityV1::ExpectedAnswer => "expected_answer",
        ContextSensitivityV1::Unclassified => "unclassified",
    }
}

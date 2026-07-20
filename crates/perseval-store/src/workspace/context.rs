use super::*;

use crate::model::{
    AGENT_CONTEXT_DRAFT_SCHEMA_VERSION, AgentContextDraftV1, AgentContextGovernanceSummaryV1,
    ContextBackfillPreviewV1, ContextBackfillResultV1, ContextBindingRecordV1,
    ContextBindingRuleSetV1, ContextBindingStatusV1, ReviewAuthorityV1,
};
use traces_to_evals::{
    AgentContextReleaseV1, TRACE_CONTEXT_BINDING_SCHEMA_VERSION, TraceContextBindingProvenanceV1,
    TraceContextBindingResolutionV1, TraceContextBindingV1,
};

impl WorkspaceStore {
    pub fn latest_agent_context_release(
        &self,
        project_id: &str,
    ) -> Result<Option<(String, AgentContextReleaseV1)>, StoreError> {
        validate_project_scope(project_id)?;
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT context_release_id, release_json FROM agent_context_releases
                 WHERE project_id = ?1
                 ORDER BY activated_at_unix_ms DESC, rowid DESC LIMIT 1",
                params![project_id],
                |row| {
                    let release_id: String = row.get(0)?;
                    let release_json: String = row.get(1)?;
                    let release = serde_json::from_str(&release_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            release_json.len(),
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok((release_id, release))
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn agent_context_governance_summary(
        &self,
        project_id: &str,
    ) -> Result<AgentContextGovernanceSummaryV1, StoreError> {
        validate_project_scope(project_id)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        let source_snapshot_count = control.query_row(
            "SELECT COUNT(*) FROM agent_context_source_snapshots WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let drafts_in_review = control.query_row(
            "SELECT COUNT(*) FROM agent_context_drafts WHERE project_id = ?1 AND status = 'review'",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let latest_draft = control
            .query_row(
                "SELECT draft_json FROM agent_context_drafts
                 WHERE project_id = ?1 AND status = 'review'
                 ORDER BY updated_at_unix_ms DESC, rowid DESC LIMIT 1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        let (active_release_count, latest_context_release_id, latest_context_release_json) =
            control.query_row(
                "SELECT COUNT(*),
                (SELECT context_release_id FROM agent_context_releases
                 WHERE project_id = ?1 ORDER BY activated_at_unix_ms DESC, rowid DESC LIMIT 1),
                (SELECT release_json FROM agent_context_releases
                 WHERE project_id = ?1 ORDER BY activated_at_unix_ms DESC, rowid DESC LIMIT 1)
             FROM agent_context_releases WHERE project_id = ?1",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )?;
        let latest_context_release = latest_context_release_json
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        let binding_count = |resolution: &str| -> Result<u64, StoreError> {
            Ok(control.query_row(
                "SELECT COUNT(*) FROM trace_context_bindings
                 WHERE project_id = ?1 AND resolution = ?2",
                params![project_id, resolution],
                |row| row.get::<_, i64>(0),
            )? as u64)
        };
        Ok(AgentContextGovernanceSummaryV1 {
            project_id: project_id.into(),
            source_snapshot_count,
            drafts_in_review,
            active_release_count,
            latest_draft,
            latest_context_release_id,
            latest_context_release,
            resolved_bindings: binding_count("resolved")?,
            unresolved_bindings: binding_count("unresolved")?,
            ambiguous_bindings: binding_count("ambiguous")?,
        })
    }

    pub fn record_context_source_snapshot(
        &self,
        project_id: &str,
        source_kind: &str,
        source_locator: &str,
        content_hash: &str,
        sensitivity: &str,
        manifest: &Value,
    ) -> Result<String, StoreError> {
        validate_project_scope(project_id)?;
        for (name, value) in [
            ("source_kind", source_kind),
            ("source_locator", source_locator),
            ("content_hash", content_hash),
            ("sensitivity", sensitivity),
        ] {
            if value.trim().is_empty() {
                return Err(StoreError::Invalid(format!("{name} must not be empty")));
            }
        }
        let identity = serde_json::json!({
            "project_id": project_id,
            "source_kind": source_kind,
            "source_locator": source_locator,
            "content_hash": content_hash,
            "sensitivity": sensitivity,
            "manifest": manifest,
        });
        let source_snapshot_id =
            learned_identity("perseval.context-source-snapshot.v1", &identity)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        control.execute(
            "INSERT OR IGNORE INTO agent_context_source_snapshots(
                source_snapshot_id, project_id, source_kind, source_locator, content_hash,
                sensitivity, captured_at_unix_ms, manifest_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                source_snapshot_id,
                project_id,
                source_kind,
                source_locator,
                content_hash,
                sensitivity,
                now_unix_ms(),
                serde_json::to_string(manifest)?,
            ],
        )?;
        Ok(source_snapshot_id)
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
    ) -> Result<AgentContextDraftV1, StoreError> {
        validate_project_scope(project_id)?;
        if agent_id.trim().is_empty() || created_by.trim().is_empty() {
            return Err(StoreError::Invalid(
                "agent_id and created_by must not be empty".into(),
            ));
        }
        if authority == ReviewAuthorityV1::McpAgent
            && contains_provenance(&proposed_context, "user_declared")
        {
            return Err(StoreError::Invalid(
                "MCP-prepared drafts cannot mark fields as user_declared".into(),
            ));
        }
        let now = now_unix_ms();
        let control = self.control.lock().expect("control store lock poisoned");
        let source_manifest: String = control
            .query_row(
                "SELECT manifest_json FROM agent_context_source_snapshots
                 WHERE source_snapshot_id = ?1 AND project_id = ?2",
                params![source_snapshot_id, project_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid("context source snapshot is missing or cross-project".into())
            })?;
        let source_manifest: Value = serde_json::from_str(&source_manifest)?;
        let draft_identity = serde_json::json!({
            "project_id": project_id,
            "agent_id": agent_id,
            "source_snapshot_id": source_snapshot_id,
            "proposed_context": proposed_context,
            "created_at_unix_ms": now,
        });
        let draft_id = learned_identity("perseval.agent-context-draft.v1", &draft_identity)?;
        let draft = AgentContextDraftV1 {
            schema_version: AGENT_CONTEXT_DRAFT_SCHEMA_VERSION.into(),
            draft_id: draft_id.clone(),
            project_id: project_id.into(),
            agent_id: agent_id.into(),
            source_snapshot_id: source_snapshot_id.into(),
            source_manifest: source_manifest.clone(),
            proposed_context,
            unresolved_field_ids,
            conflicting_field_ids,
            created_by: created_by.into(),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        };
        control.execute(
            "INSERT INTO agent_context_drafts(
                draft_id, project_id, agent_id, source_snapshot_id, status, draft_json,
                source_manifest_json, source_snapshot_digest, created_by,
                created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, 'review', ?5, ?6, ?4, ?7, ?8, ?8)",
            params![
                draft_id,
                project_id,
                agent_id,
                source_snapshot_id,
                serde_json::to_string(&draft)?,
                serde_json::to_string(&source_manifest)?,
                created_by,
                now,
            ],
        )?;
        Ok(draft)
    }

    /// Applies a human review decision to an already prepared, sourced draft.
    /// Original provenance and source identities are preserved; only review
    /// state changes before the immutable release is activated.
    pub fn approve_agent_context_draft(
        &self,
        draft_id: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can approve an agent specification".into(),
            ));
        }
        let draft = {
            let control = self.control.lock().expect("control store lock poisoned");
            control
                .query_row(
                    "SELECT draft_json FROM agent_context_drafts
                     WHERE draft_id = ?1 AND status = 'review'",
                    params![draft_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|json| serde_json::from_str::<AgentContextDraftV1>(&json))
                .transpose()?
                .ok_or_else(|| {
                    StoreError::Invalid("context draft is missing or already closed".into())
                })?
        };
        if !draft.unresolved_field_ids.is_empty() || !draft.conflicting_field_ids.is_empty() {
            return Err(StoreError::Invalid(
                "unresolved or conflicting fields must be corrected before approval".into(),
            ));
        }
        let mut release_json = draft.proposed_context;
        approve_context_review_states(&mut release_json);
        let release: AgentContextReleaseV1 = serde_json::from_value(release_json)?;
        self.activate_agent_context_release(draft_id, &release, activated_by, authority)
    }

    pub fn activate_agent_context_release(
        &self,
        draft_id: &str,
        release: &AgentContextReleaseV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can activate an agent specification".into(),
            ));
        }
        release
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let release_json = serde_json::to_value(release)?;
        let metadata = collect_field_metadata(&release_json)?;
        if metadata.is_empty() {
            return Err(StoreError::Invalid(
                "agent context release contains no sourced fields".into(),
            ));
        }
        if metadata
            .iter()
            .any(|field| field.get("review_state").and_then(Value::as_str) != Some("approved"))
        {
            return Err(StoreError::Invalid(
                "every context field must be human-approved before activation".into(),
            ));
        }
        let release_id = release
            .release_id()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let (project_id, agent_id, source_snapshot_id, unresolved_count, conflict_count): (
            String,
            String,
            String,
            usize,
            usize,
        ) = transaction
            .query_row(
                "SELECT project_id, agent_id, source_snapshot_id, draft_json
                 FROM agent_context_drafts WHERE draft_id = ?1 AND status = 'review'",
                params![draft_id],
                |row| {
                    let draft_json: String = row.get(3)?;
                    let draft: AgentContextDraftV1 =
                        serde_json::from_str(&draft_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                draft_json.len(),
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        draft.unresolved_field_ids.len(),
                        draft.conflicting_field_ids.len(),
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid("context draft is missing or already closed".into())
            })?;
        if unresolved_count > 0 || conflict_count > 0 {
            return Err(StoreError::Invalid(
                "unresolved or conflicting context fields must be reviewed before activation"
                    .into(),
            ));
        }
        if agent_id != release.agent_id {
            return Err(StoreError::Invalid(
                "activated context agent_id does not match its draft".into(),
            ));
        }
        let source_exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_context_source_snapshots
              WHERE source_snapshot_id = ?1 AND project_id = ?2)",
            params![source_snapshot_id, project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !source_exists {
            return Err(StoreError::Invalid(
                "draft source changed or disappeared; prepare a new review draft".into(),
            ));
        }
        let latest_source_snapshot_id: String = transaction.query_row(
            "SELECT newest.source_snapshot_id
             FROM agent_context_source_snapshots original
             JOIN agent_context_source_snapshots newest
               ON newest.project_id = original.project_id
              AND newest.source_locator = original.source_locator
             WHERE original.source_snapshot_id = ?1
             ORDER BY newest.captured_at_unix_ms DESC, newest.rowid DESC LIMIT 1",
            params![source_snapshot_id],
            |row| row.get(0),
        )?;
        if latest_source_snapshot_id != source_snapshot_id {
            return Err(StoreError::Invalid(
                "repository source changed after draft preparation; regenerate the draft and review its diff"
                    .into(),
            ));
        }
        transaction.execute(
            "INSERT INTO agent_context_releases(
                context_release_id, project_id, agent_id, source_draft_id, release_json,
                activated_by, activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                release_id,
                project_id,
                agent_id,
                draft_id,
                serde_json::to_string(release)?,
                activated_by,
                now,
            ],
        )?;
        for field in metadata {
            let field_source = field["source_snapshot_id"].as_str().unwrap_or_default();
            let field_source_exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_context_source_snapshots
                  WHERE source_snapshot_id = ?1 AND project_id = ?2)",
                params![field_source, project_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !field_source_exists {
                return Err(StoreError::Invalid(format!(
                    "context field {} references a missing or cross-project source snapshot",
                    field["field_id"].as_str().unwrap_or("<unknown>")
                )));
            }
            transaction.execute(
                "INSERT INTO agent_context_field_provenance(
                    context_release_id, field_id, provenance, source_snapshot_id,
                    source_locator, review_state, sensitivity, confidence, metadata_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    release_id,
                    field["field_id"].as_str().unwrap_or_default(),
                    field["provenance"].as_str().unwrap_or_default(),
                    field_source,
                    field.get("source_locator").and_then(Value::as_str),
                    field["review_state"].as_str().unwrap_or_default(),
                    field["sensitivity"].as_str().unwrap_or_default(),
                    field.get("inference_confidence").and_then(Value::as_f64),
                    serde_json::to_string(&field)?,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE agent_context_drafts SET status = 'activated', updated_at_unix_ms = ?2
             WHERE draft_id = ?1",
            params![draft_id, now],
        )?;
        transaction.commit()?;
        Ok(release_id)
    }

    pub fn activate_context_binding_rules(
        &self,
        rules: &ContextBindingRuleSetV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can activate context binding rules".into(),
            ));
        }
        validate_project_scope(&rules.project_id)?;
        let rule_id = learned_identity("perseval.context-binding-rules.v1", rules)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, &rules.project_id)?;
        for context_release_id in rules
            .selectors
            .iter()
            .map(|selector| selector.context_release_id.as_str())
            .chain(rules.reviewed_default_context_release_id.as_deref())
        {
            let exists = control.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_context_releases
                  WHERE context_release_id = ?1 AND project_id = ?2)",
                params![context_release_id, rules.project_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(StoreError::Invalid(
                    "binding rule references a missing or cross-project context release".into(),
                ));
            }
        }
        control.execute(
            "INSERT OR IGNORE INTO agent_context_binding_rule_releases(
                binding_rule_release_id, project_id, rule_json, activated_by,
                activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                rule_id,
                rules.project_id,
                serde_json::to_string(rules)?,
                activated_by,
                now_unix_ms(),
            ],
        )?;
        Ok(rule_id)
    }

    pub fn bind_finalized_trace_context(
        &self,
        project_id: &str,
        logical_trace_id: &str,
        revision: u64,
        binding_rule_release_id: &str,
        provenance_override: Option<TraceContextBindingProvenanceV1>,
    ) -> Result<ContextBindingRecordV1, StoreError> {
        validate_project_scope(project_id)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        let (agent_id, build_id, environment, lifecycle): (
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = control
            .query_row(
                "SELECT t.agent_id, t.build_id, t.environment, r.lifecycle
                 FROM logical_traces t JOIN trace_revisions r
                   ON r.logical_trace_id = t.logical_trace_id AND r.revision = ?3
                 WHERE t.workspace_id = ?1 AND t.project_id = ?2
                   AND t.logical_trace_id = ?4 AND t.revision >= ?3",
                params![
                    self.workspace_id,
                    project_id,
                    revision as i64,
                    logical_trace_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid("exact trace revision is missing".into()))?;
        if lifecycle != "finalized" {
            return Err(StoreError::Invalid(
                "only finalized trace revisions can be bound".into(),
            ));
        }
        let rules_json: String = control
            .query_row(
                "SELECT rule_json FROM agent_context_binding_rule_releases
                 WHERE binding_rule_release_id = ?1 AND project_id = ?2",
                params![binding_rule_release_id, project_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid("binding rule release is missing".into()))?;
        let rules: ContextBindingRuleSetV1 = serde_json::from_str(&rules_json)?;
        let matches = rules
            .selectors
            .iter()
            .filter(|selector| {
                selector
                    .agent_id
                    .as_ref()
                    .is_none_or(|value| agent_id.as_ref() == Some(value))
                    && selector
                        .build_id
                        .as_ref()
                        .is_none_or(|value| build_id.as_ref() == Some(value))
                    && selector
                        .environment
                        .as_ref()
                        .is_none_or(|value| environment.as_ref() == Some(value))
            })
            .map(|selector| selector.context_release_id.clone())
            .collect::<BTreeSet<_>>();
        let (resolution, context_release_id, candidates, provenance) = match matches.len() {
            0 => match rules.reviewed_default_context_release_id.clone() {
                Some(release_id) => (
                    TraceContextBindingResolutionV1::Resolved,
                    Some(release_id),
                    BTreeSet::new(),
                    TraceContextBindingProvenanceV1::ReviewedProjectDefault,
                ),
                None => (
                    TraceContextBindingResolutionV1::Unresolved,
                    None,
                    BTreeSet::new(),
                    TraceContextBindingProvenanceV1::NoSelectorMatch,
                ),
            },
            1 => (
                TraceContextBindingResolutionV1::Resolved,
                matches.iter().next().cloned(),
                BTreeSet::new(),
                TraceContextBindingProvenanceV1::SelectorRule,
            ),
            _ => (
                TraceContextBindingResolutionV1::Ambiguous,
                None,
                matches,
                TraceContextBindingProvenanceV1::MultipleSelectorMatches,
            ),
        };
        let binding = TraceContextBindingV1 {
            schema_version: TRACE_CONTEXT_BINDING_SCHEMA_VERSION.into(),
            target_key: logical_trace_id.into(),
            target_revision: revision.to_string(),
            resolution,
            agent_context_release_id: context_release_id.clone(),
            binding_rule_release_id: binding_rule_release_id.into(),
            binding_provenance: provenance_override.unwrap_or(provenance),
            candidate_context_release_ids: candidates,
        };
        let binding_id = binding
            .binding_id()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let now = now_unix_ms();
        control.execute(
            "INSERT OR IGNORE INTO trace_context_bindings(
                binding_id, project_id, logical_trace_id, revision, resolution,
                context_release_id, binding_rule_release_id, provenance, binding_json,
                created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                binding_id,
                project_id,
                logical_trace_id,
                revision as i64,
                binding_resolution_name(binding.resolution),
                context_release_id,
                binding_rule_release_id,
                format!("{:?}", binding.binding_provenance).to_ascii_lowercase(),
                serde_json::to_string(&binding)?,
                now,
            ],
        )?;
        Ok(ContextBindingRecordV1 {
            binding_id,
            project_id: project_id.into(),
            logical_trace_id: logical_trace_id.into(),
            revision,
            status: match binding.resolution {
                TraceContextBindingResolutionV1::Resolved => ContextBindingStatusV1::Resolved,
                TraceContextBindingResolutionV1::Unresolved => ContextBindingStatusV1::Unresolved,
                TraceContextBindingResolutionV1::Ambiguous => ContextBindingStatusV1::Ambiguous,
            },
            context_release_id,
            binding_rule_release_id: binding_rule_release_id.into(),
            binding_json: serde_json::to_string(&binding)?,
            created_at_unix_ms: now,
        })
    }

    pub fn preview_context_backfill(
        &self,
        project_id: &str,
        context_release_id: &str,
    ) -> Result<ContextBackfillPreviewV1, StoreError> {
        validate_project_scope(project_id)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        let release_exists = control.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_context_releases
              WHERE context_release_id = ?1 AND project_id = ?2)",
            params![context_release_id, project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !release_exists {
            return Err(StoreError::Invalid("context release is missing".into()));
        }
        let mut statement = control.prepare(
            "SELECT r.logical_trace_id, r.revision,
                    EXISTS(SELECT 1 FROM trace_context_bindings b
                       WHERE b.project_id = ?2 AND b.logical_trace_id = r.logical_trace_id
                         AND b.revision = r.revision AND b.resolution = 'resolved')
             FROM trace_revisions r JOIN logical_traces t
               ON t.logical_trace_id = r.logical_trace_id
             WHERE t.workspace_id = ?1 AND t.project_id = ?2 AND r.lifecycle = 'finalized'
             ORDER BY r.logical_trace_id, r.revision",
        )?;
        let rows = statement
            .query_map(params![self.workspace_id, project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let affected_revisions = rows
            .iter()
            .map(|(trace_id, revision, _)| (trace_id.clone(), *revision))
            .collect::<Vec<_>>();
        let unresolved_revisions = rows
            .into_iter()
            .filter(|(_, _, resolved)| !resolved)
            .map(|(trace_id, revision, _)| (trace_id, revision))
            .collect::<Vec<_>>();
        let selection_hash = learned_identity(
            "perseval.context-backfill-selection.v1",
            &(project_id, context_release_id, &affected_revisions),
        )?;
        Ok(ContextBackfillPreviewV1 {
            project_id: project_id.into(),
            context_release_id: context_release_id.into(),
            selection_hash,
            affected_revisions,
            unresolved_revisions,
        })
    }

    pub fn apply_reviewed_default_context_backfill(
        &self,
        project_id: &str,
        context_release_id: &str,
        expected_selection_hash: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<ContextBackfillResultV1, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can apply a context backfill".into(),
            ));
        }
        validate_project_scope(project_id)?;
        if activated_by.trim().is_empty() {
            return Err(StoreError::Invalid("activated_by must not be empty".into()));
        }
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let release_exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_context_releases
              WHERE context_release_id = ?1 AND project_id = ?2)",
            params![context_release_id, project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !release_exists {
            return Err(StoreError::Invalid(
                "context release is missing or cross-project".into(),
            ));
        }
        let affected_revisions = {
            let mut statement = transaction.prepare(
                "SELECT r.logical_trace_id, r.revision
                 FROM trace_revisions r JOIN logical_traces t
                   ON t.logical_trace_id = r.logical_trace_id
                 WHERE t.workspace_id = ?1 AND t.project_id = ?2
                   AND r.lifecycle = 'finalized'
                 ORDER BY r.logical_trace_id, r.revision",
            )?;
            statement
                .query_map(params![self.workspace_id, project_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let selection_hash = learned_identity(
            "perseval.context-backfill-selection.v1",
            &(project_id, context_release_id, &affected_revisions),
        )?;
        if selection_hash != expected_selection_hash {
            return Err(StoreError::Invalid(
                "context backfill preview is stale; prepare it again".into(),
            ));
        }
        let rules = ContextBindingRuleSetV1 {
            project_id: project_id.into(),
            selectors: Vec::new(),
            reviewed_default_context_release_id: Some(context_release_id.into()),
        };
        let binding_rule_release_id =
            learned_identity("perseval.context-binding-rules.v1", &rules)?;
        transaction.execute(
            "INSERT OR IGNORE INTO agent_context_binding_rule_releases(
                binding_rule_release_id, project_id, rule_json, activated_by,
                activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                binding_rule_release_id,
                project_id,
                serde_json::to_string(&rules)?,
                activated_by,
                now,
            ],
        )?;
        for (logical_trace_id, revision) in &affected_revisions {
            let binding = TraceContextBindingV1 {
                schema_version: TRACE_CONTEXT_BINDING_SCHEMA_VERSION.into(),
                target_key: logical_trace_id.clone(),
                target_revision: revision.to_string(),
                resolution: TraceContextBindingResolutionV1::Resolved,
                agent_context_release_id: Some(context_release_id.into()),
                binding_rule_release_id: binding_rule_release_id.clone(),
                binding_provenance: TraceContextBindingProvenanceV1::ReviewedProjectDefault,
                candidate_context_release_ids: BTreeSet::new(),
            };
            let binding_id = binding
                .binding_id()
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            transaction.execute(
                "INSERT OR IGNORE INTO trace_context_bindings(
                    binding_id, project_id, logical_trace_id, revision, resolution,
                    context_release_id, binding_rule_release_id, provenance, binding_json,
                    created_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, 'resolved', ?5, ?6,
                           'reviewed_project_default', ?7, ?8)",
                params![
                    binding_id,
                    project_id,
                    logical_trace_id,
                    *revision as i64,
                    context_release_id,
                    binding_rule_release_id,
                    serde_json::to_string(&binding)?,
                    now,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(ContextBackfillResultV1 {
            project_id: project_id.into(),
            context_release_id: context_release_id.into(),
            binding_rule_release_id,
            selection_hash,
            bound_revisions: affected_revisions,
        })
    }
}

pub(super) fn validate_project_scope(project_id: &str) -> Result<(), StoreError> {
    if project_id.trim().is_empty() || project_id == crate::model::UNASSIGNED_PROJECT_ID {
        return Err(StoreError::Invalid(
            "learned context requires an explicit project".into(),
        ));
    }
    Ok(())
}

pub(super) fn ensure_project_exists(
    connection: &SqliteConnection,
    workspace_id: &str,
    project_id: &str,
) -> Result<(), StoreError> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE workspace_id = ?1 AND project_id = ?2)",
        params![workspace_id, project_id],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        return Err(StoreError::Invalid("project does not exist".into()));
    }
    Ok(())
}

fn learned_identity<T: serde::Serialize>(domain: &str, value: &T) -> Result<String, StoreError> {
    let material = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(material);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn contains_provenance(value: &Value, expected: &str) -> bool {
    match value {
        Value::Object(values) => {
            values.get("provenance").and_then(Value::as_str) == Some(expected)
                || values
                    .values()
                    .any(|value| contains_provenance(value, expected))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| contains_provenance(value, expected)),
        _ => false,
    }
}

fn approve_context_review_states(value: &mut Value) {
    match value {
        Value::Object(values) => {
            if values.contains_key("field_id") && values.contains_key("review_state") {
                values.insert("review_state".into(), Value::String("approved".into()));
            }
            for value in values.values_mut() {
                approve_context_review_states(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                approve_context_review_states(value);
            }
        }
        _ => {}
    }
}

fn collect_field_metadata(value: &Value) -> Result<Vec<Value>, StoreError> {
    fn visit(value: &Value, found: &mut Vec<Value>) {
        match value {
            Value::Object(values) => {
                if values.contains_key("field_id")
                    && values.contains_key("provenance")
                    && values.contains_key("source_snapshot_id")
                    && values.contains_key("review_state")
                    && values.contains_key("sensitivity")
                {
                    found.push(value.clone());
                }
                for value in values.values() {
                    visit(value, found);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    visit(value, &mut found);
    let mut ids = BTreeSet::new();
    for field in &found {
        let id = field["field_id"].as_str().unwrap_or_default();
        if id.is_empty() || !ids.insert(id) {
            return Err(StoreError::Invalid(
                "context field provenance contains an empty or duplicate field_id".into(),
            ));
        }
    }
    Ok(found)
}

fn binding_resolution_name(resolution: TraceContextBindingResolutionV1) -> &'static str {
    match resolution {
        TraceContextBindingResolutionV1::Resolved => "resolved",
        TraceContextBindingResolutionV1::Unresolved => "unresolved",
        TraceContextBindingResolutionV1::Ambiguous => "ambiguous",
    }
}

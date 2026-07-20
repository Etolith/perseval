use super::*;

use crate::model::{
    ASSESSMENT_JOB_EXPORT_SCHEMA_VERSION, ASSESSMENT_JOB_SCHEMA_VERSION,
    ASSESSMENT_RECORD_SCHEMA_VERSION, AssessmentCommitV1, AssessmentItemStatusV1,
    AssessmentJobExportV1, AssessmentJobItemExportV1, AssessmentJobStatusV1, AssessmentJobV1,
    AssessmentRecordV1, AssessmentRuntimeHealthV1, ClaimedAssessmentItemV1, ContextBindingStatusV1,
    PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION, ProjectAssessmentPolicyV1, ReviewAuthorityV1,
};
use rusqlite::TransactionBehavior;
use traces_to_evals::{
    EVALUATOR_RELEASE_SCHEMA_VERSION, EvaluatorReleaseSpecV1, LearnedAbstentionReasonV1,
    LearnedVerdictV1, TRACE_CONTEXT_BINDING_SCHEMA_VERSION, TraceContextBindingProvenanceV1,
    TraceContextBindingResolutionV1, TraceContextBindingV1,
};

impl WorkspaceStore {
    pub fn activate_evaluator_release(
        &self,
        project_id: &str,
        evaluator: &EvaluatorReleaseSpecV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can activate an evaluator release".into(),
            ));
        }
        evaluator
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let release_id = evaluator
            .release_id()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let definition_id = assessment_identity(
            "perseval.evaluator-definition.v1",
            &(project_id, &evaluator.name, &evaluator.task_kind),
        )?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO evaluator_definitions(
                evaluator_definition_id, project_id, name, task_kind, created_by,
                created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                definition_id,
                project_id,
                evaluator.name,
                serde_json::to_string(&evaluator.task_kind)?,
                activated_by,
                now,
            ],
        )?;
        transaction.execute(
            "UPDATE evaluator_releases SET active = 0
             WHERE evaluator_definition_id = ?1 AND project_id = ?2",
            params![definition_id, project_id],
        )?;
        transaction.execute(
            "INSERT INTO evaluator_releases(
                evaluator_release_id, evaluator_definition_id, project_id, release_json,
                active, activated_by, created_at_unix_ms, activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6)
             ON CONFLICT(evaluator_release_id) DO UPDATE SET
                active = 1, activated_by = excluded.activated_by,
                activated_at_unix_ms = excluded.activated_at_unix_ms",
            params![
                release_id,
                definition_id,
                project_id,
                serde_json::to_string(evaluator)?,
                activated_by,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(release_id)
    }

    pub fn set_project_assessment_policy(
        &self,
        policy: &ProjectAssessmentPolicyV1,
        authority: ReviewAuthorityV1,
    ) -> Result<(), StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can change provider and budget policy".into(),
            ));
        }
        policy.validate().map_err(StoreError::Invalid)?;
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "INSERT INTO project_assessment_policies(
                project_id, policy_json, provider_enabled, daily_budget_micros,
                per_attempt_budget_micros, lease_duration_ms, maximum_attempts,
                updated_by, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(project_id) DO UPDATE SET
                policy_json = excluded.policy_json,
                provider_enabled = excluded.provider_enabled,
                daily_budget_micros = excluded.daily_budget_micros,
                per_attempt_budget_micros = excluded.per_attempt_budget_micros,
                lease_duration_ms = excluded.lease_duration_ms,
                maximum_attempts = excluded.maximum_attempts,
                updated_by = excluded.updated_by,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                policy.project_id,
                serde_json::to_string(policy)?,
                policy.provider_enabled,
                policy.daily_budget_micros as i64,
                policy.per_attempt_budget_micros as i64,
                policy.lease_duration_ms as i64,
                policy.maximum_attempts as i64,
                policy.updated_by,
                policy.updated_at_unix_ms,
            ],
        )?;
        Ok(())
    }

    pub fn project_assessment_policy(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectAssessmentPolicyV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let encoded = control
            .query_row(
                "SELECT policy_json FROM project_assessment_policies WHERE project_id = ?1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub fn enqueue_assessment_job(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        exact_revisions: &[(String, u64)],
        idempotency_key: &str,
    ) -> Result<AssessmentJobV1, StoreError> {
        if exact_revisions.is_empty() {
            return Err(StoreError::Invalid(
                "assessment selection must contain at least one exact revision".into(),
            ));
        }
        if idempotency_key.trim().is_empty() {
            return Err(StoreError::Invalid(
                "idempotency key must not be empty".into(),
            ));
        }
        let mut normalized = exact_revisions.to_vec();
        normalized.sort();
        normalized.dedup();
        let selection_hash = assessment_identity(
            "perseval.assessment-selection.v1",
            &(project_id, evaluator_release_id, &normalized),
        )?;

        // Authorize every exact target before loading any analytical projection.
        // This prevents cross-project selections from becoming an implicit read path.
        {
            let control = self.control.lock().expect("control store lock poisoned");
            for (logical_trace_id, revision) in &normalized {
                let allowed = control.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM logical_traces t JOIN trace_revisions r
                          ON r.logical_trace_id = t.logical_trace_id AND r.revision = ?4
                        WHERE t.workspace_id = ?1 AND t.project_id = ?2
                          AND t.logical_trace_id = ?3 AND r.lifecycle = 'finalized'
                    )",
                    params![
                        self.workspace_id,
                        project_id,
                        logical_trace_id,
                        *revision as i64
                    ],
                    |row| row.get::<_, bool>(0),
                )?;
                if !allowed {
                    return Err(StoreError::Invalid(
                        "assessment selection contains a cross-project or non-finalized revision"
                            .into(),
                    ));
                }
            }
        }

        // Projection creation can scan DuckDB. Do it before taking the SQLite
        // writer lock so provider/projection work never blocks control-plane writes.
        let mut projected = Vec::with_capacity(normalized.len());
        for (logical_trace_id, revision) in &normalized {
            let input = self.load_behavior_input(logical_trace_id, *revision)?;
            let projection_hash =
                assessment_identity("perseval.assessment-structural-projection.v1", &input)?;
            projected.push((logical_trace_id.clone(), *revision, projection_hash));
        }

        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        if let Some(existing) = load_job_by_idempotency(&transaction, project_id, idempotency_key)?
        {
            if existing.selection_hash != selection_hash
                || existing.evaluator_release_id != evaluator_release_id
            {
                return Err(StoreError::Invalid(
                    "idempotency key already belongs to a different assessment selection".into(),
                ));
            }
            return Ok(existing);
        }
        let evaluator_json: String = transaction
            .query_row(
                "SELECT release_json FROM evaluator_releases
                 WHERE evaluator_release_id = ?1 AND project_id = ?2 AND active = 1",
                params![evaluator_release_id, project_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid("active evaluator release is missing".into()))?;
        let evaluator: EvaluatorReleaseSpecV1 = serde_json::from_str(&evaluator_json)?;
        if evaluator.schema_version != EVALUATOR_RELEASE_SCHEMA_VERSION {
            return Err(StoreError::Invalid("unsupported evaluator release".into()));
        }
        let job_id = assessment_identity(
            "perseval.assessment-job.v1",
            &(
                project_id,
                evaluator_release_id,
                &selection_hash,
                idempotency_key,
            ),
        )?;
        transaction.execute(
            "INSERT INTO assessment_jobs(
                job_id, project_id, evaluator_release_id, idempotency_key, selection_hash,
                status, item_count, terminal_count, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, 0, ?7, ?7)",
            params![
                job_id,
                project_id,
                evaluator_release_id,
                idempotency_key,
                selection_hash,
                projected.len() as i64,
                now,
            ],
        )?;
        for (logical_trace_id, revision, projection_hash) in projected {
            let (trace_project_id, lifecycle, finalized_at): (String, String, Option<i64>) =
                transaction
                    .query_row(
                        "SELECT t.project_id, r.lifecycle, r.finalized_at_unix_ms
                         FROM logical_traces t JOIN trace_revisions r
                           ON r.logical_trace_id = t.logical_trace_id AND r.revision = ?3
                         WHERE t.workspace_id = ?1 AND t.logical_trace_id = ?2",
                        params![self.workspace_id, logical_trace_id, revision as i64],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        StoreError::Invalid("selected exact revision is missing".into())
                    })?;
            if trace_project_id != project_id || lifecycle != "finalized" {
                return Err(StoreError::Invalid(
                    "assessment selection contains a cross-project or non-finalized revision"
                        .into(),
                ));
            }
            let target_id = assessment_identity(
                "perseval.assessment-target.v1",
                &(project_id, &logical_trace_id, revision, "trace_revision"),
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO assessment_targets(
                    target_id, project_id, logical_trace_id, revision, target_kind,
                    target_key, target_revision, finalized_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, 'trace_revision', ?3, ?5, ?6)",
                params![
                    target_id,
                    project_id,
                    logical_trace_id,
                    revision as i64,
                    revision.to_string(),
                    finalized_at.unwrap_or(now),
                ],
            )?;
            let binding = latest_or_unresolved_binding(
                &transaction,
                project_id,
                &logical_trace_id,
                revision,
                now,
            )?;
            let cache_key = assessment_identity(
                "perseval.assessment-cache.v1",
                &(
                    evaluator_release_id,
                    &binding.binding_id,
                    &projection_hash,
                    evaluator_provider_model_identity(&evaluator),
                ),
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO assessment_projections(
                    projection_hash, target_id, projection_release_id,
                    context_projection_release_id, projection_class, projection_json,
                    created_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, 'structural_only', ?5, ?6)",
                params![
                    projection_hash,
                    target_id,
                    evaluator.projection_release_id,
                    evaluator.context_projection_release_id,
                    serde_json::to_string(&serde_json::json!({
                        "logical_trace_id": logical_trace_id,
                        "revision": revision,
                        "projection_hash": projection_hash,
                    }))?,
                    now,
                ],
            )?;
            let item_id = assessment_identity(
                "perseval.assessment-item.v1",
                &(&job_id, &target_id, evaluator_release_id),
            )?;
            transaction.execute(
                "INSERT INTO assessment_job_items(
                    item_id, job_id, project_id, target_id, logical_trace_id, revision,
                    evaluator_release_id, context_binding_id, context_release_id,
                    projection_hash, cache_key, status, created_at_unix_ms, updated_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           'pending', ?12, ?12)",
                params![
                    item_id,
                    job_id,
                    project_id,
                    target_id,
                    logical_trace_id,
                    revision as i64,
                    evaluator_release_id,
                    binding.binding_id,
                    binding.context_release_id,
                    projection_hash,
                    cache_key,
                    now,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(AssessmentJobV1 {
            schema_version: ASSESSMENT_JOB_SCHEMA_VERSION.into(),
            job_id,
            project_id: project_id.into(),
            evaluator_release_id: evaluator_release_id.into(),
            idempotency_key: idempotency_key.into(),
            selection_hash,
            status: AssessmentJobStatusV1::Pending,
            item_count: normalized.len() as u64,
            terminal_count: 0,
            cancelled_at_unix_ms: None,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        })
    }

    pub fn claim_next_assessment(
        &self,
        lease_owner: &str,
        estimated_cost_micros: u64,
    ) -> Result<Option<ClaimedAssessmentItemV1>, StoreError> {
        if lease_owner.trim().is_empty() {
            return Err(StoreError::Invalid("lease_owner must not be empty".into()));
        }
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Expired work is eligible; non-expired leases can never be stolen.
        let candidate = transaction
            .query_row(
                "SELECT i.item_id, i.job_id, i.project_id, i.logical_trace_id, i.revision,
                        i.evaluator_release_id, i.context_binding_id, i.context_release_id,
                        i.projection_hash, i.cache_key, i.attempt_count,
                        p.policy_json, j.cancel_requested
                 FROM assessment_job_items i
                 JOIN assessment_jobs j ON j.job_id = i.job_id
                 LEFT JOIN project_assessment_policies p ON p.project_id = i.project_id
                 WHERE j.cancel_requested = 0
                   AND ((i.status = 'pending' AND i.next_attempt_at_unix_ms <= ?1)
                     OR (i.status = 'running' AND i.lease_expires_at_unix_ms <= ?1))
                 ORDER BY i.created_at_unix_ms, i.item_id LIMIT 1",
                params![now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? as u64,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)? as u32,
                        row.get::<_, Option<String>>(11)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            item_id,
            job_id,
            project_id,
            logical_trace_id,
            revision,
            evaluator_release_id,
            context_binding_id,
            context_release_id,
            projection_hash,
            cache_key,
            attempt_count,
            policy_json,
        )) = candidate
        else {
            transaction.commit()?;
            return Ok(None);
        };
        if let Some(cached_json) = transaction
            .query_row(
                "SELECT assessment_json FROM assessment_cache_entries WHERE cache_key = ?1",
                params![cache_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let cached: AssessmentRecordV1 = serde_json::from_str(&cached_json)?;
            materialize_cached_assessment(
                &transaction,
                &cached,
                &item_id,
                &job_id,
                &project_id,
                &logical_trace_id,
                revision,
                &evaluator_release_id,
                &context_binding_id,
                context_release_id.as_deref(),
                &projection_hash,
                now,
            )?;
            transaction.commit()?;
            return Ok(None);
        }
        let policy = policy_json
            .map(|json| serde_json::from_str::<ProjectAssessmentPolicyV1>(&json))
            .transpose()?
            .unwrap_or_else(|| ProjectAssessmentPolicyV1 {
                schema_version: PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION.into(),
                project_id: project_id.clone(),
                provider_enabled: false,
                daily_budget_micros: 0,
                per_attempt_budget_micros: 0,
                lease_duration_ms: 30_000,
                maximum_attempts: 1,
                updated_by: "system-default".into(),
                updated_at_unix_ms: now,
            });
        if attempt_count >= policy.maximum_attempts {
            finish_without_attempt(
                &transaction,
                &item_id,
                &job_id,
                "failed",
                "retry_exhausted",
                now,
            )?;
            transaction.commit()?;
            return Ok(None);
        }
        let mut preflight_status = None;
        let reserved_cost_micros = if context_release_id.is_none() || estimated_cost_micros == 0 {
            0
        } else if !policy.provider_enabled {
            preflight_status = Some(AssessmentItemStatusV1::ProviderUnavailable);
            0
        } else {
            let reserve = estimated_cost_micros.min(policy.per_attempt_budget_micros);
            if reserve < estimated_cost_micros
                || !reserve_budget(&transaction, &project_id, &policy, reserve, now)?
            {
                preflight_status = Some(AssessmentItemStatusV1::BudgetBlocked);
                0
            } else {
                reserve
            }
        };
        let attempt_number = attempt_count + 1;
        let attempt_id = assessment_identity(
            "perseval.assessment-attempt.v1",
            &(&item_id, attempt_number, lease_owner),
        )?;
        let lease_expires_at_unix_ms = now.saturating_add(policy.lease_duration_ms as i64);
        let changed = transaction.execute(
            "UPDATE assessment_job_items SET status = 'running', attempt_count = ?2,
                    lease_owner = ?3, lease_expires_at_unix_ms = ?4,
                    updated_at_unix_ms = ?5
             WHERE item_id = ?1 AND ((status = 'pending' AND next_attempt_at_unix_ms <= ?5)
                OR (status = 'running' AND lease_expires_at_unix_ms <= ?5))",
            params![
                item_id,
                attempt_number as i64,
                lease_owner,
                lease_expires_at_unix_ms,
                now,
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::Invalid(
                "assessment lease was concurrently claimed".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO assessment_attempts(
                attempt_id, item_id, attempt_number, lease_owner, status,
                reserved_cost_micros, started_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6)",
            params![
                attempt_id,
                item_id,
                attempt_number as i64,
                lease_owner,
                reserved_cost_micros as i64,
                now,
            ],
        )?;
        transaction.execute(
            "UPDATE assessment_jobs SET status = 'running', updated_at_unix_ms = ?2
             WHERE job_id = ?1 AND status = 'pending'",
            params![job_id, now],
        )?;
        transaction.commit()?;
        Ok(Some(ClaimedAssessmentItemV1 {
            item_id,
            job_id,
            project_id,
            logical_trace_id,
            revision,
            evaluator_release_id,
            context_binding_id,
            context_release_id,
            projection_hash,
            cache_key,
            attempt_id,
            attempt_number,
            lease_owner: lease_owner.into(),
            lease_expires_at_unix_ms,
            reserved_cost_micros,
            preflight_status,
        }))
    }

    pub fn commit_assessment_attempt(
        &self,
        claim: &ClaimedAssessmentItemV1,
        commit: &AssessmentCommitV1,
    ) -> Result<Option<AssessmentRecordV1>, StoreError> {
        if !commit.status.is_terminal() {
            return Err(StoreError::Invalid(
                "assessment commit requires a terminal item status".into(),
            ));
        }
        validate_assessment_commit(claim, commit)?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, lease_owner, attempt_count, maximum_attempts, cancel_requested): (
            String,
            Option<String>,
            u32,
            u32,
            bool,
        ) = transaction.query_row(
            "SELECT i.status, i.lease_owner, i.attempt_count,
                    COALESCE(p.maximum_attempts, 1), j.cancel_requested
             FROM assessment_job_items i JOIN assessment_jobs j ON j.job_id = i.job_id
             LEFT JOIN project_assessment_policies p ON p.project_id = i.project_id
             WHERE i.item_id = ?1 AND i.job_id = ?2",
            params![claim.item_id, claim.job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, i64>(2)? as u32,
                    row.get::<_, i64>(3)? as u32,
                    row.get(4)?,
                ))
            },
        )?;
        if status != "running" || lease_owner.as_deref() != Some(claim.lease_owner.as_str()) {
            return Err(StoreError::Invalid(
                "assessment attempt no longer owns the active lease".into(),
            ));
        }
        if cancel_requested {
            release_budget(
                &transaction,
                &claim.project_id,
                claim.reserved_cost_micros,
                0,
                now,
            )?;
            transaction.execute(
                "UPDATE assessment_attempts SET status = 'cancelled', finished_at_unix_ms = ?2
                 WHERE attempt_id = ?1",
                params![claim.attempt_id, now],
            )?;
            finish_item(
                &transaction,
                &claim.item_id,
                &claim.job_id,
                "cancelled",
                now,
            )?;
            transaction.commit()?;
            return Ok(None);
        }
        if commit.charged_cost_micros > claim.reserved_cost_micros {
            return Err(StoreError::Invalid(
                "provider charge exceeds the reserved attempt budget".into(),
            ));
        }
        release_budget(
            &transaction,
            &claim.project_id,
            claim.reserved_cost_micros,
            commit.charged_cost_micros,
            now,
        )?;
        let failure_json = commit
            .provider_failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let envelope = commit.provider_response.as_ref();
        transaction.execute(
            "UPDATE assessment_attempts SET status = ?2, retryable = ?3,
                    requested_provider = ?4, requested_model = ?5, returned_model = ?6,
                    request_hash = ?7, response_hash = ?8, provider_response_id = ?9,
                    charged_cost_micros = ?10, latency_ms = ?11, failure_json = ?12,
                    finished_at_unix_ms = ?13
             WHERE attempt_id = ?1",
            params![
                claim.attempt_id,
                item_status_name(commit.status),
                commit.retryable,
                envelope.and_then(|value| value.provider.as_deref()),
                envelope.map(|value| value.requested_model.as_str()),
                envelope.and_then(|value| value.returned_model.as_deref()),
                envelope.map(|value| value.request_hash.as_str()),
                envelope.map(|value| value.response_hash.as_str()),
                envelope.and_then(|value| value.response_id.as_deref()),
                commit.charged_cost_micros as i64,
                commit.latency_ms as i64,
                failure_json,
                now,
            ],
        )?;
        if commit.retryable && attempt_count < maximum_attempts {
            let delay_ms = 250_i64.saturating_mul(1_i64 << attempt_count.min(8));
            transaction.execute(
                "UPDATE assessment_job_items SET status = 'pending', lease_owner = NULL,
                        lease_expires_at_unix_ms = NULL, next_attempt_at_unix_ms = ?2,
                        terminal_reason = ?3, updated_at_unix_ms = ?4
                 WHERE item_id = ?1",
                params![
                    claim.item_id,
                    now.saturating_add(delay_ms),
                    commit.error_code,
                    now,
                ],
            )?;
            transaction.commit()?;
            return Ok(None);
        }
        let assessment_id = assessment_identity(
            "perseval.assessment-record.v1",
            &(
                &claim.item_id,
                &claim.evaluator_release_id,
                &claim.context_binding_id,
                &claim.projection_hash,
                &commit.evaluation,
            ),
        )?;
        let evaluation = commit.evaluation.as_ref();
        let provider = envelope.and_then(|value| value.provider.clone());
        let requested_model = envelope.map(|value| value.requested_model.clone());
        let returned_model = envelope.and_then(|value| value.returned_model.clone());
        let record = AssessmentRecordV1 {
            schema_version: ASSESSMENT_RECORD_SCHEMA_VERSION.into(),
            assessment_id: assessment_id.clone(),
            item_id: claim.item_id.clone(),
            project_id: claim.project_id.clone(),
            logical_trace_id: claim.logical_trace_id.clone(),
            revision: claim.revision,
            evaluator_release_id: claim.evaluator_release_id.clone(),
            context_binding_id: claim.context_binding_id.clone(),
            context_release_id: claim.context_release_id.clone(),
            projection_hash: claim.projection_hash.clone(),
            provider: provider.clone(),
            requested_model: requested_model.clone(),
            returned_model: returned_model.clone(),
            status: commit.status,
            evaluation: commit.evaluation.clone(),
            cost_micros: commit.charged_cost_micros,
            latency_ms: commit.latency_ms,
            created_at_unix_ms: now,
        };
        transaction.execute(
            "INSERT INTO assessments(
                assessment_id, item_id, project_id, logical_trace_id, revision,
                evaluator_release_id, context_binding_id, context_release_id,
                projection_hash, provider, requested_model, returned_model, status,
                verdict, label, score, confidence, explanation, abstention_reason,
                evaluation_json, cost_micros, latency_ms, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![
                assessment_id,
                claim.item_id,
                claim.project_id,
                claim.logical_trace_id,
                claim.revision as i64,
                claim.evaluator_release_id,
                claim.context_binding_id,
                claim.context_release_id,
                claim.projection_hash,
                provider,
                requested_model,
                returned_model,
                item_status_name(commit.status),
                evaluation.map(|value| verdict_name(value.verdict)),
                evaluation.and_then(|value| value.label.as_deref()),
                evaluation.and_then(|value| value.score),
                evaluation.and_then(|value| value.model_reported_confidence),
                evaluation.map(|value| value.explanation.as_str()),
                evaluation.and_then(|value| value.abstention_reason.map(abstention_name)),
                evaluation.map(serde_json::to_string).transpose()?,
                commit.charged_cost_micros as i64,
                commit.latency_ms as i64,
                now,
            ],
        )?;
        if let Some(evaluation) = evaluation {
            for (index, evidence) in evaluation.evidence.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO assessment_evidence_refs(
                        assessment_id, evidence_index, evidence_key, evidence_kind,
                        criterion_id, location_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        assessment_id,
                        index as i64,
                        evidence.evidence_key,
                        serde_json::to_string(&evidence.evidence_kind)?,
                        evidence.criterion_id,
                        serde_json::to_string(&evidence.location)?,
                    ],
                )?;
            }
            // A provider returning a different model is preserved on the record but
            // cannot populate the exact-result cache for the requested release.
            if record.returned_model.is_none()
                || record.returned_model.as_ref() == record.requested_model.as_ref()
            {
                let provider_model_identity = format!(
                    "{}:{}",
                    record.provider.as_deref().unwrap_or("local"),
                    record
                        .returned_model
                        .as_deref()
                        .or(record.requested_model.as_deref())
                        .unwrap_or("none")
                );
                transaction.execute(
                    "INSERT OR REPLACE INTO assessment_cache_entries(
                        cache_key, evaluator_release_id, context_binding_id, projection_hash,
                        provider_model_identity, assessment_json, created_at_unix_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        claim.cache_key,
                        claim.evaluator_release_id,
                        claim.context_binding_id,
                        claim.projection_hash,
                        provider_model_identity,
                        serde_json::to_string(&record)?,
                        now,
                    ],
                )?;
            }
        }
        finish_item(
            &transaction,
            &claim.item_id,
            &claim.job_id,
            item_status_name(commit.status),
            now,
        )?;
        transaction.commit()?;
        Ok(Some(record))
    }

    pub fn cancel_assessment_job(&self, job_id: &str) -> Result<AssessmentJobV1, StoreError> {
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        transaction.execute(
            "UPDATE assessment_jobs SET cancel_requested = 1, status = 'cancelled',
                    updated_at_unix_ms = ?2 WHERE job_id = ?1
                    AND status IN ('pending', 'running', 'partial')",
            params![job_id, now],
        )?;
        transaction.execute(
            "UPDATE assessment_job_items SET status = 'cancelled', terminal_reason = 'cancelled',
                    updated_at_unix_ms = ?2
             WHERE job_id = ?1 AND status = 'pending'",
            params![job_id, now],
        )?;
        refresh_job_counts(&transaction, job_id, now)?;
        let job = load_job(&transaction, job_id)?
            .ok_or_else(|| StoreError::Invalid("assessment job not found".into()))?;
        transaction.commit()?;
        Ok(job)
    }

    pub fn list_trace_assessments(
        &self,
        project_id: &str,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<Vec<AssessmentRecordV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT assessment_id, item_id, evaluator_release_id, context_binding_id,
                    context_release_id, projection_hash, provider, requested_model,
                    returned_model, status, evaluation_json, cost_micros, latency_ms,
                    created_at_unix_ms
             FROM assessments WHERE project_id = ?1 AND logical_trace_id = ?2 AND revision = ?3
             ORDER BY created_at_unix_ms DESC, assessment_id",
        )?;
        statement
            .query_map(
                params![project_id, logical_trace_id, revision as i64],
                |row| {
                    let status: String = row.get(9)?;
                    let evaluation_json: Option<String> = row.get(10)?;
                    Ok(AssessmentRecordV1 {
                        schema_version: ASSESSMENT_RECORD_SCHEMA_VERSION.into(),
                        assessment_id: row.get(0)?,
                        item_id: row.get(1)?,
                        project_id: project_id.into(),
                        logical_trace_id: logical_trace_id.into(),
                        revision,
                        evaluator_release_id: row.get(2)?,
                        context_binding_id: row.get(3)?,
                        context_release_id: row.get(4)?,
                        projection_hash: row.get(5)?,
                        provider: row.get(6)?,
                        requested_model: row.get(7)?,
                        returned_model: row.get(8)?,
                        status: parse_item_status(&status).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                9,
                                rusqlite::types::Type::Text,
                                Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    error,
                                )),
                            )
                        })?,
                        evaluation: evaluation_json
                            .map(|json| serde_json::from_str(&json))
                            .transpose()
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    10,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                        cost_micros: row.get::<_, i64>(11)? as u64,
                        latency_ms: row.get::<_, i64>(12)? as u64,
                        created_at_unix_ms: row.get(13)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn export_assessment_job(&self, job_id: &str) -> Result<AssessmentJobExportV1, StoreError> {
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let job = load_job(&transaction, job_id)?
            .ok_or_else(|| StoreError::Invalid("assessment job not found".into()))?;
        let mut statement = transaction.prepare(
            "SELECT i.item_id, i.logical_trace_id, i.revision, i.context_binding_id,
                    i.context_release_id, i.projection_hash, i.status, i.attempt_count,
                    i.terminal_reason,
                    a.assessment_id, a.provider, a.requested_model, a.returned_model,
                    a.status, a.evaluation_json, a.cost_micros, a.latency_ms,
                    a.created_at_unix_ms
             FROM assessment_job_items i
             LEFT JOIN assessments a ON a.item_id = i.item_id
             WHERE i.job_id = ?1
             ORDER BY i.logical_trace_id, i.revision, i.item_id",
        )?;
        let items = statement
            .query_map(params![job_id], |row| {
                let item_status: String = row.get(6)?;
                let assessment_id: Option<String> = row.get(9)?;
                let assessment = assessment_id
                    .map(
                        |assessment_id| -> Result<AssessmentRecordV1, rusqlite::Error> {
                            let status: String = row.get(13)?;
                            let evaluation_json: Option<String> = row.get(14)?;
                            Ok(AssessmentRecordV1 {
                                schema_version: ASSESSMENT_RECORD_SCHEMA_VERSION.into(),
                                assessment_id,
                                item_id: row.get(0)?,
                                project_id: job.project_id.clone(),
                                logical_trace_id: row.get(1)?,
                                revision: row.get::<_, i64>(2)? as u64,
                                evaluator_release_id: job.evaluator_release_id.clone(),
                                context_binding_id: row.get(3)?,
                                context_release_id: row.get(4)?,
                                projection_hash: row.get(5)?,
                                provider: row.get(10)?,
                                requested_model: row.get(11)?,
                                returned_model: row.get(12)?,
                                status: parse_item_status(&status).map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        13,
                                        rusqlite::types::Type::Text,
                                        Box::new(std::io::Error::new(
                                            std::io::ErrorKind::InvalidData,
                                            error,
                                        )),
                                    )
                                })?,
                                evaluation: evaluation_json
                                    .map(|json| serde_json::from_str(&json))
                                    .transpose()
                                    .map_err(|error| {
                                        rusqlite::Error::FromSqlConversionFailure(
                                            14,
                                            rusqlite::types::Type::Text,
                                            Box::new(error),
                                        )
                                    })?,
                                cost_micros: row.get::<_, i64>(15)? as u64,
                                latency_ms: row.get::<_, i64>(16)? as u64,
                                created_at_unix_ms: row.get(17)?,
                            })
                        },
                    )
                    .transpose()?;
                Ok(AssessmentJobItemExportV1 {
                    item_id: row.get(0)?,
                    logical_trace_id: row.get(1)?,
                    revision: row.get::<_, i64>(2)? as u64,
                    context_binding_id: row.get(3)?,
                    context_release_id: row.get(4)?,
                    projection_hash: row.get(5)?,
                    status: parse_item_status(&item_status).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                        )
                    })?,
                    attempt_count: row.get::<_, i64>(7)? as u32,
                    terminal_reason: row.get(8)?,
                    assessment,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut status_counts = std::collections::BTreeMap::new();
        let mut total_cost_micros = 0_u64;
        let mut total_latency_ms = 0_u64;
        for item in &items {
            *status_counts
                .entry(item_status_name(item.status).to_string())
                .or_insert(0) += 1;
            if let Some(assessment) = &item.assessment {
                total_cost_micros = total_cost_micros.saturating_add(assessment.cost_micros);
                total_latency_ms = total_latency_ms.saturating_add(assessment.latency_ms);
            }
        }
        let export = AssessmentJobExportV1 {
            schema_version: ASSESSMENT_JOB_EXPORT_SCHEMA_VERSION.into(),
            job,
            status_counts,
            total_cost_micros,
            total_latency_ms,
            items,
        };
        transaction.commit()?;
        Ok(export)
    }

    pub fn assessment_runtime_health(&self) -> Result<AssessmentRuntimeHealthV1, StoreError> {
        self.assessment_runtime_health_for_project(None)
    }

    pub fn assessment_runtime_health_for_project(
        &self,
        project_id: Option<&str>,
    ) -> Result<AssessmentRuntimeHealthV1, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT
                    SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status NOT IN ('pending', 'running') THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'succeeded' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'abstained' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'budget_blocked' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'privacy_blocked' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'provider_unavailable' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'not_applicable' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'abstained' AND EXISTS(
                        SELECT 1 FROM assessments a WHERE a.item_id = assessment_job_items.item_id
                          AND a.abstention_reason = 'context_unresolved'
                    ) THEN 1 ELSE 0 END),
                    SUM(CASE WHEN attempt_count > 1 THEN attempt_count - 1 ELSE 0 END),
                    COALESCE((SELECT SUM(a.cost_micros) FROM assessments a
                        WHERE (?1 IS NULL OR a.project_id = ?1)), 0),
                    COALESCE((SELECT SUM(a.latency_ms) FROM assessments a
                        WHERE (?1 IS NULL OR a.project_id = ?1)), 0)
                 FROM assessment_job_items
                 WHERE (?1 IS NULL OR project_id = ?1)",
                params![project_id],
                |row| {
                    Ok(AssessmentRuntimeHealthV1 {
                        pending: row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64,
                        running: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        terminal: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                        succeeded: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                        abstained: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                        failed: row.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                        cancelled: row.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
                        budget_blocked: row.get::<_, Option<i64>>(7)?.unwrap_or(0) as u64,
                        privacy_blocked: row.get::<_, Option<i64>>(8)?.unwrap_or(0) as u64,
                        provider_unavailable: row.get::<_, Option<i64>>(9)?.unwrap_or(0) as u64,
                        not_applicable: row.get::<_, Option<i64>>(10)?.unwrap_or(0) as u64,
                        context_unresolved: row.get::<_, Option<i64>>(11)?.unwrap_or(0) as u64,
                        retry_count: row.get::<_, Option<i64>>(12)?.unwrap_or(0) as u64,
                        total_cost_micros: row.get::<_, i64>(13)? as u64,
                        total_latency_ms: row.get::<_, i64>(14)? as u64,
                        last_error: None,
                    })
                },
            )
            .map_err(StoreError::from)
    }
}

#[derive(Debug)]
struct BindingRef {
    binding_id: String,
    context_release_id: Option<String>,
    #[allow(dead_code)]
    status: ContextBindingStatusV1,
}

fn latest_or_unresolved_binding(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    logical_trace_id: &str,
    revision: u64,
    now: i64,
) -> Result<BindingRef, StoreError> {
    if let Some((binding_id, resolution, context_release_id)) = transaction
        .query_row(
            "SELECT binding_id, resolution, context_release_id
             FROM trace_context_bindings
             WHERE project_id = ?1 AND logical_trace_id = ?2 AND revision = ?3
             ORDER BY created_at_unix_ms DESC LIMIT 1",
            params![project_id, logical_trace_id, revision as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                ))
            },
        )
        .optional()?
    {
        return Ok(BindingRef {
            binding_id,
            context_release_id,
            status: match resolution.as_str() {
                "resolved" => ContextBindingStatusV1::Resolved,
                "ambiguous" => ContextBindingStatusV1::Ambiguous,
                _ => ContextBindingStatusV1::Unresolved,
            },
        });
    }
    let rule_id = assessment_identity(
        "perseval.no-context-binding-rule.v1",
        &(project_id, logical_trace_id, revision),
    )?;
    let binding = TraceContextBindingV1 {
        schema_version: TRACE_CONTEXT_BINDING_SCHEMA_VERSION.into(),
        target_key: logical_trace_id.into(),
        target_revision: revision.to_string(),
        resolution: TraceContextBindingResolutionV1::Unresolved,
        agent_context_release_id: None,
        binding_rule_release_id: rule_id.clone(),
        binding_provenance: TraceContextBindingProvenanceV1::NoSelectorMatch,
        candidate_context_release_ids: BTreeSet::new(),
    };
    let binding_id = binding
        .binding_id()
        .map_err(|error| StoreError::Invalid(error.to_string()))?;
    transaction.execute(
        "INSERT INTO trace_context_bindings(
            binding_id, project_id, logical_trace_id, revision, resolution,
            context_release_id, binding_rule_release_id, provenance, binding_json,
            created_at_unix_ms
         ) VALUES (?1, ?2, ?3, ?4, 'unresolved', NULL, ?5, 'no_selector_match', ?6, ?7)",
        params![
            binding_id,
            project_id,
            logical_trace_id,
            revision as i64,
            rule_id,
            serde_json::to_string(&binding)?,
            now,
        ],
    )?;
    Ok(BindingRef {
        binding_id,
        context_release_id: None,
        status: ContextBindingStatusV1::Unresolved,
    })
}

fn validate_assessment_commit(
    claim: &ClaimedAssessmentItemV1,
    commit: &AssessmentCommitV1,
) -> Result<(), StoreError> {
    if let Some(evaluation) = &commit.evaluation {
        let catalog = commit.evidence_catalog.as_ref().ok_or_else(|| {
            StoreError::Invalid("typed evaluation commit requires its evidence catalog".into())
        })?;
        evaluation
            .validate_against(catalog)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        if evaluation.evaluator_release_id != claim.evaluator_release_id
            || evaluation.target_key != claim.logical_trace_id
            || evaluation.target_revision != claim.revision.to_string()
            || evaluation.trace_context_binding_id != claim.context_binding_id
            || evaluation.projection_hash != claim.projection_hash
        {
            return Err(StoreError::Invalid(
                "evaluation identity does not match its claimed exact target".into(),
            ));
        }
        let status_matches = matches!(
            (commit.status, evaluation.verdict),
            (
                AssessmentItemStatusV1::Succeeded,
                LearnedVerdictV1::Pass | LearnedVerdictV1::Fail
            ) | (AssessmentItemStatusV1::Abstained, LearnedVerdictV1::Abstain)
                | (
                    AssessmentItemStatusV1::PrivacyBlocked,
                    LearnedVerdictV1::Abstain
                )
                | (
                    AssessmentItemStatusV1::ProviderUnavailable,
                    LearnedVerdictV1::Abstain
                )
                | (
                    AssessmentItemStatusV1::NotApplicable,
                    LearnedVerdictV1::Abstain
                )
        );
        if !status_matches {
            return Err(StoreError::Invalid(
                "assessment terminal status conflicts with evaluation verdict".into(),
            ));
        }
    } else if matches!(
        commit.status,
        AssessmentItemStatusV1::Succeeded | AssessmentItemStatusV1::Abstained
    ) {
        return Err(StoreError::Invalid(
            "succeeded or abstained assessment requires a typed evaluation".into(),
        ));
    }
    if let Some(envelope) = &commit.provider_response {
        envelope
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
    }
    if let Some(failure) = &commit.provider_failure {
        failure
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
    }
    Ok(())
}

fn reserve_budget(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    policy: &ProjectAssessmentPolicyV1,
    reserve: u64,
    now: i64,
) -> Result<bool, StoreError> {
    let day = utc_day(now);
    transaction.execute(
        "INSERT OR IGNORE INTO assessment_daily_budgets(
            project_id, utc_day, reserved_micros, charged_micros, updated_at_unix_ms
         ) VALUES (?1, ?2, 0, 0, ?3)",
        params![project_id, day, now],
    )?;
    let changed = transaction.execute(
        "UPDATE assessment_daily_budgets
         SET reserved_micros = reserved_micros + ?3, updated_at_unix_ms = ?4
         WHERE project_id = ?1 AND utc_day = ?2
           AND reserved_micros + charged_micros + ?3 <= ?5",
        params![
            project_id,
            day,
            reserve as i64,
            now,
            policy.daily_budget_micros as i64,
        ],
    )?;
    Ok(changed == 1)
}

fn release_budget(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    reserved: u64,
    charged: u64,
    now: i64,
) -> Result<(), StoreError> {
    if reserved == 0 && charged == 0 {
        return Ok(());
    }
    transaction.execute(
        "UPDATE assessment_daily_budgets
         SET reserved_micros = MAX(0, reserved_micros - ?3),
             charged_micros = charged_micros + ?4, updated_at_unix_ms = ?5
         WHERE project_id = ?1 AND utc_day = ?2",
        params![
            project_id,
            utc_day(now),
            reserved as i64,
            charged as i64,
            now
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn materialize_cached_assessment(
    transaction: &rusqlite::Transaction<'_>,
    cached: &AssessmentRecordV1,
    item_id: &str,
    job_id: &str,
    project_id: &str,
    logical_trace_id: &str,
    revision: u64,
    evaluator_release_id: &str,
    context_binding_id: &str,
    context_release_id: Option<&str>,
    projection_hash: &str,
    now: i64,
) -> Result<(), StoreError> {
    let assessment_id = assessment_identity(
        "perseval.cached-assessment-record.v1",
        &(item_id, &cached.assessment_id),
    )?;
    let evaluation = cached.evaluation.as_ref();
    transaction.execute(
        "INSERT INTO assessments(
            assessment_id, item_id, project_id, logical_trace_id, revision,
            evaluator_release_id, context_binding_id, context_release_id,
            projection_hash, provider, requested_model, returned_model, status,
            verdict, label, score, confidence, explanation, abstention_reason,
            evaluation_json, cost_micros, latency_ms, created_at_unix_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                   ?14, ?15, ?16, ?17, ?18, ?19, ?20, 0, 0, ?21)",
        params![
            assessment_id,
            item_id,
            project_id,
            logical_trace_id,
            revision as i64,
            evaluator_release_id,
            context_binding_id,
            context_release_id,
            projection_hash,
            cached.provider,
            cached.requested_model,
            cached.returned_model,
            item_status_name(cached.status),
            evaluation.map(|value| verdict_name(value.verdict)),
            evaluation.and_then(|value| value.label.as_deref()),
            evaluation.and_then(|value| value.score),
            evaluation.and_then(|value| value.model_reported_confidence),
            evaluation.map(|value| value.explanation.as_str()),
            evaluation.and_then(|value| value.abstention_reason.map(abstention_name)),
            evaluation.map(serde_json::to_string).transpose()?,
            now,
        ],
    )?;
    if let Some(evaluation) = evaluation {
        for (index, evidence) in evaluation.evidence.iter().enumerate() {
            transaction.execute(
                "INSERT INTO assessment_evidence_refs(
                    assessment_id, evidence_index, evidence_key, evidence_kind,
                    criterion_id, location_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assessment_id,
                    index as i64,
                    evidence.evidence_key,
                    serde_json::to_string(&evidence.evidence_kind)?,
                    evidence.criterion_id,
                    serde_json::to_string(&evidence.location)?,
                ],
            )?;
        }
    }
    finish_item(
        transaction,
        item_id,
        job_id,
        item_status_name(cached.status),
        now,
    )
}

fn finish_without_attempt(
    transaction: &rusqlite::Transaction<'_>,
    item_id: &str,
    job_id: &str,
    status: &str,
    reason: &str,
    now: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE assessment_job_items SET status = ?2, terminal_reason = ?3,
                updated_at_unix_ms = ?4 WHERE item_id = ?1",
        params![item_id, status, reason, now],
    )?;
    refresh_job_counts(transaction, job_id, now)
}

fn finish_item(
    transaction: &rusqlite::Transaction<'_>,
    item_id: &str,
    job_id: &str,
    status: &str,
    now: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE assessment_job_items SET status = ?2, lease_owner = NULL,
                lease_expires_at_unix_ms = NULL, updated_at_unix_ms = ?3
         WHERE item_id = ?1",
        params![item_id, status, now],
    )?;
    refresh_job_counts(transaction, job_id, now)
}

fn refresh_job_counts(
    transaction: &rusqlite::Transaction<'_>,
    job_id: &str,
    now: i64,
) -> Result<(), StoreError> {
    let (total, terminal, failed, cancelled): (i64, i64, i64, i64) = transaction.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN status NOT IN ('pending', 'running') THEN 1 ELSE 0 END),
                SUM(CASE WHEN status IN ('failed', 'budget_blocked', 'privacy_blocked',
                    'provider_unavailable') THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END)
         FROM assessment_job_items WHERE job_id = ?1",
        params![job_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        },
    )?;
    let status = if terminal < total {
        if terminal == 0 { "running" } else { "partial" }
    } else if cancelled == total {
        "cancelled"
    } else if failed > 0 {
        "partial"
    } else {
        "completed"
    };
    transaction.execute(
        "UPDATE assessment_jobs SET terminal_count = ?2, status = ?3,
                updated_at_unix_ms = ?4 WHERE job_id = ?1",
        params![job_id, terminal, status, now],
    )?;
    Ok(())
}

fn load_job_by_idempotency(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    key: &str,
) -> Result<Option<AssessmentJobV1>, StoreError> {
    let job_id = transaction
        .query_row(
            "SELECT job_id FROM assessment_jobs WHERE project_id = ?1 AND idempotency_key = ?2",
            params![project_id, key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    job_id
        .map(|job_id| load_job(transaction, &job_id))
        .transpose()
        .map(Option::flatten)
}

fn load_job(
    transaction: &rusqlite::Transaction<'_>,
    job_id: &str,
) -> Result<Option<AssessmentJobV1>, StoreError> {
    transaction
        .query_row(
            "SELECT project_id, evaluator_release_id, idempotency_key, selection_hash,
                    status, item_count, terminal_count, cancel_requested,
                    created_at_unix_ms, updated_at_unix_ms
             FROM assessment_jobs WHERE job_id = ?1",
            params![job_id],
            |row| {
                let status: String = row.get(4)?;
                Ok(AssessmentJobV1 {
                    schema_version: ASSESSMENT_JOB_SCHEMA_VERSION.into(),
                    job_id: job_id.into(),
                    project_id: row.get(0)?,
                    evaluator_release_id: row.get(1)?,
                    idempotency_key: row.get(2)?,
                    selection_hash: row.get(3)?,
                    status: parse_job_status(&status).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
                        )
                    })?,
                    item_count: row.get::<_, i64>(5)? as u64,
                    terminal_count: row.get::<_, i64>(6)? as u64,
                    cancelled_at_unix_ms: row.get::<_, bool>(7)?.then(|| row.get(9)).transpose()?,
                    created_at_unix_ms: row.get(8)?,
                    updated_at_unix_ms: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn evaluator_provider_model_identity(evaluator: &EvaluatorReleaseSpecV1) -> String {
    match &evaluator.implementation {
        traces_to_evals::EvaluationImplementationV1::PromptJudge {
            provider,
            requested_model,
            ..
        } => format!("{provider}:{requested_model}"),
        traces_to_evals::EvaluationImplementationV1::LocalClassifier {
            model_artifact_id, ..
        } => format!("local:{model_artifact_id}"),
        traces_to_evals::EvaluationImplementationV1::EmbeddingLinear {
            embedding_release_id,
            ..
        } => format!("embedding:{embedding_release_id}"),
        traces_to_evals::EvaluationImplementationV1::Hybrid { .. } => "hybrid".into(),
        traces_to_evals::EvaluationImplementationV1::Ensemble { .. } => "ensemble".into(),
    }
}

fn assessment_identity<T: serde::Serialize>(domain: &str, value: &T) -> Result<String, StoreError> {
    let material = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(material);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn utc_day(unix_ms: i64) -> String {
    // The integer epoch day is timezone-independent and avoids bringing a wall-clock
    // formatting library into the durable accounting boundary.
    format!("epoch-day-{}", unix_ms.div_euclid(86_400_000))
}

fn item_status_name(status: AssessmentItemStatusV1) -> &'static str {
    match status {
        AssessmentItemStatusV1::Pending => "pending",
        AssessmentItemStatusV1::Running => "running",
        AssessmentItemStatusV1::Succeeded => "succeeded",
        AssessmentItemStatusV1::Abstained => "abstained",
        AssessmentItemStatusV1::Failed => "failed",
        AssessmentItemStatusV1::Cancelled => "cancelled",
        AssessmentItemStatusV1::BudgetBlocked => "budget_blocked",
        AssessmentItemStatusV1::PrivacyBlocked => "privacy_blocked",
        AssessmentItemStatusV1::ProviderUnavailable => "provider_unavailable",
        AssessmentItemStatusV1::NotApplicable => "not_applicable",
    }
}

fn parse_item_status(value: &str) -> Result<AssessmentItemStatusV1, String> {
    match value {
        "pending" => Ok(AssessmentItemStatusV1::Pending),
        "running" => Ok(AssessmentItemStatusV1::Running),
        "succeeded" => Ok(AssessmentItemStatusV1::Succeeded),
        "abstained" => Ok(AssessmentItemStatusV1::Abstained),
        "failed" => Ok(AssessmentItemStatusV1::Failed),
        "cancelled" => Ok(AssessmentItemStatusV1::Cancelled),
        "budget_blocked" => Ok(AssessmentItemStatusV1::BudgetBlocked),
        "privacy_blocked" => Ok(AssessmentItemStatusV1::PrivacyBlocked),
        "provider_unavailable" => Ok(AssessmentItemStatusV1::ProviderUnavailable),
        "not_applicable" => Ok(AssessmentItemStatusV1::NotApplicable),
        other => Err(format!("unknown assessment item status {other}")),
    }
}

fn parse_job_status(value: &str) -> Result<AssessmentJobStatusV1, String> {
    match value {
        "pending" => Ok(AssessmentJobStatusV1::Pending),
        "running" => Ok(AssessmentJobStatusV1::Running),
        "completed" => Ok(AssessmentJobStatusV1::Completed),
        "partial" => Ok(AssessmentJobStatusV1::Partial),
        "cancelled" => Ok(AssessmentJobStatusV1::Cancelled),
        "failed" => Ok(AssessmentJobStatusV1::Failed),
        other => Err(format!("unknown assessment job status {other}")),
    }
}

fn verdict_name(verdict: LearnedVerdictV1) -> &'static str {
    match verdict {
        LearnedVerdictV1::Pass => "pass",
        LearnedVerdictV1::Fail => "fail",
        LearnedVerdictV1::Abstain => "abstain",
    }
}

fn abstention_name(reason: LearnedAbstentionReasonV1) -> &'static str {
    match reason {
        LearnedAbstentionReasonV1::ContextUnresolved => "context_unresolved",
        LearnedAbstentionReasonV1::ContextInsufficient => "context_insufficient",
        LearnedAbstentionReasonV1::ContentUnavailable => "content_unavailable",
        LearnedAbstentionReasonV1::ContentTruncated => "content_truncated",
        LearnedAbstentionReasonV1::PrivacyBlocked => "privacy_blocked",
        LearnedAbstentionReasonV1::EvidenceInsufficient => "evidence_insufficient",
        LearnedAbstentionReasonV1::OutOfDistribution => "out_of_distribution",
        LearnedAbstentionReasonV1::ProviderUnavailable => "provider_unavailable",
        LearnedAbstentionReasonV1::InvalidProviderOutput => "invalid_provider_output",
        LearnedAbstentionReasonV1::NotApplicable => "not_applicable",
    }
}

use super::*;

struct StoredEvalCandidateRow {
    project_id: String,
    group_id: String,
    finding_id: String,
    logical_trace_id: String,
    revision: i64,
    candidate_json: String,
    packet_json: String,
    disposition: String,
    deferred_reason: Option<String>,
    created_at_unix_ms: i64,
    updated_at_unix_ms: i64,
}

fn map_eval_candidate_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvalCandidateRow> {
    Ok(StoredEvalCandidateRow {
        project_id: row.get(0)?,
        group_id: row.get(1)?,
        finding_id: row.get(2)?,
        logical_trace_id: row.get(3)?,
        revision: row.get(4)?,
        candidate_json: row.get(5)?,
        packet_json: row.get(6)?,
        disposition: row.get(7)?,
        deferred_reason: row.get(8)?,
        created_at_unix_ms: row.get(9)?,
        updated_at_unix_ms: row.get(10)?,
    })
}

fn decode_eval_candidate_row(
    row: StoredEvalCandidateRow,
) -> Result<EvalCandidateRecordV1, StoreError> {
    let candidate: EvalCandidate = serde_json::from_str(&row.candidate_json)?;
    let queue_state = match candidate.status {
        EvalCandidateStatus::Candidate if row.disposition == "deferred" => {
            EvalReviewQueueStateV1::Deferred
        }
        EvalCandidateStatus::Candidate | EvalCandidateStatus::Reviewed => {
            EvalReviewQueueStateV1::Pending
        }
        EvalCandidateStatus::Accepted => EvalReviewQueueStateV1::Accepted,
        EvalCandidateStatus::Rejected => EvalReviewQueueStateV1::Rejected,
        EvalCandidateStatus::Superseded => EvalReviewQueueStateV1::Superseded,
    };
    Ok(EvalCandidateRecordV1 {
        schema_version: EVAL_CANDIDATE_RECORD_SCHEMA_VERSION.into(),
        project_id: row.project_id,
        group_id: row.group_id,
        finding_id: row.finding_id,
        logical_trace_id: row.logical_trace_id,
        revision: row.revision as u64,
        candidate,
        evidence_packet: serde_json::from_str(&row.packet_json)?,
        queue_state,
        deferred_reason: row.deferred_reason,
        created_at_unix_ms: row.created_at_unix_ms,
        updated_at_unix_ms: row.updated_at_unix_ms,
    })
}

impl WorkspaceStore {
    pub fn preview_eval_batch(
        &self,
        project_id: &str,
        selection_spec: &EvalBatchSelectionSpecV1,
    ) -> Result<EvalBatchPreviewV1, StoreError> {
        validate_eval_batch_request(self, project_id, selection_spec)?;
        let mut normalized_spec = selection_spec.clone();
        normalized_spec.group_ids.sort();
        normalized_spec.group_ids.dedup();

        let scoped_trace_ids = self
            .list_runs_filtered(
                &RunFiltersV1 {
                    scope: normalized_spec.scope.clone(),
                    ..RunFiltersV1::default()
                },
                0,
                u32::MAX,
            )?
            .into_iter()
            .map(|run| run.logical_trace_id)
            .collect::<HashSet<_>>();
        let analyses = self
            .load_active_analyses()?
            .into_iter()
            .filter(|analysis| scoped_trace_ids.contains(&analysis.logical_trace_id))
            .collect::<Vec<_>>();
        let maximum_per_group = normalized_spec
            .policy
            .maximum_examples_per_group
            .clamp(1, 16) as usize;
        let maximum_candidate_count = maximum_per_group
            .saturating_mul(normalized_spec.group_ids.len())
            .min(256) as u32;
        let mut items = Vec::new();
        let mut exclusions = Vec::new();

        for group_id in &normalized_spec.group_ids {
            let filters = FailureFiltersV1 {
                scope: normalized_spec.scope.clone(),
                ..FailureFiltersV1::default()
            };
            let summary = self
                .list_failure_groups(&filters, 0, 200)?
                .into_iter()
                .find(|group| group.group_id == *group_id);
            let Some(summary) = summary else {
                exclusions.push(EvalBatchExclusionV1 {
                    group_id: group_id.clone(),
                    finding_id: None,
                    reason: "Group is not present in the selected project snapshot.".into(),
                });
                continue;
            };
            let members = analyses
                .iter()
                .flat_map(|analysis| {
                    analysis
                        .findings
                        .iter()
                        .filter(|finding| finding.failure_signature == summary.failure_signature)
                        .map(move |finding| (analysis, finding))
                })
                .collect::<Vec<_>>();
            let selected =
                select_eval_representatives(&members, &normalized_spec.policy, maximum_per_group);
            if selected.is_empty() {
                exclusions.push(EvalBatchExclusionV1 {
                    group_id: group_id.clone(),
                    finding_id: None,
                    reason: "No concrete analyzed finding remains in this group.".into(),
                });
                continue;
            }
            for (analysis, finding, selection_reason) in selected {
                let Some((_, _, evidence_packet, candidate)) =
                    candidate_parts(&analyses, &finding.finding_id)
                else {
                    exclusions.push(EvalBatchExclusionV1 {
                        group_id: group_id.clone(),
                        finding_id: Some(finding.finding_id.clone()),
                        reason: "Candidate evidence could not be reconstructed.".into(),
                    });
                    continue;
                };
                let run = self.get_run(&analysis.logical_trace_id)?;
                items.push(EvalBatchItemPreviewV1 {
                    project_id: project_id.to_string(),
                    group_id: group_id.clone(),
                    finding_id: finding.finding_id.clone(),
                    logical_trace_id: analysis.logical_trace_id.clone(),
                    revision: analysis.revision,
                    run_title: run.as_ref().map(|run| run.title.clone()),
                    build_id: run.as_ref().and_then(|run| run.build_id.clone()),
                    session_id: run.as_ref().and_then(|run| run.session_id.clone()),
                    recovery: Some(finding.recovery),
                    selection_reason,
                    telemetry_gaps: evidence_packet.telemetry_gaps.clone(),
                    already_exists: self.load_candidate(&finding.finding_id)?.is_some(),
                    candidate,
                    evidence_packet,
                });
            }
        }

        let selection_hash = eval_batch_selection_hash(project_id, &normalized_spec, &items)?;
        let preview_id = format!("eval-preview:{selection_hash}");
        let preview = EvalBatchPreviewV1 {
            schema_version: EVAL_BATCH_PREVIEW_SCHEMA_VERSION.into(),
            preview_id: preview_id.clone(),
            project_id: project_id.to_string(),
            selection_hash: selection_hash.clone(),
            selection_spec: normalized_spec,
            maximum_candidate_count,
            items,
            exclusions,
            created_at_unix_ms: now_unix_ms(),
        };
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "INSERT OR IGNORE INTO eval_batch_previews(
                preview_id, project_id, selection_hash, preview_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                preview.preview_id,
                preview.project_id,
                preview.selection_hash,
                serde_json::to_string(&preview)?,
                preview.created_at_unix_ms
            ],
        )?;
        Ok(preview)
    }

    pub fn create_eval_batch(
        &self,
        project_id: &str,
        preview_id: &str,
        selection_hash: &str,
        idempotency_key: &str,
    ) -> Result<CandidateGenerationJobV1, StoreError> {
        let job = self.queue_eval_batch(project_id, preview_id, selection_hash, idempotency_key)?;
        self.execute_candidate_generation_job(&job.job_id)
    }

    pub fn queue_eval_batch(
        &self,
        project_id: &str,
        preview_id: &str,
        selection_hash: &str,
        idempotency_key: &str,
    ) -> Result<CandidateGenerationJobV1, StoreError> {
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 160 {
            return Err(StoreError::Invalid(
                "idempotency_key must contain between 1 and 160 characters".into(),
            ));
        }
        if let Some(existing) = self.load_candidate_generation_job(project_id, idempotency_key)? {
            if existing.preview_id != preview_id || existing.selection_hash != selection_hash {
                return Err(StoreError::Invalid(
                    "idempotency key is already bound to a different eval batch".into(),
                ));
            }
            if !matches!(
                existing.status,
                CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
            ) {
                return Ok(existing);
            }
            return Ok(existing);
        }
        let stored_preview = self.load_eval_batch_preview(preview_id)?.ok_or_else(|| {
            StoreError::Invalid("eval batch preview does not exist or has expired".into())
        })?;
        if stored_preview.project_id != project_id {
            return Err(StoreError::Invalid(
                "eval batch preview belongs to a different project".into(),
            ));
        }
        if stored_preview.selection_hash != selection_hash {
            return Err(StoreError::Invalid(
                "selection hash does not match the preview".into(),
            ));
        }
        let current = self.preview_eval_batch(project_id, &stored_preview.selection_spec)?;
        if current.selection_hash != selection_hash {
            return Err(StoreError::Invalid(
                "eval batch preview is stale; review the current representatives".into(),
            ));
        }

        let now = now_unix_ms();
        let job_id = candidate_generation_job_id(project_id, idempotency_key);
        let job = CandidateGenerationJobV1 {
            schema_version: CANDIDATE_GENERATION_JOB_SCHEMA_VERSION.into(),
            job_id,
            project_id: project_id.to_string(),
            preview_id: preview_id.to_string(),
            selection_hash: selection_hash.to_string(),
            idempotency_key: idempotency_key.to_string(),
            status: CandidateGenerationJobStatusV1::Queued,
            outcomes: Vec::new(),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        };
        self.persist_candidate_generation_job(&job)?;
        Ok(job)
    }

    pub fn execute_candidate_generation_job(
        &self,
        job_id: &str,
    ) -> Result<CandidateGenerationJobV1, StoreError> {
        let mut job = self
            .get_candidate_generation_job(job_id)?
            .ok_or_else(|| StoreError::Invalid("candidate generation job does not exist".into()))?;
        if !matches!(
            job.status,
            CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
        ) {
            return Ok(job);
        }
        let stored_preview = self
            .load_eval_batch_preview(&job.preview_id)?
            .ok_or_else(|| StoreError::Invalid("eval batch preview does not exist".into()))?;
        let current = self.preview_eval_batch(&job.project_id, &stored_preview.selection_spec)?;
        if current.selection_hash != job.selection_hash {
            job.status = CandidateGenerationJobStatusV1::Failed;
            job.updated_at_unix_ms = now_unix_ms();
            self.persist_candidate_generation_job(&job)?;
            return Ok(job);
        }
        job.status = CandidateGenerationJobStatusV1::Running;
        job.updated_at_unix_ms = now_unix_ms();
        self.persist_candidate_generation_job(&job)?;

        for item in current.items {
            if job
                .outcomes
                .iter()
                .any(|outcome| outcome.finding_id == item.finding_id)
            {
                continue;
            }
            if self
                .get_candidate_generation_job(job_id)?
                .is_some_and(|current| current.status == CandidateGenerationJobStatusV1::Cancelled)
            {
                job.status = CandidateGenerationJobStatusV1::Cancelled;
                job.updated_at_unix_ms = now_unix_ms();
                self.persist_candidate_generation_job(&job)?;
                return Ok(job);
            }
            let existing = self.load_candidate(&item.finding_id)?;
            match self.create_eval_candidate(&item.group_id, &item.finding_id) {
                Ok(Some(candidate)) => job.outcomes.push(CandidateGenerationItemOutcomeV1 {
                    project_id: job.project_id.clone(),
                    group_id: item.group_id,
                    finding_id: item.finding_id,
                    candidate_id: Some(candidate.candidate_id),
                    outcome: if existing.is_some() {
                        CandidateGenerationOutcomeKindV1::AlreadyExists
                    } else {
                        CandidateGenerationOutcomeKindV1::Created
                    },
                    message: None,
                }),
                Ok(None) => job.outcomes.push(CandidateGenerationItemOutcomeV1 {
                    project_id: job.project_id.clone(),
                    group_id: item.group_id,
                    finding_id: item.finding_id,
                    candidate_id: None,
                    outcome: CandidateGenerationOutcomeKindV1::Skipped,
                    message: Some("Finding is no longer available in the active revision.".into()),
                }),
                Err(error) => job.outcomes.push(CandidateGenerationItemOutcomeV1 {
                    project_id: job.project_id.clone(),
                    group_id: item.group_id,
                    finding_id: item.finding_id,
                    candidate_id: None,
                    outcome: CandidateGenerationOutcomeKindV1::Failed,
                    message: Some(error.to_string()),
                }),
            }
            job.updated_at_unix_ms = now_unix_ms();
            if self
                .get_candidate_generation_job(job_id)?
                .is_some_and(|current| current.status == CandidateGenerationJobStatusV1::Cancelled)
            {
                job.status = CandidateGenerationJobStatusV1::Cancelled;
                self.persist_candidate_generation_job(&job)?;
                return Ok(job);
            }
            self.persist_candidate_generation_job(&job)?;
        }
        let failures = job
            .outcomes
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome.outcome,
                    CandidateGenerationOutcomeKindV1::Skipped
                        | CandidateGenerationOutcomeKindV1::Failed
                )
            })
            .count();
        job.status = if failures == 0 {
            CandidateGenerationJobStatusV1::Succeeded
        } else if failures == job.outcomes.len() {
            CandidateGenerationJobStatusV1::Failed
        } else {
            CandidateGenerationJobStatusV1::PartialSuccess
        };
        job.updated_at_unix_ms = now_unix_ms();
        self.persist_candidate_generation_job(&job)?;
        Ok(job)
    }

    pub fn get_candidate_generation_job(
        &self,
        job_id: &str,
    ) -> Result<Option<CandidateGenerationJobV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let json = control
            .query_row(
                "SELECT job_json FROM candidate_generation_jobs WHERE job_id = ?1",
                params![job_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    pub fn cancel_candidate_generation_job(
        &self,
        job_id: &str,
    ) -> Result<CandidateGenerationJobV1, StoreError> {
        let mut job = self
            .get_candidate_generation_job(job_id)?
            .ok_or_else(|| StoreError::Invalid("candidate generation job does not exist".into()))?;
        if matches!(
            job.status,
            CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
        ) {
            job.status = CandidateGenerationJobStatusV1::Cancelled;
            job.updated_at_unix_ms = now_unix_ms();
            self.persist_candidate_generation_job(&job)?;
        }
        Ok(job)
    }

    pub fn retry_candidate_generation_job(
        &self,
        job_id: &str,
    ) -> Result<CandidateGenerationJobV1, StoreError> {
        let mut job = self
            .get_candidate_generation_job(job_id)?
            .ok_or_else(|| StoreError::Invalid("candidate generation job does not exist".into()))?;
        if matches!(
            job.status,
            CandidateGenerationJobStatusV1::Failed
                | CandidateGenerationJobStatusV1::PartialSuccess
                | CandidateGenerationJobStatusV1::Cancelled
        ) {
            job.outcomes.retain(|outcome| {
                matches!(
                    outcome.outcome,
                    CandidateGenerationOutcomeKindV1::Created
                        | CandidateGenerationOutcomeKindV1::AlreadyExists
                )
            });
            job.status = CandidateGenerationJobStatusV1::Queued;
            job.updated_at_unix_ms = now_unix_ms();
            self.persist_candidate_generation_job(&job)?;
        }
        Ok(job)
    }

    pub fn pending_candidate_generation_job_ids(&self) -> Result<Vec<String>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT job_id FROM candidate_generation_jobs
             WHERE status IN ('queued', 'running') ORDER BY created_at_unix_ms, job_id",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    fn load_eval_batch_preview(
        &self,
        preview_id: &str,
    ) -> Result<Option<EvalBatchPreviewV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let json = control
            .query_row(
                "SELECT preview_json FROM eval_batch_previews WHERE preview_id = ?1",
                params![preview_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    fn load_candidate_generation_job(
        &self,
        project_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<CandidateGenerationJobV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let json = control
            .query_row(
                "SELECT job_json FROM candidate_generation_jobs
                 WHERE project_id = ?1 AND idempotency_key = ?2",
                params![project_id, idempotency_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    fn persist_candidate_generation_job(
        &self,
        job: &CandidateGenerationJobV1,
    ) -> Result<(), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "INSERT INTO candidate_generation_jobs(
                job_id, project_id, preview_id, selection_hash, idempotency_key, status,
                job_json, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(project_id, idempotency_key) DO UPDATE SET
                status = excluded.status, job_json = excluded.job_json,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                job.job_id,
                job.project_id,
                job.preview_id,
                job.selection_hash,
                job.idempotency_key,
                format!("{:?}", job.status).to_ascii_lowercase(),
                serde_json::to_string(job)?,
                job.created_at_unix_ms,
                job.updated_at_unix_ms
            ],
        )?;
        Ok(())
    }

    pub fn create_eval_candidate(
        &self,
        group_id: &str,
        finding_id: &str,
    ) -> Result<Option<EvalCandidate>, StoreError> {
        if let Some(candidate) = self.load_candidate(finding_id)? {
            return Ok(Some(candidate));
        }
        let analyses = self.load_active_analyses()?;
        let Some((analysis, _finding, packet, candidate)) = candidate_parts(&analyses, finding_id)
        else {
            return Ok(None);
        };
        let control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.unchecked_transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO evidence_packets VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                packet.packet_id,
                analysis.logical_trace_id,
                analysis.revision as i64,
                finding_id,
                serde_json::to_string(&packet)?,
                now_unix_ms()
            ],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO eval_candidates VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                candidate.candidate_id,
                analysis.logical_trace_id,
                analysis.revision as i64,
                finding_id,
                group_id,
                packet.packet_id,
                serde_json::to_string(&candidate)?,
                now_unix_ms()
            ],
        )?;
        transaction.commit()?;
        Ok(Some(candidate))
    }

    pub fn preview_eval_candidate(
        &self,
        finding_id: &str,
    ) -> Result<Option<EvalCandidatePreview>, StoreError> {
        if let Some(candidate) = self.load_candidate(finding_id)? {
            let control = self.control.lock().expect("control store lock poisoned");
            let packet_json = control
                .query_row(
                    "SELECT packet_json FROM evidence_packets WHERE packet_id = ?1",
                    params![candidate.evidence_packet_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            return packet_json
                .map(|json| {
                    Ok(EvalCandidatePreview {
                        evidence_packet: serde_json::from_str(&json)?,
                        candidate,
                    })
                })
                .transpose();
        }
        let analyses = self.load_active_analyses()?;
        let Some((_, _, evidence_packet, candidate)) = candidate_parts(&analyses, finding_id)
        else {
            return Ok(None);
        };
        Ok(Some(EvalCandidatePreview {
            evidence_packet,
            candidate,
        }))
    }

    pub fn list_eval_candidates(
        &self,
        project_id: Option<&str>,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<EvalCandidateRecordV1>, StoreError> {
        let limit = limit.min(200);
        let control = self.control.lock().expect("control store lock poisoned");
        let select = "SELECT t.project_id, c.group_id, c.finding_id, c.logical_trace_id,
                             c.revision, c.candidate_json, e.packet_json,
                             COALESCE(d.state, 'pending'), d.reason, c.created_at_unix_ms,
                             COALESCE(d.updated_at_unix_ms, c.created_at_unix_ms)
                      FROM eval_candidates c
                      JOIN logical_traces t ON t.logical_trace_id = c.logical_trace_id
                      JOIN evidence_packets e ON e.packet_id = c.evidence_packet_id
                      LEFT JOIN eval_candidate_dispositions d ON d.candidate_id = c.candidate_id";
        let rows = if let Some(project_id) = project_id {
            let sql = format!(
                "{select} WHERE t.workspace_id = ?1 AND t.project_id = ?2
                 ORDER BY c.created_at_unix_ms DESC, c.candidate_id LIMIT ?3 OFFSET ?4"
            );
            let mut statement = control.prepare(&sql)?;
            statement
                .query_map(
                    params![self.workspace_id, project_id, limit as i64, offset as i64],
                    map_eval_candidate_row,
                )?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let sql = format!(
                "{select} WHERE t.workspace_id = ?1
                 ORDER BY c.created_at_unix_ms DESC, c.candidate_id LIMIT ?2 OFFSET ?3"
            );
            let mut statement = control.prepare(&sql)?;
            statement
                .query_map(
                    params![self.workspace_id, limit as i64, offset as i64],
                    map_eval_candidate_row,
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        rows.into_iter().map(decode_eval_candidate_row).collect()
    }

    pub fn get_eval_candidate(
        &self,
        project_id: &str,
        candidate_id: &str,
    ) -> Result<Option<EvalCandidateRecordV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let row = control
            .query_row(
                "SELECT t.project_id, c.group_id, c.finding_id, c.logical_trace_id,
                        c.revision, c.candidate_json, e.packet_json,
                        COALESCE(d.state, 'pending'), d.reason, c.created_at_unix_ms,
                        COALESCE(d.updated_at_unix_ms, c.created_at_unix_ms)
                 FROM eval_candidates c
                 JOIN logical_traces t ON t.logical_trace_id = c.logical_trace_id
                 JOIN evidence_packets e ON e.packet_id = c.evidence_packet_id
                 LEFT JOIN eval_candidate_dispositions d ON d.candidate_id = c.candidate_id
                 WHERE t.workspace_id = ?1 AND t.project_id = ?2 AND c.candidate_id = ?3",
                params![self.workspace_id, project_id, candidate_id],
                map_eval_candidate_row,
            )
            .optional()?;
        row.map(decode_eval_candidate_row).transpose()
    }

    pub fn review_eval_candidate(
        &self,
        request: &ReviewEvalCandidateV1,
    ) -> Result<EvalCandidateRecordV1, StoreError> {
        if request.project_id.trim().is_empty() || request.project_id == UNASSIGNED_PROJECT_ID {
            return Err(StoreError::Invalid(
                "eval review requires one persisted project".into(),
            ));
        }
        if request.reviewer_ref.trim().is_empty() || request.reviewed_at.trim().is_empty() {
            return Err(StoreError::Invalid(
                "reviewer_ref and reviewed_at are required".into(),
            ));
        }
        let record = self
            .get_eval_candidate(&request.project_id, &request.candidate_id)?
            .ok_or_else(|| {
                StoreError::Invalid("candidate does not exist in the selected project".into())
            })?;
        if matches!(request.decision, EvalReviewDecisionV1::Defer) {
            if !matches!(record.candidate.status, EvalCandidateStatus::Candidate) {
                return Err(StoreError::Invalid(
                    "only an unreviewed candidate can be deferred".into(),
                ));
            }
            let control = self.control.lock().expect("control store lock poisoned");
            control.execute(
                "INSERT INTO eval_candidate_dispositions(
                    candidate_id, project_id, state, reviewer_ref, reason, updated_at_unix_ms
                 ) VALUES (?1, ?2, 'deferred', ?3, ?4, ?5)
                 ON CONFLICT(candidate_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    state = excluded.state,
                    reviewer_ref = excluded.reviewer_ref,
                    reason = excluded.reason,
                    updated_at_unix_ms = excluded.updated_at_unix_ms",
                params![
                    request.candidate_id,
                    request.project_id,
                    request.reviewer_ref,
                    request.reason,
                    now_unix_ms()
                ],
            )?;
            drop(control);
            return self
                .get_eval_candidate(&request.project_id, &request.candidate_id)?
                .ok_or_else(|| StoreError::Invalid("reviewed candidate disappeared".into()));
        }

        let expected_status = match request.decision {
            EvalReviewDecisionV1::Accept => EvalCandidateStatus::Accepted,
            EvalReviewDecisionV1::Reject => EvalCandidateStatus::Rejected,
            EvalReviewDecisionV1::Defer => unreachable!(),
        };
        if record.candidate.status == expected_status {
            return Ok(record);
        }
        if !matches!(record.candidate.status, EvalCandidateStatus::Candidate) {
            return Err(StoreError::Invalid(format!(
                "candidate is already in {:?} state",
                record.candidate.status
            )));
        }
        let decision = match request.decision {
            EvalReviewDecisionV1::Accept => CandidateReviewDecision::Approve,
            EvalReviewDecisionV1::Reject => CandidateReviewDecision::Reject,
            EvalReviewDecisionV1::Defer => unreachable!(),
        };
        let candidate = record
            .candidate
            .record_review(CandidateReview {
                reviewer_ref: request.reviewer_ref.clone(),
                reviewed_at: request.reviewed_at.clone(),
                decision,
                reason: request.reason.clone(),
            })
            .and_then(EvalCandidate::resolve_review)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.unchecked_transaction()?;
        transaction.execute(
            "UPDATE eval_candidates SET candidate_json = ?1 WHERE candidate_id = ?2",
            params![serde_json::to_string(&candidate)?, request.candidate_id],
        )?;
        transaction.execute(
            "DELETE FROM eval_candidate_dispositions WHERE candidate_id = ?1",
            params![request.candidate_id],
        )?;
        transaction.commit()?;
        drop(control);
        self.get_eval_candidate(&request.project_id, &request.candidate_id)?
            .ok_or_else(|| StoreError::Invalid("reviewed candidate disappeared".into()))
    }
}

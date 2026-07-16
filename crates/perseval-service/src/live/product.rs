use super::*;

impl LiveTraceService {
    pub fn list_failure_groups(
        &self,
        filters: &perseval_store::FailureFiltersV1,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<perseval_store::FailureGroupSummary>, LiveServiceError> {
        Ok(self
            .store
            .list_failure_groups(filters, offset, limit.min(200))?)
    }

    pub fn list_failure_group_page(
        &self,
        filters: &perseval_store::FailureFiltersV1,
        offset: u64,
        limit: u32,
    ) -> Result<perseval_store::FailureGroupPageV1, LiveServiceError> {
        Ok(self
            .store
            .list_failure_group_page(filters, offset, limit.clamp(1, 200))?)
    }

    pub fn has_active_findings(&self, project_id: Option<&str>) -> Result<bool, LiveServiceError> {
        Ok(self.store.has_active_findings(project_id)?)
    }

    pub fn failure_filter_options(&self) -> Result<(Vec<String>, Vec<String>), LiveServiceError> {
        Ok(self.store.failure_filter_options()?)
    }

    pub fn get_failure_group(
        &self,
        group_id: &str,
    ) -> Result<Option<perseval_store::FailureGroupDetail>, LiveServiceError> {
        Ok(self.store.get_failure_group(group_id)?)
    }

    pub fn get_failure_group_for_project(
        &self,
        project_id: &str,
        group_id: &str,
    ) -> Result<Option<perseval_store::FailureGroupDetail>, LiveServiceError> {
        Ok(self
            .store
            .get_failure_group_for_project(project_id, group_id)?)
    }

    pub fn get_failure_group_in_scope(
        &self,
        scope: &perseval_store::QueryScopeV1,
        group_id: &str,
    ) -> Result<Option<perseval_store::FailureGroupDetail>, LiveServiceError> {
        Ok(self.store.get_failure_group_in_scope(scope, group_id)?)
    }

    pub fn list_failure_occurrences(
        &self,
        group_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<perseval_store::FailureOccurrence>, LiveServiceError> {
        Ok(self
            .store
            .list_failure_occurrences(group_id, offset, limit.min(200))?)
    }

    pub fn list_failure_occurrences_for_project(
        &self,
        project_id: &str,
        group_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<perseval_store::FailureOccurrence>, LiveServiceError> {
        Ok(self.store.list_failure_occurrences_for_project(
            project_id,
            group_id,
            offset,
            limit.min(200),
        )?)
    }

    pub fn list_failure_occurrences_in_scope(
        &self,
        scope: &perseval_store::QueryScopeV1,
        group_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<perseval_store::FailureOccurrence>, LiveServiceError> {
        Ok(self
            .store
            .list_failure_occurrences_in_scope(scope, group_id, offset, limit.min(200))?)
    }

    pub fn get_finding_evidence(
        &self,
        group_id: &str,
        finding_id: &str,
    ) -> Result<Option<perseval_store::FindingEvidence>, LiveServiceError> {
        Ok(self.store.get_finding_evidence(group_id, finding_id, 128)?)
    }

    pub fn set_finding_disposition(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
        finding_id: &str,
        state: FindingDispositionStateV1,
    ) -> Result<FindingDispositionV1, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::SetFindingDisposition {
                scope: scope.clone(),
                group_id: group_id.to_owned(),
                finding_id: finding_id.to_owned(),
                state,
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn undo_finding_disposition(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
        finding_id: &str,
    ) -> Result<bool, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::UndoFindingDisposition {
                scope: scope.clone(),
                group_id: group_id.to_owned(),
                finding_id: finding_id.to_owned(),
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn get_finding_evidence_for_project(
        &self,
        project_id: &str,
        group_id: &str,
        finding_id: &str,
    ) -> Result<Option<perseval_store::FindingEvidence>, LiveServiceError> {
        Ok(self
            .store
            .get_finding_evidence_for_project(project_id, group_id, finding_id, 128)?)
    }

    pub fn get_finding_evidence_in_scope(
        &self,
        scope: &perseval_store::QueryScopeV1,
        group_id: &str,
        finding_id: &str,
    ) -> Result<Option<perseval_store::FindingEvidence>, LiveServiceError> {
        Ok(self
            .store
            .get_finding_evidence_in_scope(scope, group_id, finding_id, 128)?)
    }

    pub fn create_eval_candidate(
        &self,
        group_id: &str,
        finding_id: &str,
    ) -> Result<Option<traces_to_evals::EvalCandidate>, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::CreateEvalCandidate {
                group_id: group_id.to_owned(),
                finding_id: finding_id.to_owned(),
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn preview_eval_candidate(
        &self,
        finding_id: &str,
    ) -> Result<Option<perseval_store::EvalCandidatePreview>, LiveServiceError> {
        Ok(self.store.preview_eval_candidate(finding_id)?)
    }

    pub fn preview_eval_batch(
        &self,
        project_id: &str,
        selection_spec: &EvalBatchSelectionSpecV1,
    ) -> Result<EvalBatchPreviewV1, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::PreviewEvalBatch {
                project_id: project_id.to_owned(),
                selection_spec: selection_spec.clone(),
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv_timeout(Duration::from_millis(
                self.config.lifecycle.shutdown_drain_ms,
            ))
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn create_eval_batch(
        &self,
        project_id: &str,
        preview_id: &str,
        selection_hash: &str,
        idempotency_key: &str,
    ) -> Result<CandidateGenerationJobV1, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::CreateEvalBatch {
                project_id: project_id.to_owned(),
                preview_id: preview_id.to_owned(),
                selection_hash: selection_hash.to_owned(),
                idempotency_key: idempotency_key.to_owned(),
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn get_candidate_generation_job(
        &self,
        job_id: &str,
    ) -> Result<Option<CandidateGenerationJobV1>, LiveServiceError> {
        Ok(self.store.get_candidate_generation_job(job_id)?)
    }

    pub fn cancel_eval_batch(
        &self,
        job_id: &str,
    ) -> Result<CandidateGenerationJobV1, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::CancelEvalBatch {
                job_id: job_id.to_owned(),
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn retry_eval_batch(
        &self,
        job_id: &str,
    ) -> Result<CandidateGenerationJobV1, LiveServiceError> {
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::RetryEvalBatch {
                job_id: job_id.to_owned(),
                response,
            })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }

    pub fn list_eval_candidates(
        &self,
        project_id: Option<&str>,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<EvalCandidateRecordV1>, LiveServiceError> {
        Ok(self
            .store
            .list_eval_candidates(project_id, offset, limit.min(200))?)
    }

    pub fn get_eval_candidate(
        &self,
        project_id: &str,
        candidate_id: &str,
    ) -> Result<Option<EvalCandidateRecordV1>, LiveServiceError> {
        Ok(self.store.get_eval_candidate(project_id, candidate_id)?)
    }

    pub fn review_eval_candidate(
        &self,
        project_id: &str,
        candidate_id: &str,
        decision: EvalReviewDecisionV1,
        reason: Option<String>,
    ) -> Result<EvalCandidateRecordV1, LiveServiceError> {
        let request = ReviewEvalCandidateV1 {
            project_id: project_id.to_owned(),
            candidate_id: candidate_id.to_owned(),
            decision,
            reviewer_ref: self.config.reviewer_ref.clone(),
            reviewed_at: chrono::Utc::now().to_rfc3339(),
            reason,
        };
        let (response, receiver) = mpsc::channel();
        self.ingest
            .shared
            .sender
            .try_send(WriterCommand::ReviewEvalCandidate { request, response })
            .map_err(|_| LiveServiceError::WriterUnavailable)?;
        receiver
            .recv()
            .map_err(|_| LiveServiceError::WriterUnavailable)?
            .map_err(LiveServiceError::Writer)
    }
}

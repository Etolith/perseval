use traces_to_evals::{EvaluatorReleaseSpecV1, TaskCompletionProjectionV1, TraceContextBindingV1};

use super::{StoreError, WorkspaceStore};
use crate::model::{
    AssessmentCommitV1, AssessmentRecordV1, ClaimedAssessmentItemV1, ReviewModeV1, ReviewQueueV1,
    ReviewSplitReleaseV1, ReviewTaskV1, TaskCompletionReleaseConfigV1,
};

/// Exact immutable inputs needed by one task-completion assessment attempt.
pub struct AssessmentExecutionInputsV1 {
    pub evaluator: EvaluatorReleaseSpecV1,
    pub config: TaskCompletionReleaseConfigV1,
    pub projection: TaskCompletionProjectionV1,
    pub binding: TraceContextBindingV1,
}

/// Persistence boundary for learned assessment scheduling and artifacts.
/// Existing `WorkspaceStore` methods remain available as compatibility
/// re-exports while services migrate to this narrower repository.
pub struct AssessmentRepository<'a> {
    store: &'a WorkspaceStore,
}

impl AssessmentRepository<'_> {
    pub fn execution_inputs(
        &self,
        claim: &ClaimedAssessmentItemV1,
    ) -> Result<AssessmentExecutionInputsV1, StoreError> {
        let evaluator = self
            .store
            .evaluator_release(&claim.project_id, &claim.evaluator_release_id)?
            .ok_or_else(|| StoreError::Invalid("exact evaluator release is unavailable".into()))?;
        let config = self
            .store
            .task_completion_release_config(&claim.project_id, &claim.evaluator_release_id)?
            .ok_or_else(|| {
                StoreError::Invalid("task-completion execution configuration is unavailable".into())
            })?;
        let projection = self
            .store
            .load_task_completion_projection(&claim.project_id, &claim.projection_hash)?
            .ok_or_else(|| {
                StoreError::Invalid("exact task-completion projection is unavailable".into())
            })?;
        let binding = self
            .store
            .trace_context_binding(&claim.project_id, &claim.context_binding_id)?
            .ok_or_else(|| {
                StoreError::Invalid("exact trace-context binding is unavailable".into())
            })?;
        if config.evaluator_release_id != claim.evaluator_release_id
            || projection.target_key != claim.logical_trace_id
            || projection.target_revision != claim.revision.to_string()
            || projection.projection_hash != claim.projection_hash
            || projection.trace_context_binding_id != claim.context_binding_id
        {
            return Err(StoreError::Invalid(
                "assessment execution inputs do not match the claimed exact target".into(),
            ));
        }
        Ok(AssessmentExecutionInputsV1 {
            evaluator,
            config,
            projection,
            binding,
        })
    }

    pub fn claim_next(
        &self,
        lease_owner: &str,
        estimated_cost_micros: u64,
    ) -> Result<Option<ClaimedAssessmentItemV1>, StoreError> {
        self.store
            .claim_next_assessment(lease_owner, estimated_cost_micros)
    }

    pub fn commit_attempt(
        &self,
        claim: &ClaimedAssessmentItemV1,
        commit: &AssessmentCommitV1,
    ) -> Result<Option<AssessmentRecordV1>, StoreError> {
        self.store.commit_assessment_attempt(claim, commit)
    }
}

/// Persistence boundary for human review and calibration state.
pub struct ReviewRepository<'a> {
    store: &'a WorkspaceStore,
}

impl ReviewRepository<'_> {
    pub fn reviewable_assessments(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
    ) -> Result<Vec<AssessmentRecordV1>, StoreError> {
        self.store
            .list_reviewable_assessments(project_id, evaluator_release_id)
    }

    pub fn leakage_group_id(
        &self,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<String, StoreError> {
        self.store
            .review_leakage_group_id(logical_trace_id, revision)
    }

    pub fn split_release(&self, release_id: &str) -> Result<ReviewSplitReleaseV1, StoreError> {
        self.store.review_split_release(release_id)
    }

    pub fn queues(&self, project_id: &str) -> Result<Vec<ReviewQueueV1>, StoreError> {
        self.store.list_review_queues(project_id)
    }

    pub fn tasks(
        &self,
        project_id: &str,
        mode: Option<ReviewModeV1>,
    ) -> Result<Vec<ReviewTaskV1>, StoreError> {
        self.store.list_review_tasks(project_id, mode)
    }
}

impl WorkspaceStore {
    pub fn assessments(&self) -> AssessmentRepository<'_> {
        AssessmentRepository { store: self }
    }

    pub fn reviews(&self) -> ReviewRepository<'_> {
        ReviewRepository { store: self }
    }
}

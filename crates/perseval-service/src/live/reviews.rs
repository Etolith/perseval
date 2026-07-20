use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use perseval_store::{
    AdjudicationV1, AnnotationCaseV1, AnnotationLabelV1, AnnotationRevisionV1,
    AnnotationSchemaReleaseV1, AssessmentDecisionV1, CalibrationReleaseV1, CalibrationReportV1,
    ReviewAdjudicationPacketV1, ReviewAssignmentV1, ReviewAuthorityV1, ReviewModeV1, ReviewQueueV1,
    ReviewSelectionReasonV1, ReviewSplitReleaseV1, ReviewTaskPresentationV1, ReviewTaskV1,
    ThresholdPolicyActivationV1, ThresholdPolicyReleaseV1,
};
use traces_to_evals::BinaryCalibrationFitOptionsV1;

impl LiveTraceService {
    /// Creates the first human-review stream from the latest executable task-
    /// completion quality check. This is deliberately human-invoked UI
    /// orchestration; MCP does not expose it.
    pub fn create_review_queue_from_completed_assessments(
        &self,
        project_id: &str,
    ) -> Result<(ReviewQueueV1, usize), LiveServiceError> {
        let created_by = self.config.reviewer_ref.clone();
        let checks = self.list_task_completion_quality_checks(project_id)?;
        let mut selected = None;
        for check in checks {
            let assessments = self
                .store
                .list_reviewable_assessments(project_id, &check.config.evaluator_release_id)?;
            if !assessments.is_empty() {
                selected = Some((check.config.evaluator_release_id, assessments));
                break;
            }
        }
        let Some((evaluator_release_id, assessments)) = selected else {
            return Err(LiveServiceError::InvalidInput(
                "Run a task-completion quality check before creating a review queue".into(),
            ));
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let schema = AnnotationSchemaReleaseV1 {
            schema_version: perseval_store::ANNOTATION_SCHEMA_RELEASE_SCHEMA_VERSION.into(),
            project_id: project_id.into(),
            task_kind: traces_to_evals::LearnedTaskKind::TaskCompletion,
            positive_class: "task_failure_or_partial".into(),
            labels: vec![
                AnnotationLabelV1::Completed,
                AnnotationLabelV1::Partial,
                AnnotationLabelV1::Failed,
                AnnotationLabelV1::Abstain,
            ],
            instructions: "Decide whether the observed trace completed the declared task using only trace evidence. Completed means every required outcome is demonstrated. Partial means useful required work succeeded but at least one required outcome remains incomplete. Failed means the primary requested outcome was not achieved, was contradicted, or the attempt caused a terminal failure. Abstain only when telemetry is insufficient to distinguish these states.".into(),
            required_reviewers: 2,
            created_by: created_by.clone(),
            created_at_unix_ms: now,
        };
        let assessments = assessments
            .into_iter()
            .map(|assessment| {
                let leakage_group_id = self
                    .store
                    .review_leakage_group_id(&assessment.logical_trace_id, assessment.revision)?;
                Ok((assessment, leakage_group_id))
            })
            .collect::<Result<Vec<_>, LiveServiceError>>()?;
        let project_queues = self.list_review_queues(project_id)?;
        if let Some(queue) = project_queues
            .iter()
            .find(|queue| {
                queue.active
                    && queue.mode == ReviewModeV1::BlindCalibration
                    && queue.evaluator_release_id == evaluator_release_id
            })
            .cloned()
        {
            let existing_tasks = self.list_review_tasks(project_id, Some(queue.mode))?;
            let evaluator_queue_ids = project_queues
                .iter()
                .filter(|candidate| {
                    candidate.mode == ReviewModeV1::BlindCalibration
                        && candidate.evaluator_release_id == evaluator_release_id
                })
                .map(|candidate| candidate.queue_id.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let existing_assessments = existing_tasks
                .iter()
                .filter(|task| evaluator_queue_ids.contains(task.queue_id.as_str()))
                .map(|task| task.assessment_id.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let pending = assessments
                .iter()
                .filter(|(assessment, _)| {
                    !existing_assessments.contains(assessment.assessment_id.as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            if pending.is_empty() {
                let task_count = existing_tasks
                    .iter()
                    .filter(|task| task.queue_id == queue.queue_id)
                    .count();
                return Ok((queue, task_count));
            }
            let frozen_split = self.store.review_split_release(&queue.split_release_id)?;
            if pending.iter().all(|(_, leakage_group_id)| {
                frozen_split
                    .group_assignments
                    .contains_key(leakage_group_id)
            }) {
                let mut task_count = existing_tasks
                    .iter()
                    .filter(|task| task.queue_id == queue.queue_id)
                    .count();
                for (assessment, _) in pending {
                    let case = self.create_annotation_case(
                        project_id,
                        &queue.annotation_schema_release_id,
                        &assessment.logical_trace_id,
                        assessment.revision,
                        &assessment.context_binding_id,
                        &assessment.projection_hash,
                    )?;
                    self.enqueue_review_task(
                        &queue.queue_id,
                        &case.case_id,
                        &assessment.assessment_id,
                        ReviewSelectionReasonV1::RandomAudit,
                    )?;
                    task_count += 1;
                }
                return Ok((queue, task_count));
            }

            // A frozen split cannot be mutated. Publish a cumulative successor
            // that preserves every prior assignment and adds only new groups;
            // calibration composes completed cases through this exact release.
            let pending_groups = pending
                .iter()
                .map(|(_, leakage_group_id)| leakage_group_id.clone())
                .collect::<BTreeSet<_>>();
            let new_groups = pending_groups
                .iter()
                .filter(|group| !frozen_split.group_assignments.contains_key(*group))
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut group_assignments = frozen_split.group_assignments.clone();
            for group in &new_groups {
                group_assignments.insert(group.clone(), review_split_for_group(group));
            }
            ensure_split_coverage(&mut group_assignments, &new_groups);
            let next_split = ReviewSplitReleaseV1 {
                schema_version: perseval_store::REVIEW_SPLIT_RELEASE_SCHEMA_VERSION.into(),
                project_id: project_id.into(),
                annotation_schema_release_id: queue.annotation_schema_release_id.clone(),
                group_assignments,
                created_by: created_by.clone(),
                created_at_unix_ms: now,
            };
            let next_split_id = self.publish_review_split_release(&next_split)?;
            let next_queue = self.create_review_queue(
                project_id,
                &evaluator_release_id,
                &queue.annotation_schema_release_id,
                &next_split_id,
                ReviewModeV1::BlindCalibration,
                10_000,
                &created_by,
            )?;
            let mut task_count = 0;
            for (assessment, _) in pending {
                let case = self.create_annotation_case(
                    project_id,
                    &next_queue.annotation_schema_release_id,
                    &assessment.logical_trace_id,
                    assessment.revision,
                    &assessment.context_binding_id,
                    &assessment.projection_hash,
                )?;
                self.enqueue_review_task(
                    &next_queue.queue_id,
                    &case.case_id,
                    &assessment.assessment_id,
                    ReviewSelectionReasonV1::RandomAudit,
                )?;
                task_count += 1;
            }
            return Ok((next_queue, task_count));
        }
        let schema_id = self.publish_annotation_schema_release(&schema)?;
        let groups = assessments
            .iter()
            .map(|(_, leakage_group_id)| leakage_group_id.clone())
            .collect::<BTreeSet<_>>();
        let mut group_assignments = groups
            .iter()
            .map(|group| (group.clone(), review_split_for_group(group)))
            .collect::<BTreeMap<_, _>>();
        ensure_split_coverage(&mut group_assignments, &groups);
        let split = ReviewSplitReleaseV1 {
            schema_version: perseval_store::REVIEW_SPLIT_RELEASE_SCHEMA_VERSION.into(),
            project_id: project_id.into(),
            annotation_schema_release_id: schema_id.clone(),
            group_assignments,
            created_by: created_by.clone(),
            created_at_unix_ms: now,
        };
        let split_id = self.publish_review_split_release(&split)?;
        let queue = self.create_review_queue(
            project_id,
            &evaluator_release_id,
            &schema_id,
            &split_id,
            ReviewModeV1::BlindCalibration,
            // This first UI action deliberately reviews the complete baseline.
            // The queue metadata must describe the selection actually made.
            10_000,
            &created_by,
        )?;
        let mut task_count = 0;
        for (assessment, _) in assessments {
            let case = self.create_annotation_case(
                project_id,
                &schema_id,
                &assessment.logical_trace_id,
                assessment.revision,
                &assessment.context_binding_id,
                &assessment.projection_hash,
            )?;
            self.enqueue_review_task(
                &queue.queue_id,
                &case.case_id,
                &assessment.assessment_id,
                ReviewSelectionReasonV1::RandomAudit,
            )?;
            task_count += 1;
        }
        Ok((queue, task_count))
    }

    pub fn fit_latest_review_calibration(
        &self,
        project_id: &str,
    ) -> Result<(String, CalibrationReleaseV1), LiveServiceError> {
        let queue = self
            .list_review_queues(project_id)?
            .into_iter()
            .find(|queue| queue.active && queue.mode == ReviewModeV1::BlindCalibration)
            .ok_or_else(|| {
                LiveServiceError::InvalidInput("No active blind review queue exists".into())
            })?;
        let (release_id, release) = self.publish_calibration_release(
            project_id,
            &queue.evaluator_release_id,
            &queue.annotation_schema_release_id,
            &queue.split_release_id,
            BinaryCalibrationFitOptionsV1::default(),
        )?;
        Ok((release_id, release))
    }

    fn publish_annotation_schema_release(
        &self,
        release: &AnnotationSchemaReleaseV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .publish_annotation_schema_release(release, ReviewAuthorityV1::Human)?)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_annotation_case(
        &self,
        project_id: &str,
        annotation_schema_release_id: &str,
        logical_trace_id: &str,
        revision: u64,
        context_binding_id: &str,
        safe_projection_hash: &str,
    ) -> Result<AnnotationCaseV1, LiveServiceError> {
        Ok(self.store.create_annotation_case(
            project_id,
            annotation_schema_release_id,
            logical_trace_id,
            revision,
            context_binding_id,
            safe_projection_hash,
        )?)
    }

    fn publish_review_split_release(
        &self,
        release: &ReviewSplitReleaseV1,
    ) -> Result<String, LiveServiceError> {
        Ok(self
            .store
            .publish_review_split_release(release, ReviewAuthorityV1::Human)?)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_review_queue(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        annotation_schema_release_id: &str,
        split_release_id: &str,
        mode: ReviewModeV1,
        random_audit_basis_points: u32,
        created_by: &str,
    ) -> Result<ReviewQueueV1, LiveServiceError> {
        Ok(self.store.create_review_queue(
            project_id,
            evaluator_release_id,
            annotation_schema_release_id,
            split_release_id,
            mode,
            random_audit_basis_points,
            created_by,
            ReviewAuthorityV1::Human,
        )?)
    }

    fn enqueue_review_task(
        &self,
        queue_id: &str,
        case_id: &str,
        assessment_id: &str,
        selection_reason: ReviewSelectionReasonV1,
    ) -> Result<ReviewTaskV1, LiveServiceError> {
        Ok(self
            .store
            .enqueue_review_task(queue_id, case_id, assessment_id, selection_reason)?)
    }

    pub fn assign_review_task(
        &self,
        task_id: &str,
    ) -> Result<ReviewAssignmentV1, LiveServiceError> {
        let reviewer_id = self.config.reviewer_ref.as_str();
        Ok(self
            .store
            .assign_review_task(task_id, reviewer_id, ReviewAuthorityV1::Human)?)
    }

    pub fn review_task_for_reviewer(
        &self,
        task_id: &str,
    ) -> Result<ReviewTaskPresentationV1, LiveServiceError> {
        let reviewer_id = self.config.reviewer_ref.as_str();
        Ok(self.store.review_task_for_reviewer(task_id, reviewer_id)?)
    }

    pub fn review_adjudication_packet(
        &self,
        task_id: &str,
    ) -> Result<ReviewAdjudicationPacketV1, LiveServiceError> {
        let adjudicator_id = self.config.reviewer_ref.as_str();
        Ok(self
            .store
            .review_adjudication_packet(task_id, adjudicator_id)?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_annotation_revision(
        &self,
        task_id: &str,
        expected_head_revision_id: Option<&str>,
        label: AnnotationLabelV1,
        explanation: &str,
        evidence_keys: &[String],
    ) -> Result<AnnotationRevisionV1, LiveServiceError> {
        let reviewer_id = self.config.reviewer_ref.as_str();
        Ok(self.store.submit_annotation_revision(
            task_id,
            reviewer_id,
            expected_head_revision_id,
            label,
            explanation,
            evidence_keys,
            ReviewAuthorityV1::Human,
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adjudicate_review_task(
        &self,
        task_id: &str,
        annotation_revision_ids: &[String],
        expected_head_revision_id: Option<&str>,
        label: AnnotationLabelV1,
        explanation: &str,
        evidence_keys: &[String],
    ) -> Result<AdjudicationV1, LiveServiceError> {
        let adjudicated_by = self.config.reviewer_ref.as_str();
        Ok(self.store.adjudicate_review_task(
            task_id,
            annotation_revision_ids,
            expected_head_revision_id,
            label,
            explanation,
            evidence_keys,
            adjudicated_by,
            ReviewAuthorityV1::Human,
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_calibration_release(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        annotation_schema_release_id: &str,
        split_release_id: &str,
        fit_options: BinaryCalibrationFitOptionsV1,
    ) -> Result<(String, CalibrationReleaseV1), LiveServiceError> {
        let created_by = self.config.reviewer_ref.as_str();
        Ok(self.store.publish_calibration_release(
            project_id,
            evaluator_release_id,
            annotation_schema_release_id,
            split_release_id,
            fit_options,
            created_by,
            ReviewAuthorityV1::Human,
        )?)
    }

    pub fn publish_calibration_test_report(
        &self,
        calibration_release_id: &str,
    ) -> Result<CalibrationReportV1, LiveServiceError> {
        Ok(self
            .store
            .publish_calibration_test_report(calibration_release_id)?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish_threshold_policy_release(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        calibration_release_id: &str,
        pass_probability_threshold: f64,
        fail_probability_threshold: f64,
        minimum_decision_confidence: f64,
    ) -> Result<(String, ThresholdPolicyReleaseV1), LiveServiceError> {
        let created_by = self.config.reviewer_ref.as_str();
        Ok(self.store.publish_threshold_policy_release(
            project_id,
            evaluator_release_id,
            calibration_release_id,
            pass_probability_threshold,
            fail_probability_threshold,
            minimum_decision_confidence,
            created_by,
            ReviewAuthorityV1::Human,
        )?)
    }

    pub fn activate_threshold_policy(
        &self,
        threshold_policy_release_id: &str,
    ) -> Result<ThresholdPolicyActivationV1, LiveServiceError> {
        let activated_by = self.config.reviewer_ref.as_str();
        Ok(self.store.activate_threshold_policy(
            threshold_policy_release_id,
            activated_by,
            ReviewAuthorityV1::Human,
        )?)
    }

    pub fn activate_threshold_policy_and_materialize(
        &self,
        threshold_policy_release_id: &str,
    ) -> Result<(ThresholdPolicyActivationV1, usize), LiveServiceError> {
        let activation = self.activate_threshold_policy(threshold_policy_release_id)?;
        let decision_count = self
            .store
            .assessment_decision_count_for_policy(threshold_policy_release_id)?;
        Ok((activation, decision_count))
    }

    pub fn assessment_decisions(
        &self,
        assessment_id: &str,
    ) -> Result<Vec<AssessmentDecisionV1>, LiveServiceError> {
        Ok(self.store.assessment_decisions(assessment_id)?)
    }

    pub fn list_review_queues(
        &self,
        project_id: &str,
    ) -> Result<Vec<ReviewQueueV1>, LiveServiceError> {
        Ok(self.store.list_review_queues(project_id)?)
    }

    pub fn list_review_tasks(
        &self,
        project_id: &str,
        mode: Option<ReviewModeV1>,
    ) -> Result<Vec<ReviewTaskV1>, LiveServiceError> {
        Ok(self.store.list_review_tasks(project_id, mode)?)
    }

    pub fn list_calibration_releases(
        &self,
        project_id: &str,
        evaluator_release_id: Option<&str>,
    ) -> Result<Vec<(String, CalibrationReleaseV1)>, LiveServiceError> {
        Ok(self
            .store
            .list_calibration_releases(project_id, evaluator_release_id)?)
    }

    pub fn list_calibration_reports(
        &self,
        calibration_release_id: &str,
    ) -> Result<Vec<CalibrationReportV1>, LiveServiceError> {
        Ok(self
            .store
            .list_calibration_reports(calibration_release_id)?)
    }

    pub fn threshold_policy_for_calibration(
        &self,
        calibration_release_id: &str,
    ) -> Result<Option<(String, ThresholdPolicyReleaseV1)>, LiveServiceError> {
        Ok(self
            .store
            .threshold_policy_for_calibration(calibration_release_id)?)
    }

    pub fn active_threshold_policy(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
    ) -> Result<Option<(ThresholdPolicyActivationV1, ThresholdPolicyReleaseV1)>, LiveServiceError>
    {
        Ok(self
            .store
            .active_threshold_policy(project_id, evaluator_release_id)?)
    }
}

fn review_split_for_group(group: &str) -> traces_to_evals::CalibrationDataSplitV1 {
    // FNV-1a is specified inline so a session keeps the same default split
    // across machines, restarts, and cumulative cohort releases.
    let hash = group
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    if hash % 5 == 4 {
        traces_to_evals::CalibrationDataSplitV1::Test
    } else {
        traces_to_evals::CalibrationDataSplitV1::Calibration
    }
}

fn ensure_split_coverage(
    assignments: &mut BTreeMap<String, traces_to_evals::CalibrationDataSplitV1>,
    mutable_groups: &BTreeSet<String>,
) {
    if assignments.len() < 2 || mutable_groups.is_empty() {
        return;
    }
    if !assignments
        .values()
        .any(|split| *split == traces_to_evals::CalibrationDataSplitV1::Calibration)
        && let Some(group) = mutable_groups.first()
    {
        assignments.insert(
            group.clone(),
            traces_to_evals::CalibrationDataSplitV1::Calibration,
        );
    }
    if !assignments
        .values()
        .any(|split| *split == traces_to_evals::CalibrationDataSplitV1::Test)
        && let Some(group) = mutable_groups.last()
    {
        assignments.insert(group.clone(), traces_to_evals::CalibrationDataSplitV1::Test);
    }
}

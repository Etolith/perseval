use std::collections::{BTreeMap, BTreeSet};

use duckdb::params as duck_params;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use traces_to_evals::{
    AgreementLabelScaleV1, AgreementRatingV1, BinaryCalibrationExampleV1,
    BinaryCalibrationFitOptionsV1, BinaryCalibrationModelV1, BinaryCalibrationReportV1,
    BinaryPredictionV1, CalibrationDataSplitV1, HumanAgreementReportV1,
    LearnedCalibrationFeaturesV1, LearnedEvaluationV1, LearnedVerdictV1,
    TaskCompletionProjectionV1, canonical_content_id,
};

use super::{StoreError, WorkspaceStore, now_unix_ms};
use crate::model::{
    ADJUDICATION_SCHEMA_VERSION, ANNOTATION_CASE_SCHEMA_VERSION,
    ANNOTATION_REVISION_SCHEMA_VERSION, ANNOTATION_SCHEMA_RELEASE_SCHEMA_VERSION,
    ASSESSMENT_DECISION_SCHEMA_VERSION, AdjudicationV1, AnnotationCaseV1, AnnotationLabelV1,
    AnnotationRevisionV1, AnnotationSchemaReleaseV1, AssessmentDecisionV1, BlindReviewTaskViewV1,
    CALIBRATION_RELEASE_SCHEMA_VERSION, CALIBRATION_REPORT_SCHEMA_VERSION, CalibratedDecisionV1,
    CalibrationMemberV1, CalibrationReleaseV1, CalibrationReportV1, CalibrationSliceReportV1,
    REVIEW_QUEUE_SCHEMA_VERSION, REVIEW_SPLIT_RELEASE_SCHEMA_VERSION, REVIEW_TASK_SCHEMA_VERSION,
    RevealedReviewTaskViewV1, ReviewAdjudicationPacketV1, ReviewAssignmentV1, ReviewAuthorityV1,
    ReviewModeV1, ReviewQueueV1, ReviewSelectionReasonV1, ReviewSplitReleaseV1,
    ReviewTaskPresentationV1, ReviewTaskStatusV1, ReviewTaskV1,
    THRESHOLD_POLICY_ACTIVATION_SCHEMA_VERSION, THRESHOLD_POLICY_RELEASE_SCHEMA_VERSION,
    ThresholdPolicyActivationV1, ThresholdPolicyReleaseV1,
};

#[derive(Debug, Clone)]
struct ResolvedTruth {
    label: AnnotationLabelV1,
    annotation_revision_ids: Vec<String>,
    adjudication_revision_id: Option<String>,
}

impl WorkspaceStore {
    /// Resolves the leakage boundary from the exact immutable trace revision,
    /// rather than the mutable current-run projection. Every revision in the
    /// same observed session therefore receives the same split identity.
    pub fn review_leakage_group_id(
        &self,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<String, StoreError> {
        require_non_empty(logical_trace_id, "logical_trace_id")?;
        let analytics = self.analytics_reads.connection();
        let mut statement = analytics.prepare(
            "SELECT resource_json, attributes_json
             FROM spans
             WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = true
             ORDER BY topology_depth NULLS LAST, start_time_unix_nano, span_id",
        )?;
        let rows = statement
            .query_map(duck_params![logical_trace_id, revision as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (resource_json, attributes_json) in rows {
            for encoded in [resource_json, attributes_json] {
                let values: BTreeMap<String, serde_json::Value> = serde_json::from_str(&encoded)?;
                for key in [
                    "gen_ai.conversation.id",
                    "session.id",
                    "openinference.session.id",
                ] {
                    if let Some(session_id) = values
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        return Ok(format!("session:{session_id}"));
                    }
                }
            }
        }
        Ok(format!("trace:{logical_trace_id}"))
    }

    /// Reads only an allowlisted set of low-cardinality calibration
    /// dimensions from an exact immutable trace revision. This is separate
    /// from the mutable current-run projection and never reads payload bodies.
    fn review_slice_values(
        &self,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        let analytics = self.analytics_reads.connection();
        let mut statement = analytics.prepare(
            "SELECT resource_json, attributes_json
             FROM spans
             WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = true
             ORDER BY topology_depth NULLS LAST, start_time_unix_nano, span_id",
        )?;
        let rows = statement
            .query_map(duck_params![logical_trace_id, revision as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let dimensions = [
            (
                "environment",
                &[
                    "deployment.environment.name",
                    "deployment.environment",
                    "environment",
                ][..],
            ),
            (
                "build",
                &[
                    "service.version",
                    "service.build.id",
                    "build.id",
                    "vcs.ref.head.revision",
                ][..],
            ),
            (
                "language",
                &[
                    "enduser.language",
                    "user.language",
                    "gen_ai.output.language",
                    "language",
                ][..],
            ),
            (
                "domain",
                &["agent.domain", "application.domain", "domain"][..],
            ),
        ];
        let mut values = dimensions
            .iter()
            .map(|(dimension, _)| ((*dimension).to_owned(), "not reported".to_owned()))
            .collect::<BTreeMap<_, _>>();
        for (resource_json, attributes_json) in rows {
            for encoded in [resource_json, attributes_json] {
                let attributes: BTreeMap<String, serde_json::Value> =
                    serde_json::from_str(&encoded)?;
                for (dimension, keys) in dimensions {
                    if values
                        .get(dimension)
                        .is_some_and(|value| value != "not reported")
                    {
                        continue;
                    }
                    if let Some(value) = keys
                        .iter()
                        .find_map(|key| attributes.get(*key).and_then(calibration_slice_scalar))
                    {
                        values.insert(dimension.to_owned(), value);
                    }
                }
            }
        }
        Ok(values)
    }

    pub fn list_reviewable_assessments(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
    ) -> Result<Vec<crate::model::AssessmentRecordV1>, StoreError> {
        let targets = {
            let control = self.control.lock().expect("control store lock poisoned");
            let mut statement = control.prepare(
                "SELECT DISTINCT logical_trace_id, revision
                 FROM assessments
                 WHERE project_id = ?1 AND evaluator_release_id = ?2
                   AND evaluation_json IS NOT NULL
                 ORDER BY logical_trace_id ASC, revision ASC",
            )?;
            statement
                .query_map(params![project_id, evaluator_release_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut assessments = Vec::new();
        for (logical_trace_id, revision) in targets {
            assessments.extend(
                self.load_trace_assessments_unchecked(project_id, &logical_trace_id, revision)?
                    .into_iter()
                    .filter(|assessment| {
                        assessment.evaluator_release_id == evaluator_release_id
                            && assessment.evaluation.is_some()
                    }),
            );
        }
        assessments.sort_by(|left, right| left.assessment_id.cmp(&right.assessment_id));
        Ok(assessments)
    }

    pub fn publish_annotation_schema_release(
        &self,
        release: &AnnotationSchemaReleaseV1,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        require_human(authority, "publish an annotation schema release")?;
        validate_annotation_schema(release)?;
        let release_id = review_identity("perseval.annotation-schema-release.v1", release)?;
        let control = self.control.lock().expect("control store lock poisoned");
        let project_exists = control.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE project_id = ?1)",
            params![release.project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !project_exists {
            return Err(StoreError::Invalid(
                "annotation schema project does not exist".into(),
            ));
        }
        control.execute(
            "INSERT INTO annotation_schema_releases(
                annotation_schema_release_id, project_id, release_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(annotation_schema_release_id) DO NOTHING",
            params![
                release_id,
                release.project_id,
                serde_json::to_string(release)?,
                release.created_at_unix_ms,
            ],
        )?;
        Ok(release_id)
    }

    /// Compatibility alias retained for the unshipped PV-03 call surface.
    pub fn activate_annotation_schema_release(
        &self,
        release: &AnnotationSchemaReleaseV1,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        self.publish_annotation_schema_release(release, authority)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_annotation_case(
        &self,
        project_id: &str,
        annotation_schema_release_id: &str,
        logical_trace_id: &str,
        revision: u64,
        context_binding_id: &str,
        safe_projection_hash: &str,
    ) -> Result<AnnotationCaseV1, StoreError> {
        require_non_empty(project_id, "project_id")?;
        require_non_empty(annotation_schema_release_id, "annotation_schema_release_id")?;
        require_non_empty(logical_trace_id, "logical_trace_id")?;
        require_non_empty(context_binding_id, "context_binding_id")?;
        require_sha256(safe_projection_hash, "safe_projection_hash")?;
        let leakage_group_id = self.review_leakage_group_id(logical_trace_id, revision)?;
        let evidence_span_ids = {
            let analytics = self.analytics_reads.connection();
            let mut statement = analytics.prepare(
                "SELECT span_id FROM spans
                 WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE
                 ORDER BY span_id ASC",
            )?;
            statement
                .query_map(duck_params![logical_trace_id, revision as i64], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if evidence_span_ids.is_empty() {
            return Err(StoreError::Invalid(
                "annotation case has no evaluator-independent trace evidence".into(),
            ));
        }
        let now = now_unix_ms();
        let control = self.control.lock().expect("control store lock poisoned");
        let schema_exists = control.query_row(
            "SELECT EXISTS(SELECT 1 FROM annotation_schema_releases
              WHERE annotation_schema_release_id = ?1 AND project_id = ?2)",
            params![annotation_schema_release_id, project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !schema_exists {
            return Err(StoreError::Invalid(
                "annotation schema is missing or cross-project".into(),
            ));
        }
        let target = control
            .query_row(
                "SELECT target_id FROM assessment_targets
                 WHERE project_id = ?1 AND logical_trace_id = ?2 AND revision = ?3
                   AND target_kind = 'trace_revision'",
                params![project_id, logical_trace_id, revision as i64],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid("annotation case target is missing or not finalized".into())
            })?;
        let binding_matches = control.query_row(
            "SELECT EXISTS(SELECT 1 FROM trace_context_bindings
              WHERE binding_id = ?1 AND project_id = ?2
                AND logical_trace_id = ?3 AND revision = ?4)",
            params![
                context_binding_id,
                project_id,
                logical_trace_id,
                revision as i64
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if !binding_matches {
            return Err(StoreError::Invalid(
                "annotation case context binding is stale or cross-project".into(),
            ));
        }
        let case_id = review_identity(
            "perseval.annotation-case.v1",
            &(
                project_id,
                annotation_schema_release_id,
                &target,
                context_binding_id,
                safe_projection_hash,
                &leakage_group_id,
            ),
        )?;
        let case = AnnotationCaseV1 {
            schema_version: ANNOTATION_CASE_SCHEMA_VERSION.into(),
            case_id,
            project_id: project_id.into(),
            annotation_schema_release_id: annotation_schema_release_id.into(),
            target_id: target,
            logical_trace_id: logical_trace_id.into(),
            revision,
            context_binding_id: context_binding_id.into(),
            safe_projection_hash: safe_projection_hash.into(),
            leakage_group_id,
            created_at_unix_ms: now,
        };
        control.execute(
            "INSERT INTO annotation_cases(
                case_id, project_id, annotation_schema_release_id, target_id,
                logical_trace_id, revision, context_binding_id, safe_projection_hash,
                leakage_group_id, case_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(case_id) DO NOTHING",
            params![
                case.case_id,
                case.project_id,
                case.annotation_schema_release_id,
                case.target_id,
                case.logical_trace_id,
                case.revision as i64,
                case.context_binding_id,
                case.safe_projection_hash,
                case.leakage_group_id,
                serde_json::to_string(&case)?,
                now,
            ],
        )?;
        for span_id in evidence_span_ids {
            control.execute(
                "INSERT OR IGNORE INTO annotation_case_evidence(case_id, evidence_key, span_id)
                 VALUES (?1, ?2, ?3)",
                params![case.case_id, format!("span:{span_id}"), span_id],
            )?;
        }
        Ok(case)
    }

    pub fn publish_review_split_release(
        &self,
        release: &ReviewSplitReleaseV1,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        require_human(authority, "publish a review split release")?;
        if release.schema_version != REVIEW_SPLIT_RELEASE_SCHEMA_VERSION
            || release.project_id.trim().is_empty()
            || release.annotation_schema_release_id.trim().is_empty()
            || release.created_by.trim().is_empty()
            || release.group_assignments.is_empty()
            || release
                .group_assignments
                .keys()
                .any(|group| group.trim().is_empty())
        {
            return Err(StoreError::Invalid("invalid review split release".into()));
        }
        let split_release_id = review_identity("perseval.review-split-release.v1", release)?;
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let schema_exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM annotation_schema_releases
              WHERE annotation_schema_release_id = ?1 AND project_id = ?2)",
            params![release.annotation_schema_release_id, release.project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !schema_exists {
            return Err(StoreError::Invalid(
                "review split schema is missing or cross-project".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO review_split_releases(
                split_release_id, project_id, annotation_schema_release_id,
                release_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(split_release_id) DO NOTHING",
            params![
                split_release_id,
                release.project_id,
                release.annotation_schema_release_id,
                serde_json::to_string(release)?,
                release.created_at_unix_ms,
            ],
        )?;
        for (group_id, split) in &release.group_assignments {
            transaction.execute(
                "INSERT INTO review_split_groups(split_release_id, leakage_group_id, split)
                 VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
                params![split_release_id, group_id, calibration_split_name(*split)],
            )?;
        }
        transaction.commit()?;
        Ok(split_release_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_review_queue(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        annotation_schema_release_id: &str,
        split_release_id: &str,
        mode: ReviewModeV1,
        random_audit_basis_points: u32,
        created_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<ReviewQueueV1, StoreError> {
        require_human(authority, "activate a review queue")?;
        if random_audit_basis_points > 10_000 {
            return Err(StoreError::Invalid(
                "random audit basis points must not exceed 10000".into(),
            ));
        }
        for (value, name) in [
            (project_id, "project_id"),
            (evaluator_release_id, "evaluator_release_id"),
            (annotation_schema_release_id, "annotation_schema_release_id"),
            (split_release_id, "split_release_id"),
            (created_by, "created_by"),
        ] {
            require_non_empty(value, name)?;
        }
        let now = now_unix_ms();
        let queue_id = review_identity(
            "perseval.review-queue.v1",
            &(
                project_id,
                evaluator_release_id,
                annotation_schema_release_id,
                split_release_id,
                mode,
                random_audit_basis_points,
                created_by,
                now,
            ),
        )?;
        let queue = ReviewQueueV1 {
            schema_version: REVIEW_QUEUE_SCHEMA_VERSION.into(),
            queue_id,
            project_id: project_id.into(),
            evaluator_release_id: evaluator_release_id.into(),
            annotation_schema_release_id: annotation_schema_release_id.into(),
            split_release_id: split_release_id.into(),
            mode,
            random_audit_basis_points,
            active: true,
            created_by: created_by.into(),
            created_at_unix_ms: now,
        };
        let control = self.control.lock().expect("control store lock poisoned");
        let bindings_match = control.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM evaluator_releases e
                JOIN annotation_schema_releases s ON s.project_id = e.project_id
                JOIN review_split_releases r
                  ON r.project_id = s.project_id
                 AND r.annotation_schema_release_id = s.annotation_schema_release_id
                WHERE e.project_id = ?1 AND e.evaluator_release_id = ?2
                  AND s.annotation_schema_release_id = ?3 AND r.split_release_id = ?4
             )",
            params![
                project_id,
                evaluator_release_id,
                annotation_schema_release_id,
                split_release_id
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if !bindings_match {
            return Err(StoreError::Invalid(
                "review queue dependencies are missing or cross-project".into(),
            ));
        }
        control.execute(
            "INSERT INTO review_queues(
                queue_id, project_id, evaluator_release_id,
                annotation_schema_release_id, split_release_id, mode,
                queue_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                queue.queue_id,
                queue.project_id,
                queue.evaluator_release_id,
                queue.annotation_schema_release_id,
                queue.split_release_id,
                review_mode_name(queue.mode),
                serde_json::to_string(&queue)?,
                now,
            ],
        )?;
        Ok(queue)
    }

    pub fn enqueue_review_task(
        &self,
        queue_id: &str,
        case_id: &str,
        assessment_id: &str,
        selection_reason: ReviewSelectionReasonV1,
    ) -> Result<ReviewTaskV1, StoreError> {
        for (value, name) in [
            (queue_id, "queue_id"),
            (case_id, "case_id"),
            (assessment_id, "assessment_id"),
        ] {
            require_non_empty(value, name)?;
        }
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let queue = load_json_by_id::<ReviewQueueV1>(
            &transaction,
            "SELECT queue_json FROM review_queues WHERE queue_id = ?1",
            queue_id,
            "review queue not found",
        )?;
        if !queue.active {
            return Err(StoreError::Invalid("review queue is not active".into()));
        }
        let case = load_json_by_id::<AnnotationCaseV1>(
            &transaction,
            "SELECT case_json FROM annotation_cases WHERE case_id = ?1",
            case_id,
            "annotation case not found",
        )?;
        if case.project_id != queue.project_id
            || case.annotation_schema_release_id != queue.annotation_schema_release_id
        {
            return Err(StoreError::Invalid(
                "annotation case is cross-project or uses another schema".into(),
            ));
        }
        let split_name = transaction
            .query_row(
                "SELECT split FROM review_split_groups
                 WHERE split_release_id = ?1 AND leakage_group_id = ?2",
                params![queue.split_release_id, case.leakage_group_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid(
                    "annotation case leakage group is absent from the frozen split release".into(),
                )
            })?;
        let split = parse_calibration_split(&split_name)?;
        let assessment_matches = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM assessments
              WHERE assessment_id = ?1 AND project_id = ?2
                AND logical_trace_id = ?3 AND revision = ?4
                AND evaluator_release_id = ?5 AND context_binding_id = ?6
                AND projection_hash = ?7)",
            params![
                assessment_id,
                queue.project_id,
                case.logical_trace_id,
                case.revision as i64,
                queue.evaluator_release_id,
                case.context_binding_id,
                case.safe_projection_hash,
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if !assessment_matches {
            return Err(StoreError::Invalid(
                "review task assessment is missing, stale, or cross-project".into(),
            ));
        }
        let required_reviewers: u32 = transaction.query_row(
            "SELECT json_extract(release_json, '$.required_reviewers')
             FROM annotation_schema_releases
             WHERE annotation_schema_release_id = ?1",
            params![queue.annotation_schema_release_id],
            |row| row.get(0),
        )?;
        let duplicate_case_for_evaluator = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM review_tasks existing
                JOIN review_queues existing_queue
                  ON existing_queue.queue_id = existing.queue_id
                WHERE existing.case_id = ?1
                  AND existing_queue.evaluator_release_id = ?2
             )",
            params![case.case_id, queue.evaluator_release_id],
            |row| row.get::<_, bool>(0),
        )?;
        if duplicate_case_for_evaluator {
            return Err(StoreError::Invalid(
                "this exact human-review case already calibrates the evaluator release".into(),
            ));
        }
        if queue.mode == ReviewModeV1::VisibleTriage
            && selection_reason == ReviewSelectionReasonV1::RandomAudit
        {
            return Err(StoreError::Invalid(
                "visible triage cannot be labeled as independent random-audit ground truth".into(),
            ));
        }
        if queue.mode == ReviewModeV1::BlindCalibration
            && selection_reason != ReviewSelectionReasonV1::RandomAudit
        {
            let random_audit_count = transaction.query_row(
                "SELECT COUNT(*) FROM review_tasks
                 WHERE queue_id = ?1
                   AND json_extract(task_json, '$.selection_reason') = 'random_audit'",
                params![queue.queue_id],
                |row| row.get::<_, u64>(0),
            )?;
            if queue.random_audit_basis_points == 0 || random_audit_count == 0 {
                return Err(StoreError::Invalid(
                    "selected blind-review cases require an existing random-audit lane".into(),
                ));
            }
        }
        let task_id = review_identity(
            "perseval.review-task.v1",
            &(
                &queue.queue_id,
                &case.case_id,
                assessment_id,
                selection_reason,
            ),
        )?;
        let task = ReviewTaskV1 {
            schema_version: REVIEW_TASK_SCHEMA_VERSION.into(),
            task_id,
            queue_id: queue.queue_id,
            case_id: case.case_id,
            project_id: queue.project_id,
            logical_trace_id: case.logical_trace_id,
            revision: case.revision,
            assessment_id: assessment_id.into(),
            leakage_group_id: case.leakage_group_id,
            split,
            selection_reason,
            required_reviewers,
            status: ReviewTaskStatusV1::Pending,
            created_at_unix_ms: now,
        };
        transaction.execute(
            "INSERT INTO review_tasks(
                task_id, queue_id, case_id, project_id, logical_trace_id, revision,
                assessment_id, leakage_group_id, split, status, task_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                task.task_id,
                task.queue_id,
                task.case_id,
                task.project_id,
                task.logical_trace_id,
                task.revision as i64,
                task.assessment_id,
                task.leakage_group_id,
                split_name,
                review_task_status_name(task.status),
                serde_json::to_string(&task)?,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(task)
    }

    pub fn assign_review_task(
        &self,
        task_id: &str,
        reviewer_id: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<ReviewAssignmentV1, StoreError> {
        require_human(authority, "accept a review assignment")?;
        require_non_empty(reviewer_id, "reviewer_id")?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let mut task = load_task(&transaction, task_id)?;
        if matches!(
            task.status,
            ReviewTaskStatusV1::Completed | ReviewTaskStatusV1::Cancelled
        ) {
            return Err(StoreError::Invalid(
                "completed or cancelled review task cannot be assigned".into(),
            ));
        }
        if let Some(existing) = load_assignment(&transaction, task_id, reviewer_id)? {
            return Ok(existing);
        }
        let assignment_count: u32 = transaction.query_row(
            "SELECT COUNT(*) FROM review_assignments WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        if assignment_count >= task.required_reviewers {
            return Err(StoreError::Invalid(
                "review task already has its required independent reviewers".into(),
            ));
        }
        let ordinal = assignment_count + 1;
        transaction.execute(
            "INSERT INTO review_assignments(
                task_id, reviewer_id, reviewer_ordinal, assigned_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4)",
            params![task_id, reviewer_id, ordinal, now],
        )?;
        task.status = ReviewTaskStatusV1::InReview;
        persist_task_status(&transaction, &task)?;
        transaction.commit()?;
        Ok(ReviewAssignmentV1 {
            task_id: task_id.into(),
            reviewer_id: reviewer_id.into(),
            assigned_at_unix_ms: now,
            submitted_annotation_revision_id: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_annotation_revision(
        &self,
        task_id: &str,
        reviewer_id: &str,
        expected_head_revision_id: Option<&str>,
        label: AnnotationLabelV1,
        explanation: &str,
        evidence_keys: &[String],
        authority: ReviewAuthorityV1,
    ) -> Result<AnnotationRevisionV1, StoreError> {
        require_human(authority, "submit an independent annotation")?;
        validate_review_answer(label, explanation, evidence_keys)?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let mut task = load_task(&transaction, task_id)?;
        let queue = load_queue(&transaction, &task.queue_id)?;
        if task.status == ReviewTaskStatusV1::Cancelled {
            return Err(StoreError::Invalid(
                "cancelled review task cannot be annotated".into(),
            ));
        }
        load_assignment(&transaction, task_id, reviewer_id)?
            .ok_or_else(|| StoreError::Invalid("reviewer is not assigned to this task".into()))?;
        validate_evidence_keys(&transaction, &task.case_id, evidence_keys)?;
        let annotation_id = review_identity(
            "perseval.annotation.v1",
            &(
                &task.case_id,
                annotation_schema_for_task(&transaction, task_id)?,
                reviewer_id,
            ),
        )?;
        transaction.execute(
            "INSERT INTO annotations(
                annotation_id, case_id, annotation_schema_release_id,
                reviewer_id, created_at_unix_ms
             ) SELECT ?1, ?2, q.annotation_schema_release_id, ?3, ?4
               FROM review_tasks t JOIN review_queues q ON q.queue_id = t.queue_id
              WHERE t.task_id = ?5
             ON CONFLICT(annotation_id) DO NOTHING",
            params![annotation_id, task.case_id, reviewer_id, now, task_id],
        )?;
        let latest = latest_annotation_for_logical(&transaction, &annotation_id)?;
        if queue.mode == ReviewModeV1::BlindCalibration
            && task.status == ReviewTaskStatusV1::Completed
            && latest.is_some()
        {
            return Err(StoreError::Invalid(
                "blind answers are locked after evaluator reveal; start a new blind review round for a correction"
                    .into(),
            ));
        }
        if latest.as_ref().map(|value| value.revision_id.as_str()) != expected_head_revision_id {
            return Err(StoreError::Invalid(
                "annotation head changed; reload before appending a correction".into(),
            ));
        }
        let annotation_revision = latest
            .as_ref()
            .map_or(1, |value| value.annotation_revision + 1);
        let annotation_schema_release_id = annotation_schema_for_task(&transaction, task_id)?;
        let revision_id = review_identity(
            "perseval.annotation-revision.v1",
            &(
                &annotation_id,
                annotation_revision,
                expected_head_revision_id,
                task_id,
                label,
                explanation.trim(),
                evidence_keys,
                now,
            ),
        )?;
        let annotation = AnnotationRevisionV1 {
            schema_version: ANNOTATION_REVISION_SCHEMA_VERSION.into(),
            annotation_id,
            revision_id,
            case_id: task.case_id.clone(),
            annotation_schema_release_id,
            source_task_id: task_id.into(),
            reviewer_id: reviewer_id.into(),
            annotation_revision,
            supersedes_revision_id: latest.map(|value| value.revision_id),
            label,
            explanation: explanation.trim().into(),
            evidence_keys: evidence_keys.to_vec(),
            submitted_at_unix_ms: now,
        };
        transaction.execute(
            "INSERT INTO annotation_revisions(
                revision_id, annotation_id, case_id, source_task_id, reviewer_id,
                annotation_revision, supersedes_revision_id, label,
                annotation_json, submitted_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                annotation.revision_id,
                annotation.annotation_id,
                annotation.case_id,
                annotation.source_task_id,
                annotation.reviewer_id,
                annotation.annotation_revision as i64,
                annotation.supersedes_revision_id,
                annotation_label_name(label),
                serde_json::to_string(&annotation)?,
                now,
            ],
        )?;
        transaction.execute(
            "UPDATE review_assignments SET submitted_annotation_revision_id = ?3
             WHERE task_id = ?1 AND reviewer_id = ?2",
            params![task_id, reviewer_id, annotation.revision_id],
        )?;
        task.status = resolve_task_status(&transaction, &task)?;
        persist_task_status(&transaction, &task)?;
        transaction.commit()?;
        Ok(annotation)
    }

    pub fn review_task_for_reviewer(
        &self,
        task_id: &str,
        reviewer_id: &str,
    ) -> Result<ReviewTaskPresentationV1, StoreError> {
        let (
            task,
            queue,
            assignment,
            annotation_schema,
            evidence_keys,
            latest_annotation,
            submitted_review_count,
        ) = {
            let control = self.control.lock().expect("control store lock poisoned");
            let task = load_task(&control, task_id)?;
            let queue = load_queue(&control, &task.queue_id)?;
            let assignment = load_assignment(&control, task_id, reviewer_id)?.ok_or_else(|| {
                StoreError::Invalid("reviewer is not assigned to this task".into())
            })?;
            let annotation_schema = load_json_by_id::<AnnotationSchemaReleaseV1>(
                &control,
                "SELECT release_json FROM annotation_schema_releases
                 WHERE annotation_schema_release_id = ?1",
                &queue.annotation_schema_release_id,
                "annotation schema release not found",
            )?;
            let latest_annotation = latest_annotation_for_reviewer(&control, task_id, reviewer_id)?;
            let evidence_keys = review_evidence_keys(&control, &task.case_id)?;
            let submitted_review_count = control.query_row(
                "SELECT COUNT(*) FROM review_assignments
                 WHERE task_id = ?1 AND submitted_annotation_revision_id IS NOT NULL",
                params![task_id],
                |row| row.get::<_, u32>(0),
            )?;
            (
                task,
                queue,
                assignment,
                annotation_schema,
                evidence_keys,
                latest_annotation,
                submitted_review_count,
            )
        };
        let reveal = queue.mode == ReviewModeV1::VisibleTriage
            || blind_task_model_reveal_allowed(&self.control, &task, &queue)?;
        if !reveal {
            return Ok(ReviewTaskPresentationV1::Blind(Box::new(
                BlindReviewTaskViewV1 {
                    task,
                    assignment,
                    annotation_schema,
                    evidence_keys,
                    latest_annotation,
                    submitted_review_count,
                },
            )));
        }
        let assessment = self
            .load_trace_assessments_unchecked(
                &task.project_id,
                &task.logical_trace_id,
                task.revision,
            )?
            .into_iter()
            .find(|assessment| assessment.assessment_id == task.assessment_id)
            .ok_or_else(|| StoreError::Invalid("review task assessment disappeared".into()))?;
        Ok(ReviewTaskPresentationV1::Revealed(Box::new(
            RevealedReviewTaskViewV1 {
                task,
                assignment,
                annotation_schema,
                evidence_keys,
                latest_annotation,
                assessment,
            },
        )))
    }

    pub fn review_adjudication_packet(
        &self,
        task_id: &str,
        adjudicator_id: &str,
    ) -> Result<ReviewAdjudicationPacketV1, StoreError> {
        require_non_empty(adjudicator_id, "adjudicator_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let task = load_task(&control, task_id)?;
        if task.status != ReviewTaskStatusV1::AwaitingAdjudication {
            return Err(StoreError::Invalid(
                "only a current reviewer disagreement has an adjudication packet".into(),
            ));
        }
        if assigned_reviewer_ids(&control, task_id)?
            .iter()
            .any(|reviewer| reviewer == adjudicator_id)
        {
            return Err(StoreError::Invalid(
                "the adjudicator must be distinct from both independent reviewers".into(),
            ));
        }
        let annotation_revision_ids = current_annotation_revision_ids(&control, task_id)?;
        let annotation_schema = load_json_by_id::<AnnotationSchemaReleaseV1>(
            &control,
            "SELECT s.release_json
             FROM annotation_schema_releases s
             JOIN review_queues q
               ON q.annotation_schema_release_id = s.annotation_schema_release_id
             JOIN review_tasks t ON t.queue_id = q.queue_id
             WHERE t.task_id = ?1",
            task_id,
            "annotation schema release not found",
        )?;
        let mut annotation_revisions = annotation_revision_ids
            .iter()
            .map(|revision_id| {
                load_json_by_id::<AnnotationRevisionV1>(
                    &control,
                    "SELECT annotation_json FROM annotation_revisions WHERE revision_id = ?1",
                    revision_id,
                    "annotation revision not found",
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        annotation_revisions.sort_by(|left, right| left.reviewer_id.cmp(&right.reviewer_id));
        let adjudication_id = review_identity("perseval.adjudication.v1", &task.case_id)?;
        Ok(ReviewAdjudicationPacketV1 {
            annotation_schema,
            evidence_keys: review_evidence_keys(&control, &task.case_id)?,
            latest_adjudication: latest_adjudication(&control, &adjudication_id)?,
            task,
            annotation_revisions,
        })
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
        adjudicated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<AdjudicationV1, StoreError> {
        require_human(authority, "adjudicate reviewer disagreement")?;
        validate_review_answer(label, explanation, evidence_keys)?;
        require_non_empty(adjudicated_by, "adjudicated_by")?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let mut task = load_task(&transaction, task_id)?;
        if task.status != ReviewTaskStatusV1::AwaitingAdjudication {
            return Err(StoreError::Invalid(
                "only a current reviewer disagreement can be adjudicated".into(),
            ));
        }
        let reviewer_ids = assigned_reviewer_ids(&transaction, task_id)?;
        if reviewer_ids
            .iter()
            .any(|reviewer| reviewer == adjudicated_by)
        {
            return Err(StoreError::Invalid(
                "the adjudicator must be distinct from both independent reviewers".into(),
            ));
        }
        let current_ids = current_annotation_revision_ids(&transaction, task_id)?;
        if as_set(annotation_revision_ids) != as_set(&current_ids) {
            return Err(StoreError::Invalid(
                "adjudication must bind the current independent annotation revisions".into(),
            ));
        }
        validate_evidence_keys(&transaction, &task.case_id, evidence_keys)?;
        let adjudication_id = review_identity("perseval.adjudication.v1", &task.case_id)?;
        let latest = latest_adjudication(&transaction, &adjudication_id)?;
        if latest.as_ref().map(|value| value.revision_id.as_str()) != expected_head_revision_id {
            return Err(StoreError::Invalid(
                "adjudication head changed; reload before appending".into(),
            ));
        }
        let adjudication_revision = latest
            .as_ref()
            .map_or(1, |value| value.adjudication_revision + 1);
        let revision_id = review_identity(
            "perseval.adjudication-revision.v1",
            &(
                &adjudication_id,
                adjudication_revision,
                expected_head_revision_id,
                annotation_revision_ids,
                label,
                explanation.trim(),
                evidence_keys,
                adjudicated_by,
                now,
            ),
        )?;
        let adjudication = AdjudicationV1 {
            schema_version: ADJUDICATION_SCHEMA_VERSION.into(),
            adjudication_id,
            revision_id,
            adjudication_revision,
            supersedes_revision_id: latest.map(|value| value.revision_id),
            task_id: task_id.into(),
            annotation_revision_ids: annotation_revision_ids.to_vec(),
            label,
            explanation: explanation.trim().into(),
            evidence_keys: evidence_keys.to_vec(),
            adjudicated_by: adjudicated_by.into(),
            adjudicated_at_unix_ms: now,
        };
        transaction.execute(
            "INSERT INTO adjudication_revisions(
                revision_id, adjudication_id, task_id, adjudication_revision,
                supersedes_revision_id, adjudication_json, adjudicated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                adjudication.revision_id,
                adjudication.adjudication_id,
                adjudication.task_id,
                adjudication.adjudication_revision as i64,
                adjudication.supersedes_revision_id,
                serde_json::to_string(&adjudication)?,
                now,
            ],
        )?;
        for annotation_revision_id in annotation_revision_ids {
            transaction.execute(
                "INSERT INTO adjudication_inputs(
                    adjudication_revision_id, annotation_revision_id
                 ) VALUES (?1, ?2)",
                params![adjudication.revision_id, annotation_revision_id],
            )?;
        }
        task.status = ReviewTaskStatusV1::Completed;
        persist_task_status(&transaction, &task)?;
        transaction.commit()?;
        Ok(adjudication)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish_calibration_release(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
        annotation_schema_release_id: &str,
        split_release_id: &str,
        fit_options: BinaryCalibrationFitOptionsV1,
        created_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<(String, CalibrationReleaseV1), StoreError> {
        require_human(authority, "publish a calibration release")?;
        for (value, name) in [
            (project_id, "project_id"),
            (evaluator_release_id, "evaluator_release_id"),
            (annotation_schema_release_id, "annotation_schema_release_id"),
            (split_release_id, "split_release_id"),
            (created_by, "created_by"),
        ] {
            require_non_empty(value, name)?;
        }
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        validate_calibration_dependencies(
            &transaction,
            project_id,
            evaluator_release_id,
            annotation_schema_release_id,
            split_release_id,
        )?;
        let mut members = calibration_members(
            &transaction,
            project_id,
            evaluator_release_id,
            annotation_schema_release_id,
            split_release_id,
            CalibrationDataSplitV1::Calibration,
        )?;
        for member in &mut members {
            member.slice_values =
                self.review_slice_values(&member.logical_trace_id, member.revision)?;
            member.slice_values.insert(
                "selection stream".into(),
                review_selection_reason_name(member.selection_reason).into(),
            );
        }
        let selected_task_ids = members
            .iter()
            .map(|member| member.task_id.as_str())
            .collect::<BTreeSet<_>>();
        let ratings = agreement_ratings(&transaction, &selected_task_ids)?;
        let agreement_report = HumanAgreementReportV1::from_ratings(
            &ratings,
            &AgreementLabelScaleV1 {
                labels: vec![
                    "completed".into(),
                    "partial".into(),
                    "failed".into(),
                    "abstain".into(),
                ],
                ordinal: false,
            },
        )
        .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let ordinal_ratings = ratings
            .iter()
            .filter(|rating| rating.label != "abstain")
            .cloned()
            .collect::<Vec<_>>();
        let ordinal_agreement_report = (!ordinal_ratings.is_empty())
            .then(|| {
                HumanAgreementReportV1::from_ratings(
                    &ordinal_ratings,
                    &AgreementLabelScaleV1 {
                        labels: vec!["completed".into(), "partial".into(), "failed".into()],
                        ordinal: true,
                    },
                )
                .map_err(|error| StoreError::Invalid(error.to_string()))
            })
            .transpose()?;
        members.retain(|member| member.label.is_failure().is_some() && member.features.is_some());
        if members.len() < 2 {
            return Err(StoreError::Invalid(
                "calibration release requires at least two resolved, scored calibration cases"
                    .into(),
            ));
        }
        let examples = members
            .iter()
            .map(|member| BinaryCalibrationExampleV1 {
                observation_id: member.task_id.clone(),
                group_id: member.leakage_group_id.clone(),
                evaluator_release_id: evaluator_release_id.into(),
                split: CalibrationDataSplitV1::Calibration,
                features: member
                    .features
                    .clone()
                    .expect("retained calibrated features"),
                label_failure: member
                    .label
                    .is_failure()
                    .expect("retained decisive human label"),
            })
            .collect::<Vec<_>>();
        let model = BinaryCalibrationModelV1::fit(&examples, fit_options)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let model_id = model
            .model_id()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let predictions = examples
            .iter()
            .map(|example| {
                model
                    .predict_failure_probability(&example.features)
                    .map(|probability| BinaryPredictionV1 {
                        observation_id: example.observation_id.clone(),
                        group_id: example.group_id.clone(),
                        evaluator_release_id: evaluator_release_id.into(),
                        calibration_model_id: model_id.clone(),
                        split: CalibrationDataSplitV1::Calibration,
                        probability_failure: Some(probability),
                        label_failure: example.label_failure,
                    })
                    .map_err(|error| StoreError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (member, prediction) in members.iter_mut().zip(&predictions) {
            member.calibrated_failure_probability = prediction.probability_failure;
        }
        let fit_report = BinaryCalibrationReportV1::from_predictions(&predictions, 0.5, 10)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let fit_slice_reports = calibration_slice_reports(&members, &predictions, 0.5)?;
        let review_selection_hash = review_identity(
            "perseval.calibration-selection.v1",
            &members
                .iter()
                .map(|member| {
                    (
                        &member.task_id,
                        &member.assessment_id,
                        &member.leakage_group_id,
                        &member.annotation_revision_ids,
                        &member.adjudication_revision_id,
                        member.label,
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        let fit_annotation_revision_ids = members
            .iter()
            .flat_map(|member| member.annotation_revision_ids.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let release = CalibrationReleaseV1 {
            schema_version: CALIBRATION_RELEASE_SCHEMA_VERSION.into(),
            project_id: project_id.into(),
            evaluator_release_id: evaluator_release_id.into(),
            annotation_schema_release_id: annotation_schema_release_id.into(),
            split_release_id: split_release_id.into(),
            review_selection_hash,
            agreement_report,
            ordinal_agreement_report,
            model,
            fit_report,
            fit_slice_reports,
            fit_annotation_revision_ids,
            created_by: created_by.into(),
            created_at_unix_ms: now,
        };
        let calibration_release_id = review_identity("perseval.calibration-release.v1", &release)?;
        transaction.execute(
            "INSERT INTO calibration_releases(
                calibration_release_id, project_id, evaluator_release_id,
                annotation_schema_release_id, release_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(calibration_release_id) DO NOTHING",
            params![
                calibration_release_id,
                project_id,
                evaluator_release_id,
                annotation_schema_release_id,
                serde_json::to_string(&release)?,
                now,
            ],
        )?;
        for member in &members {
            transaction.execute(
                "INSERT INTO calibration_release_members(
                    calibration_release_id, task_id, assessment_id,
                    leakage_group_id, split, member_role, member_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'fit', ?6)
                 ON CONFLICT(calibration_release_id, task_id) DO NOTHING",
                params![
                    calibration_release_id,
                    member.task_id,
                    member.assessment_id,
                    member.leakage_group_id,
                    calibration_split_name(member.split),
                    serde_json::to_string(member)?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok((calibration_release_id, release))
    }

    pub fn publish_calibration_test_report(
        &self,
        calibration_release_id: &str,
    ) -> Result<CalibrationReportV1, StoreError> {
        require_non_empty(calibration_release_id, "calibration_release_id")?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let release = load_json_by_id::<CalibrationReleaseV1>(
            &transaction,
            "SELECT release_json FROM calibration_releases WHERE calibration_release_id = ?1",
            calibration_release_id,
            "calibration release not found",
        )?;
        let (threshold_policy_release_id, policy_json) = transaction
            .query_row(
                "SELECT threshold_policy_release_id, release_json
                 FROM threshold_policy_releases
                 WHERE calibration_release_id = ?1",
                params![calibration_release_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid(
                    "freeze one threshold policy before evaluating held-out labels".into(),
                )
            })?;
        let policy: ThresholdPolicyReleaseV1 = serde_json::from_str(&policy_json)?;
        if let Some(existing_json) = transaction
            .query_row(
                "SELECT report_json FROM calibration_reports
                 WHERE calibration_release_id = ?1 AND split = 'test'",
                params![calibration_release_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(serde_json::from_str(&existing_json)?);
        }
        let mut members = calibration_members(
            &transaction,
            &release.project_id,
            &release.evaluator_release_id,
            &release.annotation_schema_release_id,
            &release.split_release_id,
            CalibrationDataSplitV1::Test,
        )?;
        for member in &mut members {
            member.slice_values =
                self.review_slice_values(&member.logical_trace_id, member.revision)?;
            member.slice_values.insert(
                "selection stream".into(),
                review_selection_reason_name(member.selection_reason).into(),
            );
        }
        members.retain(|member| member.label.is_failure().is_some());
        if members.is_empty() {
            return Err(StoreError::Invalid(
                "held-out calibration report requires resolved test cases".into(),
            ));
        }
        let model_id = release
            .model
            .model_id()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let predictions = members
            .iter_mut()
            .map(|member| {
                let probability = member
                    .features
                    .as_ref()
                    .map(|features| {
                        release
                            .model
                            .predict_failure_probability(features)
                            .map_err(|error| StoreError::Invalid(error.to_string()))
                    })
                    .transpose()?;
                member.calibrated_failure_probability = probability;
                Ok(BinaryPredictionV1 {
                    observation_id: member.task_id.clone(),
                    group_id: member.leakage_group_id.clone(),
                    evaluator_release_id: release.evaluator_release_id.clone(),
                    calibration_model_id: model_id.clone(),
                    split: CalibrationDataSplitV1::Test,
                    probability_failure: probability,
                    label_failure: member
                        .label
                        .is_failure()
                        .expect("retained decisive human label"),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let report = BinaryCalibrationReportV1::from_predictions(
            &predictions,
            policy.fail_probability_threshold,
            10,
        )
        .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let slice_reports =
            calibration_slice_reports(&members, &predictions, policy.fail_probability_threshold)?;
        let annotation_revision_ids = members
            .iter()
            .flat_map(|member| member.annotation_revision_ids.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let report_id = review_identity(
            "perseval.calibration-report.v1",
            &(
                calibration_release_id,
                &threshold_policy_release_id,
                &release.split_release_id,
                CalibrationDataSplitV1::Test,
                &report,
                &annotation_revision_ids,
            ),
        )?;
        let product_report = CalibrationReportV1 {
            schema_version: CALIBRATION_REPORT_SCHEMA_VERSION.into(),
            report_id: report_id.clone(),
            project_id: release.project_id,
            evaluator_release_id: release.evaluator_release_id,
            calibration_release_id: calibration_release_id.into(),
            threshold_policy_release_id,
            split_release_id: release.split_release_id,
            split: CalibrationDataSplitV1::Test,
            report,
            slice_reports,
            annotation_revision_ids,
            created_at_unix_ms: now,
        };
        transaction.execute(
            "INSERT INTO calibration_reports(
                report_id, project_id, evaluator_release_id, calibration_release_id,
                threshold_policy_release_id, split_release_id, split, report_json,
                created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'test', ?7, ?8)
             ON CONFLICT(report_id) DO NOTHING",
            params![
                product_report.report_id,
                product_report.project_id,
                product_report.evaluator_release_id,
                product_report.calibration_release_id,
                product_report.threshold_policy_release_id,
                product_report.split_release_id,
                serde_json::to_string(&product_report)?,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(product_report)
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
        created_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<(String, ThresholdPolicyReleaseV1), StoreError> {
        require_human(authority, "publish a threshold policy release")?;
        if !(0.0..=1.0).contains(&pass_probability_threshold)
            || !(0.0..=1.0).contains(&fail_probability_threshold)
            || pass_probability_threshold >= fail_probability_threshold
            || !(0.5..=1.0).contains(&minimum_decision_confidence)
        {
            return Err(StoreError::Invalid(
                "threshold policy requires 0 <= pass < fail <= 1 and confidence in [0.5, 1]".into(),
            ));
        }
        for (value, name) in [
            (project_id, "project_id"),
            (evaluator_release_id, "evaluator_release_id"),
            (calibration_release_id, "calibration_release_id"),
            (created_by, "created_by"),
        ] {
            require_non_empty(value, name)?;
        }
        let now = now_unix_ms();
        let control = self.control.lock().expect("control store lock poisoned");
        let release = load_json_by_id::<CalibrationReleaseV1>(
            &control,
            "SELECT release_json FROM calibration_releases WHERE calibration_release_id = ?1",
            calibration_release_id,
            "calibration release not found",
        )?;
        if release.project_id != project_id || release.evaluator_release_id != evaluator_release_id
        {
            return Err(StoreError::Invalid(
                "threshold policy calibration release is cross-project or evaluator-mismatched"
                    .into(),
            ));
        }
        if !matches!(
            release.agreement_report.krippendorff_alpha,
            Some(alpha) if alpha.is_finite() && alpha >= 0.67
        ) {
            return Err(StoreError::Invalid(
                "threshold policy publication is blocked until Krippendorff alpha is at least 0.67"
                    .into(),
            ));
        }
        let held_out_report_exists = control.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM calibration_reports
                WHERE calibration_release_id = ?1 AND split = 'test'
             )",
            params![calibration_release_id],
            |row| row.get::<_, bool>(0),
        )?;
        if held_out_report_exists {
            return Err(StoreError::Invalid(
                "thresholds are frozen before held-out evaluation; create a new calibration release instead of tuning on test labels".into(),
            ));
        }
        let existing_policy_exists = control.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM threshold_policy_releases
                WHERE calibration_release_id = ?1
             )",
            params![calibration_release_id],
            |row| row.get::<_, bool>(0),
        )?;
        if existing_policy_exists {
            return Err(StoreError::Invalid(
                "this calibration release already has its frozen threshold policy".into(),
            ));
        }
        let policy = ThresholdPolicyReleaseV1 {
            schema_version: THRESHOLD_POLICY_RELEASE_SCHEMA_VERSION.into(),
            project_id: project_id.into(),
            evaluator_release_id: evaluator_release_id.into(),
            calibration_release_id: calibration_release_id.into(),
            positive_class: "failure".into(),
            pass_probability_threshold,
            fail_probability_threshold,
            minimum_decision_confidence,
            created_by: created_by.into(),
            created_at_unix_ms: now,
        };
        let policy_id = review_identity("perseval.threshold-policy-release.v1", &policy)?;
        control.execute(
            "INSERT INTO threshold_policy_releases(
                threshold_policy_release_id, project_id, evaluator_release_id,
                calibration_release_id, release_json, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(threshold_policy_release_id) DO NOTHING",
            params![
                policy_id,
                project_id,
                evaluator_release_id,
                calibration_release_id,
                serde_json::to_string(&policy)?,
                now,
            ],
        )?;
        Ok((policy_id, policy))
    }

    pub fn activate_threshold_policy(
        &self,
        threshold_policy_release_id: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<ThresholdPolicyActivationV1, StoreError> {
        require_human(authority, "activate a threshold policy")?;
        require_non_empty(activated_by, "activated_by")?;
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let previous_activation_at = transaction.query_row(
            "SELECT COALESCE(MAX(activated_at_unix_ms), 0)
             FROM threshold_policy_activations",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let now = now_unix_ms().max(previous_activation_at.saturating_add(1));
        let policy = load_json_by_id::<ThresholdPolicyReleaseV1>(
            &transaction,
            "SELECT release_json FROM threshold_policy_releases
             WHERE threshold_policy_release_id = ?1",
            threshold_policy_release_id,
            "threshold policy release not found",
        )?;
        let held_out_report_json = transaction
            .query_row(
                "SELECT report_json FROM calibration_reports
             WHERE calibration_release_id = ?1
               AND threshold_policy_release_id = ?2
               AND split = 'test'",
                params![policy.calibration_release_id, threshold_policy_release_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid(
                    "threshold policy activation requires its one-shot held-out report".into(),
                )
            })?;
        let held_out_report: CalibrationReportV1 = serde_json::from_str(&held_out_report_json)?;
        let calibration = load_json_by_id::<CalibrationReleaseV1>(
            &transaction,
            "SELECT release_json FROM calibration_releases
             WHERE calibration_release_id = ?1",
            &policy.calibration_release_id,
            "calibration release not found",
        )?;
        let fit_random_audit = calibration_random_audit_report(&calibration.fit_slice_reports)?;
        let test_random_audit = calibration_random_audit_report(&held_out_report.slice_reports)?;
        let fit_confusion = &fit_random_audit.confusion;
        let test_confusion = &test_random_audit.confusion;
        let label_count = fit_random_audit
            .attempted_count
            .saturating_add(test_random_audit.attempted_count);
        let positive_count = fit_confusion
            .true_positive
            .saturating_add(fit_confusion.false_negative)
            .saturating_add(test_confusion.true_positive)
            .saturating_add(test_confusion.false_negative);
        let negative_count = fit_confusion
            .true_negative
            .saturating_add(fit_confusion.false_positive)
            .saturating_add(test_confusion.true_negative)
            .saturating_add(test_confusion.false_positive);
        if label_count < 500 || positive_count < 100 || negative_count < 100 {
            return Err(StoreError::Invalid(format!(
                "automation requires an unbiased random-audit cohort with at least 500 resolved labels and 100 per binary class; current random cohort has {label_count} labels ({positive_count} failure, {negative_count} non-failure)"
            )));
        }
        let quality = test_random_audit;
        let decision_coverage = if quality.attempted_count == 0 {
            0.0
        } else {
            quality.decided_count as f64 / quality.attempted_count as f64
        };
        let grouped_lower_bound_passes = quality
            .macro_f1_interval
            .as_ref()
            .is_some_and(|interval| interval.lower_95 > 0.053);
        let release_quality_passes = quality.average_precision.is_some_and(|value| value >= 0.65)
            && quality.macro_f1.is_some_and(|value| value >= 0.60)
            && quality.precision.is_some_and(|value| value >= 0.60)
            && quality.recall.is_some_and(|value| value >= 0.60)
            && quality.f1.is_some_and(|value| value > 0.206)
            && quality
                .matthews_correlation
                .is_some_and(|value| value > 0.200)
            && quality.brier_score.is_some_and(|value| value <= 0.20)
            && quality
                .expected_calibration_error
                .is_some_and(|value| value <= 0.08)
            && decision_coverage >= 0.90
            && grouped_lower_bound_passes;
        if !release_quality_passes {
            return Err(StoreError::Invalid(
                "automation quality gate requires the held-out random-audit slice to have AUPRC >= 0.65, macro F1 >= 0.60, precision/recall >= 0.60, F1 > 0.206, MCC > 0.200, Brier <= 0.20, ECE <= 0.08, decision coverage >= 90%, and a grouped-bootstrap macro-F1 lower 95% bound above the 0.053 prior-product and 0.0 all-negative baselines"
                    .into(),
            ));
        }
        let activation_id = review_identity(
            "perseval.threshold-policy-activation.v1",
            &(threshold_policy_release_id, activated_by, now),
        )?;
        let activation = ThresholdPolicyActivationV1 {
            schema_version: THRESHOLD_POLICY_ACTIVATION_SCHEMA_VERSION.into(),
            activation_id,
            project_id: policy.project_id,
            evaluator_release_id: policy.evaluator_release_id,
            threshold_policy_release_id: threshold_policy_release_id.into(),
            activated_by: activated_by.into(),
            activated_at_unix_ms: now,
        };
        transaction.execute(
            "INSERT INTO threshold_policy_activations(
                activation_id, project_id, evaluator_release_id,
                threshold_policy_release_id, activation_json, activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                activation.activation_id,
                activation.project_id,
                activation.evaluator_release_id,
                activation.threshold_policy_release_id,
                serde_json::to_string(&activation)?,
                now,
            ],
        )?;
        let assessment_ids = {
            let mut statement = transaction.prepare(
                "SELECT assessment_id FROM assessments
                 WHERE project_id = ?1 AND evaluator_release_id = ?2
                 ORDER BY created_at_unix_ms, assessment_id",
            )?;
            statement
                .query_map(
                    params![activation.project_id, activation.evaluator_release_id],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for assessment_id in assessment_ids {
            materialize_assessment_decision_in_transaction(
                &transaction,
                &assessment_id,
                threshold_policy_release_id,
                now,
            )?;
        }
        transaction.commit()?;
        Ok(activation)
    }

    pub fn materialize_assessment_decision(
        &self,
        assessment_id: &str,
        threshold_policy_release_id: &str,
    ) -> Result<AssessmentDecisionV1, StoreError> {
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        if assessment_blind_embargo(&transaction, assessment_id)?.is_some() {
            return Err(StoreError::Invalid(
                "calibrated decisions are withheld while blind calibration is sealed".into(),
            ));
        }
        let record = materialize_assessment_decision_in_transaction(
            &transaction,
            assessment_id,
            threshold_policy_release_id,
            now,
        )?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn assessment_decisions(
        &self,
        assessment_id: &str,
    ) -> Result<Vec<AssessmentDecisionV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        if assessment_blind_embargo(&control, assessment_id)?.is_some() {
            return Err(StoreError::Invalid(
                "calibrated decisions are withheld while blind calibration is sealed".into(),
            ));
        }
        let mut statement = control.prepare(
            "SELECT decision_json FROM assessment_decisions
             WHERE assessment_id = ?1 ORDER BY created_at_unix_ms, decision_id",
        )?;
        let encoded = statement
            .query_map(params![assessment_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        encoded
            .into_iter()
            .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .collect()
    }

    pub fn assessment_decision_count_for_policy(
        &self,
        threshold_policy_release_id: &str,
    ) -> Result<usize, StoreError> {
        require_non_empty(threshold_policy_release_id, "threshold_policy_release_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let count = control.query_row(
            "SELECT COUNT(*) FROM assessment_decisions
             WHERE threshold_policy_release_id = ?1",
            params![threshold_policy_release_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn list_review_queues(&self, project_id: &str) -> Result<Vec<ReviewQueueV1>, StoreError> {
        require_non_empty(project_id, "project_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT queue_json FROM review_queues
             WHERE project_id = ?1 ORDER BY created_at_unix_ms DESC, queue_id",
        )?;
        decode_json_rows(&mut statement, params![project_id])
    }

    pub fn review_split_release(
        &self,
        split_release_id: &str,
    ) -> Result<ReviewSplitReleaseV1, StoreError> {
        require_non_empty(split_release_id, "split_release_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        load_json_by_id(
            &control,
            "SELECT release_json FROM review_split_releases WHERE split_release_id = ?1",
            split_release_id,
            "review split release not found",
        )
    }

    pub fn list_review_tasks(
        &self,
        project_id: &str,
        mode: Option<ReviewModeV1>,
    ) -> Result<Vec<ReviewTaskV1>, StoreError> {
        require_non_empty(project_id, "project_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let mode = mode.map(review_mode_name);
        let mut statement = control.prepare(
            "SELECT t.task_json FROM review_tasks t
             JOIN review_queues q ON q.queue_id = t.queue_id
             WHERE t.project_id = ?1 AND (?2 IS NULL OR q.mode = ?2)
             ORDER BY CASE t.status
                WHEN 'awaiting_adjudication' THEN 0
                WHEN 'in_review' THEN 1
                WHEN 'pending' THEN 2
                WHEN 'completed' THEN 3
                ELSE 4 END,
                t.created_at_unix_ms, t.task_id",
        )?;
        decode_json_rows(&mut statement, params![project_id, mode])
    }

    pub fn list_calibration_releases(
        &self,
        project_id: &str,
        evaluator_release_id: Option<&str>,
    ) -> Result<Vec<(String, CalibrationReleaseV1)>, StoreError> {
        require_non_empty(project_id, "project_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT calibration_release_id, release_json FROM calibration_releases
             WHERE project_id = ?1 AND (?2 IS NULL OR evaluator_release_id = ?2)
             ORDER BY created_at_unix_ms DESC, calibration_release_id",
        )?;
        let rows = statement
            .query_map(params![project_id, evaluator_release_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, json)| {
                serde_json::from_str(&json)
                    .map(|release| (id, release))
                    .map_err(StoreError::from)
            })
            .collect()
    }

    pub fn list_calibration_reports(
        &self,
        calibration_release_id: &str,
    ) -> Result<Vec<CalibrationReportV1>, StoreError> {
        require_non_empty(calibration_release_id, "calibration_release_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT report_json FROM calibration_reports
             WHERE calibration_release_id = ?1 ORDER BY created_at_unix_ms DESC, report_id",
        )?;
        decode_json_rows(&mut statement, params![calibration_release_id])
    }

    pub fn threshold_policy_for_calibration(
        &self,
        calibration_release_id: &str,
    ) -> Result<Option<(String, ThresholdPolicyReleaseV1)>, StoreError> {
        require_non_empty(calibration_release_id, "calibration_release_id")?;
        let control = self.control.lock().expect("control store lock poisoned");
        let encoded = control
            .query_row(
                "SELECT threshold_policy_release_id, release_json
                 FROM threshold_policy_releases
                 WHERE calibration_release_id = ?1",
                params![calibration_release_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        encoded
            .map(|(id, json)| {
                serde_json::from_str(&json)
                    .map(|policy| (id, policy))
                    .map_err(StoreError::from)
            })
            .transpose()
    }

    pub fn active_threshold_policy(
        &self,
        project_id: &str,
        evaluator_release_id: &str,
    ) -> Result<Option<(ThresholdPolicyActivationV1, ThresholdPolicyReleaseV1)>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let encoded = control
            .query_row(
                "SELECT a.activation_json, p.release_json
                 FROM threshold_policy_activations a
                 JOIN threshold_policy_releases p
                   ON p.threshold_policy_release_id = a.threshold_policy_release_id
                 WHERE a.project_id = ?1 AND a.evaluator_release_id = ?2
                 ORDER BY a.activated_at_unix_ms DESC, a.activation_id DESC LIMIT 1",
                params![project_id, evaluator_release_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        encoded
            .map(|(activation, policy)| {
                Ok((
                    serde_json::from_str(&activation)?,
                    serde_json::from_str(&policy)?,
                ))
            })
            .transpose()
    }
}

pub(super) fn materialize_assessment_decision_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    assessment_id: &str,
    threshold_policy_release_id: &str,
    now: i64,
) -> Result<AssessmentDecisionV1, StoreError> {
    let policy = load_json_by_id::<ThresholdPolicyReleaseV1>(
        transaction,
        "SELECT release_json FROM threshold_policy_releases
         WHERE threshold_policy_release_id = ?1",
        threshold_policy_release_id,
        "threshold policy release not found",
    )?;
    let active_policy_id = transaction
        .query_row(
            "SELECT threshold_policy_release_id
             FROM threshold_policy_activations
             WHERE project_id = ?1 AND evaluator_release_id = ?2
             ORDER BY activated_at_unix_ms DESC, activation_id DESC
             LIMIT 1",
            params![policy.project_id, policy.evaluator_release_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if active_policy_id.as_deref() != Some(threshold_policy_release_id) {
        return Err(StoreError::Invalid(
            "assessment decisions can only be materialized with the active threshold policy".into(),
        ));
    }
    let calibration = load_json_by_id::<CalibrationReleaseV1>(
        transaction,
        "SELECT release_json FROM calibration_releases WHERE calibration_release_id = ?1",
        &policy.calibration_release_id,
        "calibration release not found",
    )?;
    let assessment = load_assessment_for_calibration(transaction, assessment_id)?;
    if assessment.0 != policy.project_id || assessment.1 != policy.evaluator_release_id {
        return Err(StoreError::Invalid(
            "assessment is cross-project or uses another evaluator release".into(),
        ));
    }
    let (probability, decision) = match assessment.2.as_ref() {
        Some(evaluation) if evaluation.verdict != LearnedVerdictV1::Abstain => {
            let features = calibration_features(transaction, assessment_id, evaluation)?;
            let probability = calibration
                .model
                .predict_failure_probability(&features)
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            let confidence = probability.max(1.0 - probability);
            let decision = if confidence < policy.minimum_decision_confidence {
                CalibratedDecisionV1::Review
            } else if probability <= policy.pass_probability_threshold {
                CalibratedDecisionV1::Pass
            } else if probability >= policy.fail_probability_threshold {
                CalibratedDecisionV1::Fail
            } else {
                CalibratedDecisionV1::Review
            };
            (Some(probability), decision)
        }
        _ => (None, CalibratedDecisionV1::Abstain),
    };
    let decision_id = review_identity(
        "perseval.assessment-decision.v1",
        &(
            assessment_id,
            &policy.calibration_release_id,
            threshold_policy_release_id,
        ),
    )?;
    let record = AssessmentDecisionV1 {
        schema_version: ASSESSMENT_DECISION_SCHEMA_VERSION.into(),
        decision_id,
        project_id: policy.project_id,
        evaluator_release_id: policy.evaluator_release_id,
        assessment_id: assessment_id.into(),
        calibration_release_id: policy.calibration_release_id,
        threshold_policy_release_id: threshold_policy_release_id.into(),
        calibrated_failure_probability: probability,
        decision,
        created_at_unix_ms: now,
    };
    transaction.execute(
        "INSERT INTO assessment_decisions(
            decision_id, assessment_id, calibration_release_id,
            threshold_policy_release_id, decision_json, created_at_unix_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(decision_id) DO NOTHING",
        params![
            record.decision_id,
            record.assessment_id,
            record.calibration_release_id,
            record.threshold_policy_release_id,
            serde_json::to_string(&record)?,
            now,
        ],
    )?;
    Ok(record)
}

pub(super) fn assessment_blind_embargo(
    connection: &Connection,
    assessment_id: &str,
) -> Result<Option<(String, String)>, StoreError> {
    connection
        .query_row(
            "SELECT t.task_id, t.queue_id
             FROM review_tasks t JOIN review_queues q ON q.queue_id = t.queue_id
             WHERE t.assessment_id = ?1 AND q.mode = 'blind_calibration'
               AND t.status != 'cancelled'
               AND (
                    t.status != 'completed'
                    OR (
                        t.split = 'test'
                        AND NOT EXISTS(
                            SELECT 1
                            FROM calibration_releases c
                            JOIN threshold_policy_releases p
                              ON p.calibration_release_id = c.calibration_release_id
                            JOIN review_split_groups calibration_group
                              ON calibration_group.split_release_id = json_extract(c.release_json, '$.split_release_id')
                             AND calibration_group.leakage_group_id = t.leakage_group_id
                             AND calibration_group.split = t.split
                            WHERE c.project_id = t.project_id
                              AND c.evaluator_release_id = q.evaluator_release_id
                              AND c.annotation_schema_release_id = q.annotation_schema_release_id
                        )
                    )
               )
             ORDER BY t.created_at_unix_ms, t.task_id LIMIT 1",
            params![assessment_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(StoreError::from)
}

fn blind_task_model_reveal_allowed(
    control: &std::sync::Mutex<Connection>,
    task: &ReviewTaskV1,
    queue: &ReviewQueueV1,
) -> Result<bool, StoreError> {
    if task.status != ReviewTaskStatusV1::Completed {
        return Ok(false);
    }
    if task.split != CalibrationDataSplitV1::Test {
        return Ok(true);
    }
    let connection = control.lock().expect("control store lock poisoned");
    let threshold_is_frozen = connection.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM calibration_releases c
            JOIN threshold_policy_releases p
              ON p.calibration_release_id = c.calibration_release_id
            JOIN review_split_groups calibration_group
              ON calibration_group.split_release_id = json_extract(c.release_json, '$.split_release_id')
             AND calibration_group.leakage_group_id = ?4
             AND calibration_group.split = 'test'
            WHERE c.project_id = ?1
              AND c.evaluator_release_id = ?2
              AND c.annotation_schema_release_id = ?3
         )",
        params![
            task.project_id,
            queue.evaluator_release_id,
            queue.annotation_schema_release_id,
            task.leakage_group_id,
        ],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(threshold_is_frozen)
}

fn validate_annotation_schema(release: &AnnotationSchemaReleaseV1) -> Result<(), StoreError> {
    if release.schema_version != ANNOTATION_SCHEMA_RELEASE_SCHEMA_VERSION
        || release.project_id.trim().is_empty()
        || release.instructions.trim().is_empty()
        || release.created_by.trim().is_empty()
        || release.required_reviewers < 2
        || release.positive_class != "task_failure_or_partial"
    {
        return Err(StoreError::Invalid(
            "invalid annotation schema release".into(),
        ));
    }
    let labels = release.labels.iter().copied().collect::<BTreeSet<_>>();
    let required = [
        AnnotationLabelV1::Completed,
        AnnotationLabelV1::Partial,
        AnnotationLabelV1::Failed,
        AnnotationLabelV1::Abstain,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if labels != required || release.labels.len() != required.len() {
        return Err(StoreError::Invalid(
            "task-completion annotation schema must declare each v1 label exactly once".into(),
        ));
    }
    Ok(())
}

fn validate_review_answer(
    label: AnnotationLabelV1,
    explanation: &str,
    evidence_keys: &[String],
) -> Result<(), StoreError> {
    if explanation.trim().is_empty()
        || (label != AnnotationLabelV1::Abstain && evidence_keys.is_empty())
    {
        return Err(StoreError::Invalid(
            "annotations require an explanation and decisive labels require evidence".into(),
        ));
    }
    let mut unique = BTreeSet::new();
    if evidence_keys
        .iter()
        .any(|key| key.trim().is_empty() || !unique.insert(key))
    {
        return Err(StoreError::Invalid(
            "annotation evidence keys must be unique and non-empty".into(),
        ));
    }
    Ok(())
}

fn validate_evidence_keys(
    connection: &Connection,
    case_id: &str,
    evidence_keys: &[String],
) -> Result<(), StoreError> {
    for evidence_key in evidence_keys {
        let exists = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM annotation_case_evidence
                WHERE case_id = ?1 AND evidence_key = ?2
             )",
            params![case_id, evidence_key],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(StoreError::Invalid(format!(
                "annotation references unknown assessment evidence key {evidence_key}"
            )));
        }
    }
    Ok(())
}

fn review_evidence_keys(connection: &Connection, case_id: &str) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT evidence_key FROM annotation_case_evidence
         WHERE case_id = ?1 ORDER BY evidence_key ASC",
    )?;
    Ok(statement
        .query_map(params![case_id], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?)
}

fn validate_calibration_dependencies(
    connection: &Connection,
    project_id: &str,
    evaluator_release_id: &str,
    annotation_schema_release_id: &str,
    split_release_id: &str,
) -> Result<(), StoreError> {
    let matches = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM evaluator_releases e
            JOIN annotation_schema_releases s ON s.project_id = e.project_id
            JOIN review_split_releases r
              ON r.project_id = s.project_id
             AND r.annotation_schema_release_id = s.annotation_schema_release_id
            WHERE e.project_id = ?1 AND e.evaluator_release_id = ?2
              AND s.annotation_schema_release_id = ?3 AND r.split_release_id = ?4
         )",
        params![
            project_id,
            evaluator_release_id,
            annotation_schema_release_id,
            split_release_id
        ],
        |row| row.get::<_, bool>(0),
    )?;
    if !matches {
        return Err(StoreError::Invalid(
            "calibration dependencies are missing or cross-project".into(),
        ));
    }
    Ok(())
}

fn calibration_members(
    connection: &Connection,
    project_id: &str,
    evaluator_release_id: &str,
    annotation_schema_release_id: &str,
    split_release_id: &str,
    split: CalibrationDataSplitV1,
) -> Result<Vec<CalibrationMemberV1>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT t.task_json
         FROM review_tasks t
         JOIN review_queues q ON q.queue_id = t.queue_id
         JOIN review_split_groups target_split
           ON target_split.split_release_id = ?4
          AND target_split.leakage_group_id = t.leakage_group_id
          AND target_split.split = t.split
         WHERE t.project_id = ?1 AND q.evaluator_release_id = ?2
           AND q.annotation_schema_release_id = ?3
           AND q.mode = 'blind_calibration' AND t.split = ?5 AND t.status = 'completed'
         ORDER BY t.task_id",
    )?;
    let encoded = statement
        .query_map(
            params![
                project_id,
                evaluator_release_id,
                annotation_schema_release_id,
                split_release_id,
                calibration_split_name(split)
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut members = Vec::new();
    let mut seen_case_ids = BTreeSet::new();
    for value in encoded {
        let task: ReviewTaskV1 = serde_json::from_str(&value)?;
        if !seen_case_ids.insert(task.case_id.clone()) {
            continue;
        }
        let Some(truth) = resolved_truth(connection, &task)? else {
            continue;
        };
        let (assessment_project, assessment_evaluator, evaluation) =
            load_assessment_for_calibration(connection, &task.assessment_id)?;
        if assessment_project != project_id || assessment_evaluator != evaluator_release_id {
            return Err(StoreError::Invalid(
                "calibration task assessment changed project or evaluator".into(),
            ));
        }
        let features = evaluation
            .as_ref()
            .filter(|evaluation| evaluation.verdict != LearnedVerdictV1::Abstain)
            .map(|evaluation| calibration_features(connection, &task.assessment_id, evaluation))
            .transpose()?;
        members.push(CalibrationMemberV1 {
            task_id: task.task_id,
            assessment_id: task.assessment_id,
            logical_trace_id: task.logical_trace_id,
            revision: task.revision,
            leakage_group_id: task.leakage_group_id,
            selection_reason: task.selection_reason,
            split,
            annotation_revision_ids: truth.annotation_revision_ids,
            adjudication_revision_id: truth.adjudication_revision_id,
            label: truth.label,
            features,
            calibrated_failure_probability: None,
            slice_values: BTreeMap::new(),
        });
    }
    Ok(members)
}

fn calibration_slice_scalar(value: &serde_json::Value) -> Option<String> {
    let rendered = match value {
        serde_json::Value::String(value) => value.trim().to_owned(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    (!rendered.is_empty()).then_some(rendered)
}

const fn review_selection_reason_name(reason: ReviewSelectionReasonV1) -> &'static str {
    match reason {
        ReviewSelectionReasonV1::RandomAudit => "random audit",
        ReviewSelectionReasonV1::ActiveLearning => "active selection",
        ReviewSelectionReasonV1::Manual => "manual",
    }
}

fn calibration_slice_reports(
    members: &[CalibrationMemberV1],
    predictions: &[BinaryPredictionV1],
    decision_threshold: f64,
) -> Result<Vec<CalibrationSliceReportV1>, StoreError> {
    if members.len() != predictions.len() {
        return Err(StoreError::Invalid(
            "calibration slice inputs do not reconcile with predictions".into(),
        ));
    }
    let mut grouped = BTreeMap::<(String, String), Vec<BinaryPredictionV1>>::new();
    for (member, prediction) in members.iter().zip(predictions) {
        for (dimension, value) in &member.slice_values {
            grouped
                .entry((dimension.clone(), value.clone()))
                .or_default()
                .push(prediction.clone());
        }
    }
    grouped
        .into_iter()
        .map(|((dimension, value), predictions)| {
            let report =
                BinaryCalibrationReportV1::from_predictions(&predictions, decision_threshold, 10)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?;
            Ok(CalibrationSliceReportV1 {
                dimension,
                value,
                report,
            })
        })
        .collect()
}

fn calibration_random_audit_report(
    slices: &[CalibrationSliceReportV1],
) -> Result<&BinaryCalibrationReportV1, StoreError> {
    slices
        .iter()
        .find(|slice| slice.dimension == "selection stream" && slice.value == "random audit")
        .map(|slice| &slice.report)
        .ok_or_else(|| {
            StoreError::Invalid(
                "automation requires an independently sampled random-audit slice".into(),
            )
        })
}

fn agreement_ratings(
    connection: &Connection,
    task_ids: &BTreeSet<&str>,
) -> Result<Vec<AgreementRatingV1>, StoreError> {
    let mut ratings = Vec::new();
    for task_id in task_ids {
        for annotation in current_annotations(connection, task_id)? {
            ratings.push(AgreementRatingV1 {
                item_id: annotation.case_id,
                rater_id: annotation.reviewer_id,
                label: annotation_label_name(annotation.label).into(),
            });
        }
    }
    ratings.sort_by(|left, right| {
        (&left.item_id, &left.rater_id).cmp(&(&right.item_id, &right.rater_id))
    });
    Ok(ratings)
}

fn calibration_features(
    connection: &Connection,
    assessment_id: &str,
    evaluation: &LearnedEvaluationV1,
) -> Result<LearnedCalibrationFeaturesV1, StoreError> {
    let completion_score = evaluation.score.ok_or_else(|| {
        StoreError::Invalid("decisive assessment is missing its raw completion score".into())
    })?;
    let projection_json: String = connection.query_row(
        "SELECT p.projection_json
         FROM assessments a JOIN assessment_projections p
           ON p.projection_hash = a.projection_hash
         WHERE a.assessment_id = ?1",
        params![assessment_id],
        |row| row.get(0),
    )?;
    let projection: TaskCompletionProjectionV1 = serde_json::from_str(&projection_json)?;
    let total_evidence = projection.evidence_catalog.entries.len();
    let cited_evidence = evaluation
        .evidence
        .iter()
        .map(|citation| citation.evidence_key.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let evidence_coverage = if total_evidence == 0 {
        0.0
    } else {
        (cited_evidence as f64 / total_evidence as f64).min(1.0)
    };
    Ok(LearnedCalibrationFeaturesV1 {
        normalized_failure_score: 1.0 - completion_score,
        model_reported_confidence: evaluation.model_reported_confidence,
        evidence_coverage,
        projection_truncated: projection.truncated,
        evaluator_disagreement: None,
        missing_telemetry: 0.0,
        out_of_distribution_score: None,
    })
}

fn load_assessment_for_calibration(
    connection: &Connection,
    assessment_id: &str,
) -> Result<(String, String, Option<LearnedEvaluationV1>), StoreError> {
    let encoded = connection
        .query_row(
            "SELECT project_id, evaluator_release_id, evaluation_json
             FROM assessments WHERE assessment_id = ?1",
            params![assessment_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::Invalid("assessment not found".into()))?;
    Ok((
        encoded.0,
        encoded.1,
        encoded
            .2
            .map(|json| serde_json::from_str(&json))
            .transpose()?,
    ))
}

fn resolved_truth(
    connection: &Connection,
    task: &ReviewTaskV1,
) -> Result<Option<ResolvedTruth>, StoreError> {
    let annotations = current_annotations(connection, &task.task_id)?;
    if annotations.len() < task.required_reviewers as usize {
        return Ok(None);
    }
    let annotation_revision_ids = annotations
        .iter()
        .map(|annotation| annotation.revision_id.clone())
        .collect::<Vec<_>>();
    if annotations
        .iter()
        .all(|annotation| annotation.label == annotations[0].label)
    {
        return Ok(Some(ResolvedTruth {
            label: annotations[0].label,
            annotation_revision_ids,
            adjudication_revision_id: None,
        }));
    }
    let adjudication_id = review_identity("perseval.adjudication.v1", &task.case_id)?;
    let Some(adjudication) = latest_adjudication(connection, &adjudication_id)? else {
        return Ok(None);
    };
    if as_set(&adjudication.annotation_revision_ids) != as_set(&annotation_revision_ids) {
        return Ok(None);
    }
    Ok(Some(ResolvedTruth {
        label: adjudication.label,
        annotation_revision_ids,
        adjudication_revision_id: Some(adjudication.revision_id),
    }))
}

fn resolve_task_status(
    connection: &Connection,
    task: &ReviewTaskV1,
) -> Result<ReviewTaskStatusV1, StoreError> {
    let annotations = current_annotations(connection, &task.task_id)?;
    if annotations.len() < task.required_reviewers as usize {
        return Ok(ReviewTaskStatusV1::InReview);
    }
    if annotations
        .iter()
        .all(|annotation| annotation.label == annotations[0].label)
    {
        return Ok(ReviewTaskStatusV1::Completed);
    }
    Ok(if resolved_truth(connection, task)?.is_some() {
        ReviewTaskStatusV1::Completed
    } else {
        ReviewTaskStatusV1::AwaitingAdjudication
    })
}

fn current_annotations(
    connection: &Connection,
    task_id: &str,
) -> Result<Vec<AnnotationRevisionV1>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT a.annotation_json
         FROM review_assignments r JOIN annotation_revisions a
           ON a.revision_id = r.submitted_annotation_revision_id
         WHERE r.task_id = ?1 ORDER BY r.reviewer_ordinal",
    )?;
    let encoded = statement
        .query_map(params![task_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    encoded
        .into_iter()
        .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
        .collect()
}

fn current_annotation_revision_ids(
    connection: &Connection,
    task_id: &str,
) -> Result<Vec<String>, StoreError> {
    Ok(current_annotations(connection, task_id)?
        .into_iter()
        .map(|annotation| annotation.revision_id)
        .collect())
}

fn assigned_reviewer_ids(
    connection: &Connection,
    task_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT reviewer_id FROM review_assignments
         WHERE task_id = ?1 ORDER BY reviewer_ordinal",
    )?;
    statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn latest_annotation_for_logical(
    connection: &Connection,
    annotation_id: &str,
) -> Result<Option<AnnotationRevisionV1>, StoreError> {
    load_optional_json(
        connection,
        "SELECT annotation_json FROM annotation_revisions
         WHERE annotation_id = ?1 ORDER BY annotation_revision DESC LIMIT 1",
        annotation_id,
    )
}

fn latest_annotation_for_reviewer(
    connection: &Connection,
    task_id: &str,
    reviewer_id: &str,
) -> Result<Option<AnnotationRevisionV1>, StoreError> {
    let revision_id = connection
        .query_row(
            "SELECT submitted_annotation_revision_id FROM review_assignments
             WHERE task_id = ?1 AND reviewer_id = ?2",
            params![task_id, reviewer_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    revision_id
        .as_deref()
        .map(|id| {
            load_json_by_id(
                connection,
                "SELECT annotation_json FROM annotation_revisions WHERE revision_id = ?1",
                id,
                "annotation revision not found",
            )
        })
        .transpose()
}

fn latest_adjudication(
    connection: &Connection,
    adjudication_id: &str,
) -> Result<Option<AdjudicationV1>, StoreError> {
    load_optional_json(
        connection,
        "SELECT adjudication_json FROM adjudication_revisions
         WHERE adjudication_id = ?1 ORDER BY adjudication_revision DESC LIMIT 1",
        adjudication_id,
    )
}

fn annotation_schema_for_task(
    connection: &Connection,
    task_id: &str,
) -> Result<String, StoreError> {
    connection
        .query_row(
            "SELECT q.annotation_schema_release_id
             FROM review_tasks t JOIN review_queues q ON q.queue_id = t.queue_id
             WHERE t.task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn load_assignment(
    connection: &Connection,
    task_id: &str,
    reviewer_id: &str,
) -> Result<Option<ReviewAssignmentV1>, StoreError> {
    connection
        .query_row(
            "SELECT assigned_at_unix_ms, submitted_annotation_revision_id
             FROM review_assignments WHERE task_id = ?1 AND reviewer_id = ?2",
            params![task_id, reviewer_id],
            |row| {
                Ok(ReviewAssignmentV1 {
                    task_id: task_id.into(),
                    reviewer_id: reviewer_id.into(),
                    assigned_at_unix_ms: row.get(0)?,
                    submitted_annotation_revision_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn load_task(connection: &Connection, task_id: &str) -> Result<ReviewTaskV1, StoreError> {
    load_json_by_id(
        connection,
        "SELECT task_json FROM review_tasks WHERE task_id = ?1",
        task_id,
        "review task not found",
    )
}

fn load_queue(connection: &Connection, queue_id: &str) -> Result<ReviewQueueV1, StoreError> {
    load_json_by_id(
        connection,
        "SELECT queue_json FROM review_queues WHERE queue_id = ?1",
        queue_id,
        "review queue not found",
    )
}

fn load_json_by_id<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
    missing: &str,
) -> Result<T, StoreError> {
    let encoded = connection
        .query_row(sql, params![id], |row| row.get::<_, String>(0))
        .optional()?
        .ok_or_else(|| StoreError::Invalid(missing.into()))?;
    serde_json::from_str(&encoded).map_err(StoreError::from)
}

fn load_optional_json<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
) -> Result<Option<T>, StoreError> {
    connection
        .query_row(sql, params![id], |row| row.get::<_, String>(0))
        .optional()?
        .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
        .transpose()
}

fn decode_json_rows<T: serde::de::DeserializeOwned, P: rusqlite::Params>(
    statement: &mut rusqlite::Statement<'_>,
    parameters: P,
) -> Result<Vec<T>, StoreError> {
    let encoded = statement
        .query_map(parameters, |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    encoded
        .into_iter()
        .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
        .collect()
}

fn persist_task_status(connection: &Connection, task: &ReviewTaskV1) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE review_tasks SET status = ?2, task_json = ?3 WHERE task_id = ?1",
        params![
            task.task_id,
            review_task_status_name(task.status),
            serde_json::to_string(task)?,
        ],
    )?;
    Ok(())
}

fn require_human(authority: ReviewAuthorityV1, action: &str) -> Result<(), StoreError> {
    if authority != ReviewAuthorityV1::Human {
        return Err(StoreError::Invalid(format!(
            "only a human reviewer can {action}"
        )));
    }
    Ok(())
}

fn require_non_empty(value: &str, field: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(StoreError::Invalid(format!("{field} must not be empty")));
    }
    Ok(())
}

fn require_sha256(value: &str, field: &str) -> Result<(), StoreError> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(StoreError::Invalid(format!(
            "{field} must be a sha256 content identity"
        )));
    }
    Ok(())
}

fn review_identity<T: Serialize>(domain: &str, value: &T) -> Result<String, StoreError> {
    canonical_content_id(domain, value).map_err(|error| StoreError::Invalid(error.to_string()))
}

fn as_set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

fn review_mode_name(mode: ReviewModeV1) -> &'static str {
    match mode {
        ReviewModeV1::BlindCalibration => "blind_calibration",
        ReviewModeV1::VisibleTriage => "visible_triage",
    }
}

fn review_task_status_name(status: ReviewTaskStatusV1) -> &'static str {
    match status {
        ReviewTaskStatusV1::Pending => "pending",
        ReviewTaskStatusV1::InReview => "in_review",
        ReviewTaskStatusV1::AwaitingAdjudication => "awaiting_adjudication",
        ReviewTaskStatusV1::Completed => "completed",
        ReviewTaskStatusV1::Cancelled => "cancelled",
    }
}

fn annotation_label_name(label: AnnotationLabelV1) -> &'static str {
    match label {
        AnnotationLabelV1::Completed => "completed",
        AnnotationLabelV1::Partial => "partial",
        AnnotationLabelV1::Failed => "failed",
        AnnotationLabelV1::Abstain => "abstain",
    }
}

fn calibration_split_name(split: CalibrationDataSplitV1) -> &'static str {
    match split {
        CalibrationDataSplitV1::Train => "train",
        CalibrationDataSplitV1::Calibration => "calibration",
        CalibrationDataSplitV1::Test => "test",
    }
}

fn parse_calibration_split(value: &str) -> Result<CalibrationDataSplitV1, StoreError> {
    match value {
        "train" => Ok(CalibrationDataSplitV1::Train),
        "calibration" => Ok(CalibrationDataSplitV1::Calibration),
        "test" => Ok(CalibrationDataSplitV1::Test),
        _ => Err(StoreError::Invalid(format!(
            "unknown calibration split {value}"
        ))),
    }
}

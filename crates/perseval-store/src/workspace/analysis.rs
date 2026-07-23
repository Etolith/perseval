mod evals;
mod inbox;
mod runtime;

use super::*;
use crate::model::{
    FailureRecurrenceBucketV1, FailureRecurrenceSeriesV1, QueryScopeCriteriaV1, QueryScopeV1,
};

fn finding_severity_name(severity: FindingSeverity) -> &'static str {
    match severity {
        FindingSeverity::Info => "info",
        FindingSeverity::Low => "low",
        FindingSeverity::Medium => "medium",
        FindingSeverity::High => "high",
        FindingSeverity::Critical => "critical",
    }
}

fn finding_recovery_name(recovery: RecoveryStatus) -> &'static str {
    match recovery {
        RecoveryStatus::Recovered => "recovered",
        RecoveryStatus::Unrecovered => "unrecovered",
        RecoveryStatus::Unknown => "unknown",
    }
}

fn refresh_failure_membership_dispositions(
    transaction: &rusqlite::Transaction<'_>,
    logical_trace_id: &str,
    group_id: &str,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE active_failure_group_memberships AS membership SET
            confirmed_count = (SELECT COUNT(*) FROM active_failure_findings finding
                JOIN finding_dispositions disposition USING(finding_id)
               WHERE finding.logical_trace_id = membership.logical_trace_id
                 AND finding.group_id = membership.group_id
                 AND disposition.analysis_id = finding.analysis_id
                 AND disposition.detector_id = finding.detector_id
                 AND disposition.detector_version = finding.detector_version
                 AND disposition.state = 'confirmed'),
            dismissed_count = (SELECT COUNT(*) FROM active_failure_findings finding
                JOIN finding_dispositions disposition USING(finding_id)
               WHERE finding.logical_trace_id = membership.logical_trace_id
                 AND finding.group_id = membership.group_id
                 AND disposition.analysis_id = finding.analysis_id
                 AND disposition.detector_id = finding.detector_id
                 AND disposition.detector_version = finding.detector_version
                 AND disposition.state = 'dismissed'),
            needs_context_count = (SELECT COUNT(*) FROM active_failure_findings finding
                JOIN finding_dispositions disposition USING(finding_id)
               WHERE finding.logical_trace_id = membership.logical_trace_id
                 AND finding.group_id = membership.group_id
                 AND disposition.analysis_id = finding.analysis_id
                 AND disposition.detector_id = finding.detector_id
                 AND disposition.detector_version = finding.detector_version
                 AND disposition.state = 'needs_context'),
            stale_disposition_count = (SELECT COUNT(*) FROM active_failure_findings finding
                JOIN finding_dispositions disposition USING(finding_id)
               WHERE finding.logical_trace_id = membership.logical_trace_id
                 AND finding.group_id = membership.group_id
                 AND (disposition.analysis_id <> finding.analysis_id
                      OR disposition.detector_id <> finding.detector_id
                      OR disposition.detector_version <> finding.detector_version)),
            unreviewed_count = (SELECT COUNT(*) FROM active_failure_findings finding
                LEFT JOIN finding_dispositions disposition USING(finding_id)
               WHERE finding.logical_trace_id = membership.logical_trace_id
                 AND finding.group_id = membership.group_id
                 AND (disposition.finding_id IS NULL
                      OR disposition.analysis_id <> finding.analysis_id
                      OR disposition.detector_id <> finding.detector_id
                      OR disposition.detector_version <> finding.detector_version))
          WHERE membership.logical_trace_id = ?1 AND membership.group_id = ?2",
        params![logical_trace_id, group_id],
    )?;
    Ok(())
}

impl WorkspaceStore {
    fn load_candidate(&self, finding_id: &str) -> Result<Option<EvalCandidate>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let json = control.query_row(
            "SELECT candidate_json FROM eval_candidates WHERE finding_id = ?1 ORDER BY created_at_unix_ms DESC LIMIT 1",
            params![finding_id],
            |row| row.get::<_, String>(0),
        ).optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    fn load_active_analyses(&self) -> Result<Vec<StoredAnalysis>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT a.logical_trace_id, a.revision, a.behavior_json, a.findings_json
             FROM active_analysis_runs active
             JOIN analysis_runs a ON a.analysis_id = active.analysis_id
             ORDER BY a.committed_at_unix_ms, a.logical_trace_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .map(|row| {
                let (logical_trace_id, revision, behavior_json, findings_json) = row?;
                Ok(StoredAnalysis {
                    logical_trace_id,
                    revision: revision as u64,
                    behavior: serde_json::from_str(&behavior_json)?,
                    findings: serde_json::from_str(&findings_json)?,
                })
            })
            .collect()
    }
}

struct StoredAnalysis {
    logical_trace_id: String,
    revision: u64,
    behavior: AgentBehaviorTrace,
    findings: Vec<BehaviorFinding>,
}

fn candidate_parts<'a>(
    analyses: &'a [StoredAnalysis],
    finding_id: &str,
) -> Option<(
    &'a StoredAnalysis,
    &'a BehaviorFinding,
    traces_to_evals::EvidencePacket,
    EvalCandidate,
)> {
    let (analysis, finding) = analyses.iter().find_map(|analysis| {
        analysis
            .findings
            .iter()
            .find(|finding| finding.finding_id == finding_id)
            .map(|finding| (analysis, finding))
    })?;
    let packet = EvidencePacketBuilder.build(
        std::slice::from_ref(&analysis.behavior),
        std::slice::from_ref(finding),
    );
    let candidate = FindingEvalCandidateGenerator.generate_with_evidence_packet(
        &analysis.behavior,
        finding,
        &packet,
    );
    Some((analysis, finding, packet, candidate))
}

fn trace_span_kind(category: &str) -> SpanKind {
    traces_to_evals::semantic_span_kind(category)
}

fn validate_eval_batch_request(
    store: &WorkspaceStore,
    project_id: &str,
    selection_spec: &EvalBatchSelectionSpecV1,
) -> Result<(), StoreError> {
    selection_spec
        .scope
        .validate()
        .map_err(StoreError::Invalid)?;
    if selection_spec.scope.criteria.project_id.as_deref() != Some(project_id) {
        return Err(StoreError::Invalid(
            "eval project does not match the immutable query scope".into(),
        ));
    }
    if project_id.trim().is_empty() || matches!(project_id, "all-projects" | UNASSIGNED_PROJECT_ID)
    {
        return Err(StoreError::Invalid(
            "eval generation requires one explicit persisted project".into(),
        ));
    }
    if selection_spec.group_ids.is_empty() || selection_spec.group_ids.len() > 100 {
        return Err(StoreError::Invalid(
            "eval batch must contain between 1 and 100 failure groups".into(),
        ));
    }
    if !(1..=16).contains(&selection_spec.policy.maximum_examples_per_group) {
        return Err(StoreError::Invalid(
            "maximum_examples_per_group must be between 1 and 16".into(),
        ));
    }
    if selection_spec
        .group_ids
        .iter()
        .any(|group_id| group_id.trim().is_empty())
    {
        return Err(StoreError::Invalid(
            "failure group IDs must not be empty".into(),
        ));
    }
    let control = store.control.lock().expect("control store lock poisoned");
    let exists = control.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE workspace_id = ?1 AND project_id = ?2)",
        params![store.workspace_id, project_id],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        return Err(StoreError::Invalid(format!(
            "project '{project_id}' does not exist in this workspace"
        )));
    }
    Ok(())
}

fn select_eval_representatives<'a>(
    members: &[(&'a StoredAnalysis, &'a BehaviorFinding)],
    policy: &crate::model::EvalEvidenceSelectionPolicyV1,
    maximum: usize,
) -> Vec<(
    &'a StoredAnalysis,
    &'a BehaviorFinding,
    EvalSelectionReasonV1,
)> {
    let mut selected = Vec::new();
    let mut selected_ids = HashSet::new();
    let mut push = |member: (&'a StoredAnalysis, &'a BehaviorFinding), reason| {
        if selected.len() < maximum && selected_ids.insert(member.1.finding_id.as_str()) {
            selected.push((member.0, member.1, reason));
        }
    };
    if let Some(canonical) = members
        .iter()
        .copied()
        .min_by(|left, right| left.1.finding_id.cmp(&right.1.finding_id))
    {
        push(canonical, EvalSelectionReasonV1::CanonicalMember);
    }
    if policy.include_newest_unrecovered
        && let Some(member) = newest_member(
            members
                .iter()
                .copied()
                .filter(|(_, finding)| finding.recovery == RecoveryStatus::Unrecovered),
        )
    {
        push(member, EvalSelectionReasonV1::NewestUnrecovered);
    }
    if policy.include_recovered
        && let Some(member) = newest_member(
            members
                .iter()
                .copied()
                .filter(|(_, finding)| finding.recovery == RecoveryStatus::Recovered),
        )
    {
        push(member, EvalSelectionReasonV1::RecoveredExample);
    }
    if policy.include_distinct_builds {
        let mut builds = BTreeMap::new();
        for member in members.iter().copied() {
            if let Some(build) = member_identity(
                member,
                &[
                    "agent.build.id",
                    "agent.version",
                    "service.version",
                    "build.id",
                ],
            ) {
                let replace = builds
                    .get(&build)
                    .is_none_or(|existing: &&BehaviorFinding| {
                        member.1.created_at > existing.created_at
                            || (member.1.created_at == existing.created_at
                                && member.1.finding_id < existing.finding_id)
                    });
                if replace {
                    builds.insert(build, member.1);
                }
            }
        }
        for finding in builds.into_values() {
            if let Some(member) = members
                .iter()
                .copied()
                .find(|(_, candidate)| candidate.finding_id == finding.finding_id)
            {
                push(member, EvalSelectionReasonV1::DistinctAgentBuild);
            }
        }
    }
    if policy.include_distinct_shapes {
        let mut shapes = BTreeMap::new();
        for member in members.iter().copied() {
            if let Some(shape) = member_identity(
                member,
                &[
                    "execution_shape_fingerprint",
                    "perseval.execution_shape",
                    "trace.shape",
                ],
            ) {
                shapes.entry(shape).or_insert(member);
            }
        }
        for member in shapes.into_values() {
            push(member, EvalSelectionReasonV1::DistinctExecutionShape);
        }
    }
    selected
}

fn newest_member<'a>(
    members: impl Iterator<Item = (&'a StoredAnalysis, &'a BehaviorFinding)>,
) -> Option<(&'a StoredAnalysis, &'a BehaviorFinding)> {
    members.max_by(|left, right| {
        left.1
            .created_at
            .cmp(&right.1.created_at)
            .then_with(|| right.1.finding_id.cmp(&left.1.finding_id))
    })
}

fn member_identity(member: (&StoredAnalysis, &BehaviorFinding), keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        member
            .1
            .metadata
            .get(*key)
            .or_else(|| member.0.behavior.metadata.get(*key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    })
}

fn eval_batch_selection_hash(
    project_id: &str,
    selection_spec: &EvalBatchSelectionSpecV1,
    items: &[EvalBatchItemPreviewV1],
) -> Result<String, StoreError> {
    let identities = items
        .iter()
        .map(|item| {
            (
                &item.group_id,
                &item.finding_id,
                &item.logical_trace_id,
                item.revision,
                &item.candidate.candidate_id,
                &item.evidence_packet.packet_id,
            )
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_vec(&(project_id, selection_spec, identities))?;
    let mut hasher = Sha256::new();
    hasher.update(b"perseval.eval_batch.selection.v1");
    hasher.update(encoded);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn candidate_generation_job_id(project_id: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"perseval.candidate_generation_job.v1");
    hasher.update(project_id.len().to_be_bytes());
    hasher.update(project_id.as_bytes());
    hasher.update(idempotency_key.len().to_be_bytes());
    hasher.update(idempotency_key.as_bytes());
    format!("eval-job:sha256:{}", hex::encode(hasher.finalize()))
}

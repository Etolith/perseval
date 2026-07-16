use std::collections::BTreeMap;

use perseval_service::{
    CandidateGenerationJobV1, FailureGroupDetail, FailureGroupSummary, FailureOccurrence,
    FindingEvidence, ProjectV1, RunSummary, SpanRow,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) fn project(project: &ProjectV1) -> Value {
    json!({
        "project_id": project.project_id,
        "display_name": project.display_name,
        "artifact_namespace": project.artifact_namespace,
        "created_at_unix_ms": project.created_at_unix_ms.to_string(),
        "updated_at_unix_ms": project.updated_at_unix_ms.to_string(),
    })
}

pub(crate) fn run(run: &RunSummary) -> Value {
    json!({
        "project_id": run.project_id,
        "logical_trace_id": run.logical_trace_id,
        "external_trace_id": run.external_trace_id,
        "revision": run.revision.to_string(),
        "lifecycle": run.lifecycle,
        "title": bounded(&run.title, 512),
        "service_name": run.service_name,
        "environment": run.environment,
        "session_id": run.session_id,
        "build_id": run.build_id,
        "agent_id": run.agent_id,
        "identity_quality": run.identity_quality,
        "start_time_unix_nano": run.start_time_unix_nano.to_string(),
        "end_time_unix_nano": run.end_time_unix_nano.to_string(),
        "last_committed_unix_ms": run.last_committed_unix_ms.to_string(),
        "span_count": run.span_count.to_string(),
        "error_count": run.error_count.to_string(),
        "analysis_status": run.analysis_status,
        "finding_count": run.finding_count.to_string(),
    })
}

pub(crate) fn failure_group(group: &FailureGroupSummary) -> Value {
    let recurrence = group.recurrence.as_ref().map(|series| {
        json!({
            "started_at_unix_nano": series.started_at_unix_nano.to_string(),
            "ended_at_unix_nano": series.ended_at_unix_nano.to_string(),
            "bucket_width_nano": series.bucket_width_nano.to_string(),
            "buckets": series.buckets.iter().map(|bucket| json!({
                "started_at_unix_nano": bucket.started_at_unix_nano.to_string(),
                "ended_at_unix_nano": bucket.ended_at_unix_nano.to_string(),
                "eligible_run_count": bucket.eligible_run_count.to_string(),
                "affected_run_count": bucket.affected_run_count.to_string(),
                "finding_count": bucket.finding_count.to_string(),
                "recurrence_rate_basis_points": bucket.recurrence_rate_basis_points,
            })).collect::<Vec<_>>()
        })
    });
    json!({
        "scope_id": group.scope.scope_id,
        "project_id": group.project_id,
        "group_id": group.group_id,
        "failure_signature": group.failure_signature,
        "detector_ids": group.detector_ids,
        "subject": group.subject,
        "operation": group.operation,
        "presentation": group.presentation.as_ref().map(safe_presentation),
        "severity": group.severity,
        "occurrence_count": group.occurrence_count.to_string(),
        "recovered_count": group.recovered_count.to_string(),
        "unrecovered_count": group.unrecovered_count.to_string(),
        "unknown_recovery_count": group.unknown_recovery_count.to_string(),
        "affected_run_count": group.affected_run_count.to_string(),
        "affected_build_count": group.affected_build_count.to_string(),
        "affected_environment_count": group.affected_environment_count.to_string(),
        "confirmed_count": group.confirmed_count.to_string(),
        "dismissed_count": group.dismissed_count.to_string(),
        "needs_context_count": group.needs_context_count.to_string(),
        "unreviewed_count": group.unreviewed_count.to_string(),
        "first_seen_at": group.first_seen_at,
        "last_seen_at": group.last_seen_at,
        "occurrence_trend": group.occurrence_trend.iter().map(u64::to_string).collect::<Vec<_>>(),
        "recurrence": recurrence,
        "telemetry_gap_count": group.telemetry_gap_count.to_string(),
        "reanalyzing": group.reanalyzing,
        "feature_similarity_cohorts": group.feature_similarity_cohorts.iter().map(|cohort| json!({
            "model_id": cohort.model_id,
            "cluster_id": cohort.cluster_id,
            "member_count": cohort.member_count.to_string(),
            "mean_confidence": cohort.mean_confidence,
            "novelty_count": cohort.novelty_count.to_string(),
            "method": cohort.method,
            "embedding_provider": cohort.embedding_provider,
            "embedding_model": cohort.embedding_model,
        })).collect::<Vec<_>>(),
    })
}

pub(crate) fn failure_group_detail(detail: &FailureGroupDetail, occurrences: Vec<Value>) -> Value {
    json!({
        "group": failure_group(&detail.summary),
        "explanation": bounded(&detail.explanation, 4_096),
        "detector_versions": detail.detector_versions,
        "adapter_versions": detail.adapter_versions,
        "telemetry_gaps": detail.telemetry_gaps,
        "representative_occurrences": occurrences,
    })
}

pub(crate) fn failure_occurrence(occurrence: &FailureOccurrence) -> Value {
    let finding = &occurrence.finding;
    json!({
        "project_id": occurrence.project_id,
        "group_id": occurrence.group_id,
        "logical_trace_id": occurrence.logical_trace_id,
        "revision": occurrence.revision.to_string(),
        "run_title": bounded(&occurrence.run_title, 512),
        "service_name": occurrence.service_name,
        "analysis_status": occurrence.analysis_status,
        "finding_id": finding.finding_id,
        "detector_id": finding.detector_id,
        "detector_version": finding.detector_version,
        "severity": finding.severity,
        "recovery": finding.recovery,
        "telemetry_gaps": occurrence.telemetry_gaps,
    })
}

pub(crate) fn finding(evidence: &FindingEvidence) -> Value {
    let occurrence = &evidence.occurrence;
    let finding = &occurrence.finding;
    json!({
        "finding": {
            "finding_id": finding.finding_id,
            "group_id": occurrence.group_id,
            "failure_signature": finding.failure_signature,
            "detector_id": finding.detector_id,
            "detector_version": finding.detector_version,
            "kind": finding.kind,
            "severity": finding.severity,
            "recovery": finding.recovery,
            "certainty": {
                "rule_match": finding.certainty.rule_match,
                "semantic_coverage": finding.certainty.semantic_coverage,
                "missing_facts": finding.certainty.missing_facts,
                "calibrated_failure_risk": finding.certainty.calibrated_failure_risk,
            },
            "created_at": finding.created_at,
            "evidence_references": finding.evidence.iter().map(|reference| json!({
                "kind": reference.kind,
                "identity": reference.identity,
                "project_id": occurrence.project_id,
                "logical_trace_id": occurrence.logical_trace_id,
                "revision": occurrence.revision.to_string(),
                "span_id": reference.span_id,
            })).collect::<Vec<_>>(),
        },
        "occurrence": occurrence_projection(evidence),
        "presentation": evidence.presentation.as_ref().map(safe_presentation),
        "disposition": occurrence.disposition,
        "disposition_stale": occurrence.disposition_stale,
        "final_outcome": {"available": !evidence.final_outcome.is_null()},
        "telemetry_gaps": occurrence.telemetry_gaps,
        "version_identity": {
            "adapter": "persisted_analysis",
            "detector": finding.detector_version,
        },
    })
}

pub(crate) fn evidence_trace(evidence: &FindingEvidence, spans: &[SpanRow]) -> Value {
    let occurrence = &evidence.occurrence;
    let roles = evidence
        .presentation
        .as_ref()
        .map(|presentation| {
            presentation
                .evidence
                .iter()
                .filter_map(|item| {
                    item.evidence
                        .span_id
                        .as_ref()
                        .map(|span_id| (span_id.clone(), (&item.role, item.explanation.as_str())))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    json!({
        "project_id": occurrence.project_id,
        "logical_trace_id": occurrence.logical_trace_id,
        "revision": occurrence.revision.to_string(),
        "group_id": occurrence.group_id,
        "finding_id": occurrence.finding.finding_id,
        "evidence_span_ids": evidence.evidence_span_ids,
        "spans": spans.iter().map(|span| safe_span(span, roles.get(&span.span_id))).collect::<Vec<_>>(),
    })
}

pub(crate) fn eval_batch_job(job: &CandidateGenerationJobV1, poll_after_ms: u64) -> Value {
    json!({
        "job": {
            "schema_version": job.schema_version,
            "job_id": job.job_id,
            "kind": "eval_batch",
            "project_id": job.project_id,
            "preview_id": job.preview_id,
            "selection_hash": job.selection_hash,
            "idempotency_key_hash": format!("sha256:{}", hex::encode(Sha256::digest(job.idempotency_key.as_bytes()))),
            "status": job.status,
            "outcomes": job.outcomes,
            "created_at_unix_ms": job.created_at_unix_ms.to_string(),
            "updated_at_unix_ms": job.updated_at_unix_ms.to_string(),
        },
        "suggested_poll_after_ms": poll_after_ms.to_string(),
    })
}

fn occurrence_projection(evidence: &FindingEvidence) -> Value {
    let occurrence = &evidence.occurrence;
    json!({
        "project_id": occurrence.project_id,
        "logical_trace_id": occurrence.logical_trace_id,
        "revision": occurrence.revision.to_string(),
        "run_title": bounded(&occurrence.run_title, 512),
        "service_name": occurrence.service_name,
        "analysis_status": occurrence.analysis_status,
    })
}

fn safe_span(
    span: &SpanRow,
    role: Option<&(&perseval_service::analysis::FindingEvidenceRoleV1, &str)>,
) -> Value {
    json!({
        "span_id": span.span_id,
        "parent_span_id": span.parent_span_id,
        "name": bounded(&span.name, 512),
        "category": span.category,
        "start_time_unix_nano": span.start_time_unix_nano.to_string(),
        "duration_nano": span.duration_nano.to_string(),
        "status_code": span.status_code,
        "depth": span.depth,
        "has_children": span.has_children,
        "safe_attributes": {},
        "payload_references": span.payload_refs.iter().map(|(kind, blob)| json!({
            "kind": kind,
            "blob_id": blob.sha256,
            "original_bytes": blob.original_bytes.to_string(),
        })).collect::<Vec<_>>(),
        "events": span.events.iter().map(|event| json!({
            "name": bounded(&event.name, 256),
            "timestamp_unix_nano": event.timestamp_unix_nano.to_string(),
        })).collect::<Vec<_>>(),
        "links": span.links.iter().map(|link| json!({
            "trace_id": link.trace_id,
            "span_id": link.span_id,
        })).collect::<Vec<_>>(),
        "evidence_role": role.map(|(role, _)| role),
        "evidence_explanation": role.map(|(_, explanation)| bounded(explanation, 1_024)),
    })
}

fn safe_presentation(presentation: &perseval_service::analysis::FindingPresentationV1) -> Value {
    json!({
        "title": bounded(&presentation.title, 512),
        "diagnosis": bounded(&presentation.diagnosis, 2_048),
        "expected_behavior": bounded(&presentation.expected_behavior, 2_048),
        "observed_behavior": bounded(&presentation.observed_behavior, 2_048),
        "recovery_summary": bounded(&presentation.recovery_summary, 1_024),
        "caveat": presentation.caveat.as_ref().map(|value| bounded(value, 1_024)),
        "remediation_hint": bounded(&presentation.remediation_hint, 2_048),
    })
}

fn bounded(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use perseval_service::{AnalysisStatus, IdentityQualityV1, TraceLifecycle};

    #[test]
    fn safe_run_uses_decimal_strings_for_wide_integers() {
        let value = run(&RunSummary {
            project_id: "p".into(),
            logical_trace_id: "t".into(),
            external_trace_id: "e".into(),
            revision: u64::MAX,
            lifecycle: TraceLifecycle::Finalized,
            title: "run".into(),
            service_name: None,
            environment: None,
            session_id: None,
            build_id: None,
            agent_id: None,
            identity_quality: IdentityQualityV1::Unknown,
            start_time_unix_nano: u64::MAX,
            end_time_unix_nano: u64::MAX,
            last_committed_unix_ms: i64::MAX,
            span_count: u64::MAX,
            error_count: 0,
            analysis_status: AnalysisStatus::Ready,
            finding_count: 0,
        });
        assert_eq!(value["revision"], u64::MAX.to_string());
        assert_eq!(value["start_time_unix_nano"], u64::MAX.to_string());
        assert!(value["span_count"].is_string());
    }
}

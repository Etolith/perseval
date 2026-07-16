use super::*;
use perseval_service::RUN_COMPARISON_REQUEST_SCHEMA_VERSION;

impl FailureInbox {
    pub(super) fn can_compare_examples(&self) -> bool {
        let Some(current) = self.evidence.as_ref().map(|evidence| &evidence.occurrence) else {
            return false;
        };
        self.occurrences.iter().any(|occurrence| {
            occurrence.finding.finding_id != current.finding.finding_id
                && (occurrence.logical_trace_id != current.logical_trace_id
                    || occurrence.revision != current.revision)
        })
    }

    pub(super) fn begin_compare_examples(&mut self, cx: &mut Context<Self>) {
        if !self.can_compare_examples() {
            return;
        }
        self.compare_base_finding_id = self.selected_finding_id.clone();
        self.group_details_open = true;
        if self.inspector_open {
            self.inspector_open = false;
            self.emit_inspector_preference(cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_compare_examples(&mut self, cx: &mut Context<Self>) {
        self.compare_base_finding_id = None;
        cx.notify();
    }

    pub(super) fn compare_with_occurrence(
        &mut self,
        candidate_finding_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(base_id) = self.compare_base_finding_id.as_deref() else {
            return;
        };
        let Some(baseline) = self
            .occurrences
            .iter()
            .find(|occurrence| occurrence.finding.finding_id == base_id)
        else {
            return;
        };
        let Some(candidate) = self
            .occurrences
            .iter()
            .find(|occurrence| occurrence.finding.finding_id == candidate_finding_id)
        else {
            return;
        };
        let Some(request) = comparison_request(baseline, candidate) else {
            return;
        };
        self.compare_base_finding_id = None;
        self.group_details_open = false;
        cx.emit(FailureInboxEvent::OpenCompare(request));
        cx.notify();
    }

    pub(super) fn is_comparison_base(&self, finding_id: &str) -> bool {
        self.compare_base_finding_id.as_deref() == Some(finding_id)
    }

    pub(super) fn is_compatible_comparison_target(&self, candidate: &FailureOccurrence) -> bool {
        let Some(base_id) = self.compare_base_finding_id.as_deref() else {
            return false;
        };
        let Some(baseline) = self
            .occurrences
            .iter()
            .find(|occurrence| occurrence.finding.finding_id == base_id)
        else {
            return false;
        };
        comparison_request(baseline, candidate).is_some()
    }
}

fn comparison_request(
    baseline: &FailureOccurrence,
    candidate: &FailureOccurrence,
) -> Option<RunComparisonRequestV1> {
    if baseline.project_id != candidate.project_id
        || baseline.scope != candidate.scope
        || (baseline.logical_trace_id == candidate.logical_trace_id
            && baseline.revision == candidate.revision)
    {
        return None;
    }
    Some(RunComparisonRequestV1 {
        schema_version: RUN_COMPARISON_REQUEST_SCHEMA_VERSION.into(),
        scope: baseline.scope.clone(),
        baseline_trace_id: baseline.logical_trace_id.clone(),
        baseline_revision: baseline.revision,
        candidate_trace_id: candidate.logical_trace_id.clone(),
        candidate_revision: candidate.revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use perseval_service::{
        AnalysisStatus,
        analysis::{
            BEHAVIOR_FINDING_SCHEMA_VERSION, BehaviorFinding, EvidenceRef, FindingSeverity,
            RecoveryStatus,
        },
    };

    fn occurrence(project: &str, trace: &str, revision: u64) -> FailureOccurrence {
        FailureOccurrence {
            scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                project_id: Some(project.into()),
                ..QueryScopeCriteriaV1::default()
            }),
            project_id: project.into(),
            group_id: "group".into(),
            logical_trace_id: trace.into(),
            revision,
            run_title: trace.into(),
            service_name: None,
            analysis_status: AnalysisStatus::Ready,
            finding: BehaviorFinding {
                schema_version: BEHAVIOR_FINDING_SCHEMA_VERSION.into(),
                finding_id: format!("{trace}-{revision}"),
                detector_id: "test".into(),
                detector_version: "1".into(),
                trace_id: trace.into(),
                kind: "test".into(),
                severity: FindingSeverity::High,
                recovery: RecoveryStatus::Unrecovered,
                confidence: Some(1.0),
                certainty: Default::default(),
                failure_signature: "failure".into(),
                evidence: vec![EvidenceRef::span("root")],
                created_at: "2026-07-13T00:00:00Z".into(),
                metadata: BTreeMap::new(),
            },
            disposition: None,
            disposition_stale: false,
            telemetry_gaps: Vec::new(),
        }
    }

    #[test]
    fn comparison_requires_distinct_revisions_in_one_project() {
        let baseline = occurrence("project-a", "trace-a", 1);
        let candidate = occurrence("project-a", "trace-b", 2);
        let request = comparison_request(&baseline, &candidate).expect("compatible examples");
        assert_eq!(request.baseline_trace_id, "trace-a");
        assert_eq!(request.candidate_trace_id, "trace-b");
        assert!(comparison_request(&baseline, &baseline).is_none());
        assert!(comparison_request(&baseline, &occurrence("project-b", "trace-b", 2)).is_none());
    }
}

use std::collections::BTreeMap;
use std::time::Instant;

use perseval_store::{
    ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION, ANALYSIS_DEFINITION_SCHEMA_VERSION,
    ANALYSIS_IDENTITY_SCHEMA_VERSION, ANALYSIS_RESULT_SCHEMA_VERSION, AnalysisDefinitionV1,
    AnalysisIdentityV1, AnalysisResultV1, AnalysisStatus, CandidateGenerationJobStatusV1,
    CandidateGenerationOutcomeKindV1, CreateProjectV1, DEFAULT_ANALYSIS_GROUPING_VERSION,
    DEFAULT_ANALYSIS_RISK_MODEL_VERSION, EvalBatchSelectionSpecV1, EvalReviewDecisionV1,
    EvalReviewQueueStateV1, FailureFiltersV1, FindingDispositionStateV1, QueryScopeCriteriaV1,
    QueryScopeV1, RUN_COMPARISON_REQUEST_SCHEMA_VERSION, ReviewEvalCandidateV1,
    RunComparisonRequestV1, SPAN_UPSERT_SCHEMA_VERSION, SpanUpsertBatchV1, SpanUpsertV1,
    WorkspaceStore, WorkspaceStoreLayout,
};
use serde_json::Value;
use tempfile::tempdir;
use traces_to_evals::{
    AGENT_BEHAVIOR_TRACE_SCHEMA_VERSION, AgentBehaviorTrace, BEHAVIOR_FINDING_SCHEMA_VERSION,
    BEHAVIOR_INPUT_SCHEMA_VERSION, BehaviorFinding, BehaviorInputCoverageV1, DetectionReportV1,
    DetectorProfileIdentityV1, EvalCandidateStatus, EvidenceRef, FindingCertaintyV1,
    FindingSeverity, RecoveryStatus, TraceAlignmentOptions,
};

fn span(trace: &str, span_id: &str, parent: Option<&str>) -> SpanUpsertV1 {
    SpanUpsertV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        external_trace_id: trace.into(),
        external_span_id: span_id.into(),
        external_parent_span_id: parent.map(str::to_owned),
        logical_trace_id: trace.into(),
        content_hash: String::new(),
        observed_at_unix_nano: 1,
        name: span_id.into(),
        category: "agent".into(),
        span_kind: 0,
        start_time_unix_nano: 1,
        end_time_unix_nano: 2,
        status_code: 0,
        status_message: String::new(),
        trace_state: String::new(),
        flags: 0,
        dropped_attributes_count: 0,
        dropped_events_count: 0,
        dropped_links_count: 0,
        resource: BTreeMap::from([
            ("service.name".into(), Value::String("checkout".into())),
            (
                "perseval.project.id".into(),
                Value::String("checkout".into()),
            ),
        ]),
        scope: BTreeMap::new(),
        attributes: BTreeMap::from([(
            "input.value".into(),
            Value::String("bounded private payload".into()),
        )]),
        payload_refs: BTreeMap::new(),
        payload_identities: BTreeMap::new(),
        events: Vec::new(),
        links: Vec::new(),
        decoder_version: "test".into(),
        semantic_mapping_version: "test".into(),
    }
}

fn scope(project_id: Option<&str>) -> QueryScopeV1 {
    QueryScopeV1::new(QueryScopeCriteriaV1 {
        project_id: project_id.map(str::to_string),
        ..QueryScopeCriteriaV1::default()
    })
}

fn ingest(store: &WorkspaceStore, trace: &str, span_id: &str, parent: Option<&str>) {
    ingest_project(store, "checkout", trace, span_id, parent);
}

fn ingest_project(
    store: &WorkspaceStore,
    project_id: &str,
    trace: &str,
    span_id: &str,
    parent: Option<&str>,
) {
    let mut projected_span = span(trace, span_id, parent);
    projected_span.resource.insert(
        "perseval.project.id".into(),
        Value::String(project_id.into()),
    );
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        received_at_unix_ms: 1,
        spans: vec![projected_span],
        rejected_spans: 0,
        rejection_message: None,
    };
    let raw = format!("{trace}:{span_id}");
    let receipt = store
        .journal_batch(&mut batch, raw.as_bytes(), "test", 4096)
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();
}

fn ingest_scoped(
    store: &WorkspaceStore,
    trace: &str,
    environment: &str,
    build_id: &str,
    session_id: &str,
) {
    let mut projected_span = span(trace, "root", None);
    projected_span.resource.insert(
        "deployment.environment.name".into(),
        Value::String(environment.into()),
    );
    projected_span
        .resource
        .insert("service.version".into(), Value::String(build_id.into()));
    projected_span.resource.insert(
        "gen_ai.conversation.id".into(),
        Value::String(session_id.into()),
    );
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        received_at_unix_ms: 1,
        spans: vec![projected_span],
        rejected_spans: 0,
        rejection_message: None,
    };
    let receipt = store
        .journal_batch(&mut batch, trace.as_bytes(), "test", 4096)
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();
}

fn finalize(store: &WorkspaceStore) {
    store.advance_lifecycle(i64::MAX / 4, 1, 1).unwrap();
    store.advance_lifecycle(i64::MAX / 4, 1, 1).unwrap();
}

fn project_pending_topologies(store: &WorkspaceStore, chunk_rows: usize) {
    while let Some(job) = store.claim_pending_topology().unwrap() {
        let rows = store.build_topology_projection(&job).unwrap();
        if rows.is_empty() {
            let _ = store.commit_topology_chunk(&job, &[], true, true).unwrap();
            continue;
        }
        let chunk_count = rows.len().div_ceil(chunk_rows);
        for (index, chunk) in rows.chunks(chunk_rows).enumerate() {
            let _ = store
                .commit_topology_chunk(&job, chunk, index == 0, index + 1 == chunk_count)
                .unwrap();
        }
    }
}

fn finding(trace: &str, finding_id: &str, signature: &str) -> BehaviorFinding {
    BehaviorFinding {
        schema_version: BEHAVIOR_FINDING_SCHEMA_VERSION.into(),
        finding_id: finding_id.into(),
        detector_id: "false_success_claim".into(),
        detector_version: "2".into(),
        trace_id: trace.into(),
        kind: "false_success_claim".into(),
        severity: FindingSeverity::High,
        recovery: RecoveryStatus::Unrecovered,
        confidence: Some(1.0),
        certainty: FindingCertaintyV1::default(),
        failure_signature: signature.into(),
        evidence: vec![EvidenceRef::span("root")],
        created_at: "2026-07-11T12:00:00Z".into(),
        metadata: BTreeMap::from([
            ("subject".into(), Value::String("cancel_card".into())),
            ("operation".into(), Value::String("cancel_card".into())),
        ]),
    }
}

fn result(trace: &str, revision: u64, findings: Vec<BehaviorFinding>) -> AnalysisResultV1 {
    let mut behavior = AgentBehaviorTrace::new(trace);
    behavior.schema_version = AGENT_BEHAVIOR_TRACE_SCHEMA_VERSION.into();
    behavior.evidence = vec![EvidenceRef::span("root")];
    behavior.metadata.insert(
        "traceeval.behavior_adapter.id".into(),
        Value::String("openinference".into()),
    );
    behavior.metadata.insert(
        "traceeval.behavior_adapter.version".into(),
        Value::String("1".into()),
    );
    let identity = AnalysisIdentityV1 {
        schema_version: ANALYSIS_IDENTITY_SCHEMA_VERSION.into(),
        logical_trace_id: trace.into(),
        revision,
        input_schema_version: BEHAVIOR_INPUT_SCHEMA_VERSION.into(),
        projection_version: "test.safe_projection.v1".into(),
        adapter_id: "openinference".into(),
        adapter_version: "1".into(),
        detector_profile_id: "test".into(),
        detector_profile_version: "1".into(),
        detector_versions: BTreeMap::from([("false_success_claim".into(), "2".into())]),
        grouping_version: DEFAULT_ANALYSIS_GROUPING_VERSION.into(),
        risk_model_version: DEFAULT_ANALYSIS_RISK_MODEL_VERSION.into(),
    };
    let detection_report = DetectionReportV1 {
        schema_version: traces_to_evals::DETECTION_REPORT_SCHEMA_VERSION.into(),
        trace_id: trace.into(),
        input_schema_version: BEHAVIOR_INPUT_SCHEMA_VERSION.into(),
        profile: DetectorProfileIdentityV1 {
            profile_id: "test".into(),
            profile_version: "1".into(),
        },
        detector_versions: identity.detector_versions.clone(),
        input_coverage: BehaviorInputCoverageV1::default(),
        detector_coverage: BTreeMap::new(),
        findings: findings.clone(),
        telemetry_diagnostics: Vec::new(),
    };
    AnalysisResultV1 {
        schema_version: ANALYSIS_RESULT_SCHEMA_VERSION.into(),
        analysis_id: identity.analysis_id(),
        identity,
        logical_trace_id: trace.into(),
        revision,
        adapter_id: "openinference".into(),
        adapter_version: "1".into(),
        behavior,
        detection_report,
        findings,
    }
}

fn definition(result: &AnalysisResultV1) -> AnalysisDefinitionV1 {
    AnalysisDefinitionV1 {
        schema_version: ANALYSIS_DEFINITION_SCHEMA_VERSION.into(),
        input_schema_version: result.identity.input_schema_version.clone(),
        projection_version: result.identity.projection_version.clone(),
        adapter_id: result.identity.adapter_id.clone(),
        adapter_version: result.identity.adapter_version.clone(),
        detector_profile_id: result.identity.detector_profile_id.clone(),
        detector_profile_version: result.identity.detector_profile_version.clone(),
        detector_versions: result.identity.detector_versions.clone(),
        grouping_version: result.identity.grouping_version.clone(),
        risk_model_version: result.identity.risk_model_version.clone(),
    }
}

include!("failure_inbox/analysis_projection.rs");
include!("failure_inbox/product_workflows.rs");

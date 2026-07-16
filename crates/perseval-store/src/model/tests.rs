use super::*;

#[test]
fn query_scope_identity_is_deterministic_and_tamper_evident() {
    let criteria = QueryScopeCriteriaV1 {
        project_id: Some("project".into()),
        build_id: Some("build-7".into()),
        started_after_unix_nano: Some(10),
        started_before_unix_nano: Some(20),
        ..QueryScopeCriteriaV1::default()
    };
    let scope = QueryScopeV1::new(criteria.clone());
    assert_eq!(scope, QueryScopeV1::new(criteria));
    assert!(scope.validate().is_ok());

    let mut tampered = scope;
    tampered.criteria.build_id = Some("build-8".into());
    assert!(tampered.validate().is_err());
}

#[test]
fn query_scope_rejects_reversed_time_bounds() {
    let scope = QueryScopeV1::new(QueryScopeCriteriaV1 {
        started_after_unix_nano: Some(20),
        started_before_unix_nano: Some(10),
        ..QueryScopeCriteriaV1::default()
    });

    assert!(scope.validate().is_err());
}

#[test]
fn failure_groups_serialize_feature_similarity_and_read_legacy_field() {
    let summary = FailureGroupSummary {
        scope: QueryScopeV1::default(),
        project_id: "project".into(),
        group_id: "group".into(),
        failure_signature: "signature".into(),
        detector_ids: vec!["detector".into()],
        subject: None,
        operation: None,
        presentation: None,
        severity: FindingSeverity::High,
        occurrence_count: 1,
        recovered_count: 0,
        unrecovered_count: 1,
        unknown_recovery_count: 0,
        affected_run_count: 1,
        affected_build_count: 1,
        affected_environment_count: 1,
        confirmed_count: 0,
        dismissed_count: 0,
        needs_context_count: 0,
        unreviewed_count: 1,
        stale_disposition_count: 0,
        first_seen_at: "unknown".into(),
        last_seen_at: "unknown".into(),
        occurrence_trend: vec![1],
        recurrence: None,
        telemetry_gap_count: 0,
        reanalyzing: false,
        feature_similarity_cohorts: vec![FeatureSimilarityCohortSummary {
            model_id: "feature-similarity-model".into(),
            cluster_id: "cluster".into(),
            member_count: 1,
            mean_confidence: 0.9,
            novelty_count: 0,
            method: "kmeans".into(),
            embedding_provider: Some("perseval-local".into()),
            embedding_model: Some("signed-feature-hash-v1".into()),
        }],
    };

    let mut encoded = serde_json::to_value(&summary).unwrap();
    assert!(encoded.get("feature_similarity_cohorts").is_some());
    assert!(encoded.get("semantic_cohorts").is_none());

    let legacy = encoded
        .as_object_mut()
        .unwrap()
        .remove("feature_similarity_cohorts")
        .unwrap();
    encoded
        .as_object_mut()
        .unwrap()
        .insert("semantic_cohorts".into(), legacy);
    let decoded: FailureGroupSummary = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.feature_similarity_cohorts.len(), 1);
}

#[test]
fn pipeline_diagnostics_use_feature_similarity_field_names() {
    let diagnostics = PipelineDiagnosticsV1 {
        schema_version: PIPELINE_DIAGNOSTICS_SCHEMA_VERSION.into(),
        stages: Vec::new(),
        journal_backlog_rows: 0,
        journal_backlog_oldest_age_ms: 0,
        analysis_backlog_rows: 0,
        analysis_backlog_oldest_age_ms: 0,
        feature_similarity_models_built: 2,
        feature_similarity_assignments_written: 100,
    };
    let encoded = serde_json::to_value(diagnostics).unwrap();
    assert_eq!(encoded["feature_similarity_models_built"], 2);
    assert_eq!(encoded["feature_similarity_assignments_written"], 100);
    assert!(encoded.get("semantic_models_built").is_none());
    assert!(encoded.get("semantic_assignments_written").is_none());
}

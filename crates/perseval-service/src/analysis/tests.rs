use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use traces_to_evals::{
    BEHAVIOR_FINDING_SCHEMA_VERSION, BehaviorFinding, ClusterModel, ClusterModelSource,
    ClusterQualityReport, EvalCase, FindingCertaintyV1, FindingSeverity, ProjectName,
    RecoveryStatus,
};

use crate::config::AnalysisConfig;

use super::{
    CohortControlHandle, CohortFeatureCache, CohortHealthState, CohortJob, CohortRebuildJob,
    OpenAiHealthHandle, cohort_growth_requires_rebuild, feature_hash_embedding,
    feature_similarity_model_id, model_matches_scope, project_and_embed_findings,
    run_debounced_jobs,
};

#[test]
fn local_embeddings_are_deterministic_normalized_and_content_sensitive() {
    let first = feature_hash_embedding("tool: cancel_card error: timeout", 256);
    let repeated = feature_hash_embedding("tool: cancel_card error: timeout", 256);
    let different = feature_hash_embedding("policy: approval bypass", 256);

    assert_eq!(first, repeated);
    assert_ne!(first, different);
    let norm = first.iter().map(|value| value * value).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 0.000_1);
}

#[test]
fn provider_health_never_retains_raw_provider_errors() {
    let health = OpenAiHealthHandle::new(true, true);
    health.begin_job();
    health.finish_failure("401 invalid api key provider-secret-material");

    let snapshot = health.snapshot();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.failed_jobs, 1);
    let error = snapshot.last_error.unwrap();
    assert_eq!(error, "OpenAI authentication failed; check OPENAI_API_KEY");
    assert!(!error.contains("provider-secret-material"));
}

#[test]
fn missing_key_is_an_explicit_offline_health_state() {
    let snapshot = OpenAiHealthHandle::new(true, false).snapshot();
    assert!(snapshot.enabled);
    assert!(!snapshot.configured);
    assert!(snapshot.degraded);
    assert_eq!(snapshot.successful_jobs, 0);
    assert_eq!(snapshot.failed_jobs, 0);
}

#[test]
fn cohort_scheduler_coalesces_each_burst_into_one_rebuild() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let rebuilds = Arc::new(AtomicUsize::new(0));
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_rebuilds = Arc::clone(&rebuilds);
    let worker = thread::spawn(move || {
        run_debounced_jobs(
            &worker_shutdown,
            receiver,
            Duration::from_millis(20),
            || true,
            || {
                worker_rebuilds.fetch_add(1, Ordering::AcqRel);
                true
            },
        );
    });

    for _ in 0..100 {
        sender.send(()).unwrap();
    }
    wait_for_rebuilds(&rebuilds, 1);
    assert_eq!(rebuilds.load(Ordering::Acquire), 1);

    for _ in 0..100 {
        sender.send(()).unwrap();
    }
    wait_for_rebuilds(&rebuilds, 2);
    assert_eq!(rebuilds.load(Ordering::Acquire), 2);

    shutdown.store(true, Ordering::Release);
    drop(sender);
    worker.join().unwrap();
}

#[test]
fn bounded_cohort_mailbox_falls_back_to_a_full_rebuild_on_overflow() {
    let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
    let rebuild_all = Arc::new(AtomicBool::new(false));
    let control = CohortControlHandle {
        sender,
        rebuild_all: Arc::clone(&rebuild_all),
        health: Arc::new(CohortHealthState::default()),
    };

    control
        .submit(CohortJob::Rebuild(CohortRebuildJob {
            project_id: Some("project-a".into()),
        }))
        .unwrap();
    control
        .submit(CohortJob::Rebuild(CohortRebuildJob {
            project_id: Some("project-b".into()),
        }))
        .unwrap();

    assert!(rebuild_all.load(Ordering::Acquire));
    assert_eq!(control.health().rebuild_pending, 2);
}

#[test]
fn feature_similarity_model_identity_includes_project_definition_and_scope() {
    let hashes = ["projection-a", "projection-b"];
    let baseline = feature_similarity_model_id(
        "project-a",
        "definition-a",
        "scope-a",
        "local",
        "model-a",
        32,
        &hashes,
    );

    assert_ne!(
        baseline,
        feature_similarity_model_id(
            "project-b",
            "definition-a",
            "scope-a",
            "local",
            "model-a",
            32,
            &hashes,
        )
    );
    assert_ne!(
        baseline,
        feature_similarity_model_id(
            "project-a",
            "definition-b",
            "scope-a",
            "local",
            "model-a",
            32,
            &hashes,
        )
    );
    assert_ne!(
        baseline,
        feature_similarity_model_id(
            "project-a",
            "definition-a",
            "scope-b",
            "local",
            "model-a",
            32,
            &hashes,
        )
    );
    assert_ne!(
        baseline,
        feature_similarity_model_id(
            "project-a",
            "definition-a",
            "scope-a",
            "openai",
            "model-a",
            32,
            &hashes,
        )
    );
}

#[test]
fn cohort_growth_threshold_is_bounded_by_percentage_and_case_limit() {
    let mut config = AnalysisConfig {
        cohort_rebuild_new_percent: 10,
        cohort_rebuild_new_cases: 250,
        ..AnalysisConfig::default()
    };
    let mut model = cohort_model(1_000, 1_099);
    assert!(!cohort_growth_requires_rebuild(&model, &config));
    model.assignments.push(cluster_assignment("case-1099"));
    assert!(cohort_growth_requires_rebuild(&model, &config));

    config.cohort_rebuild_new_percent = 100;
    let mut model = cohort_model(10_000, 10_249);
    assert!(!cohort_growth_requires_rebuild(&model, &config));
    model.assignments.push(cluster_assignment("case-10249"));
    assert!(cohort_growth_requires_rebuild(&model, &config));
}

#[test]
fn measured_novelty_drift_can_trigger_a_rebuild_before_volume_threshold() {
    let config = AnalysisConfig {
        cohort_rebuild_new_percent: 10,
        cohort_rebuild_new_cases: 250,
        cohort_rebuild_novelty_percent: 25,
        minimum_findings: 3,
        ..AnalysisConfig::default()
    };
    let mut model = cohort_model(100, 103);
    model
        .metadata
        .insert("perseval_incremental_assignment_count".into(), 3.into());
    model
        .metadata
        .insert("perseval_incremental_novelty_count".into(), 1.into());

    assert!(cohort_growth_requires_rebuild(&model, &config));
}

#[test]
fn model_compatibility_requires_exact_project_definition_and_scope() {
    let mut model = cohort_model(1, 1);
    model
        .metadata
        .insert("perseval_project_id".into(), "project-a".into());
    model.metadata.insert(
        "perseval_analysis_definition_id".into(),
        "definition-a".into(),
    );
    model
        .metadata
        .insert("perseval_scope_id".into(), "scope-a".into());
    model.metadata.insert(
        "perseval_embedding_provider".into(),
        "perseval-local".into(),
    );
    model.metadata.insert(
        "perseval_embedding_model".into(),
        "signed-feature-hash-v1".into(),
    );
    model
        .metadata
        .insert("perseval_embedding_dimensions".into(), 256.into());
    let config = AnalysisConfig::default();

    assert!(model_matches_scope(
        &model,
        "project-a",
        "definition-a",
        "scope-a",
        &config,
    ));
    assert!(!model_matches_scope(
        &model,
        "project-a",
        "definition-b",
        "scope-a",
        &config,
    ));
}

#[test]
fn feature_cache_reuses_only_exact_compatible_projection_and_embedding() {
    let config = AnalysisConfig {
        embedding_dimensions: 32,
        ..AnalysisConfig::default()
    };
    let mut cache = CohortFeatureCache::new(2);
    let health = OpenAiHealthHandle::new(false, false);
    let finding = behavior_finding("finding-a", "operation-a");

    let first = project_and_embed_findings(
        std::slice::from_ref(&finding),
        &config,
        &mut cache,
        None,
        None,
        &health,
    )
    .unwrap();
    let repeated = project_and_embed_findings(
        std::slice::from_ref(&finding),
        &config,
        &mut cache,
        None,
        None,
        &health,
    )
    .unwrap();
    assert_eq!(first.cache_misses, 1);
    assert_eq!(repeated.cache_misses, 0);
    assert_eq!(first.projections, repeated.projections);
    assert_eq!(first.embeddings, repeated.embeddings);

    let changed = behavior_finding("finding-a", "operation-b");
    assert_eq!(
        project_and_embed_findings(&[changed], &config, &mut cache, None, None, &health)
            .unwrap()
            .cache_misses,
        1
    );
    let changed_dimensions = AnalysisConfig {
        embedding_dimensions: 64,
        ..config
    };
    assert_eq!(
        project_and_embed_findings(
            &[finding],
            &changed_dimensions,
            &mut cache,
            None,
            None,
            &health,
        )
        .unwrap()
        .cache_misses,
        1
    );
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn feature_projection_stops_before_work_when_rebuild_is_cancelled() {
    let config = AnalysisConfig::default();
    let mut cache = CohortFeatureCache::new(2);
    let shutdown = AtomicBool::new(true);
    let health = OpenAiHealthHandle::new(false, false);
    let result = project_and_embed_findings(
        &[behavior_finding("finding-a", "operation-a")],
        &config,
        &mut cache,
        Some(&shutdown),
        None,
        &health,
    );

    assert!(matches!(result, Err(perseval_store::StoreError::Cancelled)));
    assert!(cache.entries.is_empty());
}

fn cohort_model(case_count: usize, assignment_count: usize) -> ClusterModel {
    ClusterModel::new_with_project(
        &ProjectName::new("perseval-test").unwrap(),
        "model",
        "now",
        ClusterModelSource {
            case_count,
            embedding_provider: None,
            embedding_model: None,
            embedding_dimensions: Some(2),
            projection_version: Some("safe.v1".into()),
            algorithm: "kmeans".into(),
            distance_metric: "cosine".into(),
            random_seed: 42,
        },
        Vec::new(),
        (0..assignment_count)
            .map(|index| cluster_assignment(&format!("case-{index}")))
            .collect(),
        ClusterQualityReport::empty(),
    )
}

fn cluster_assignment(case_id: &str) -> traces_to_evals::ClusterAssignment {
    traces_to_evals::ClusterAssignment::new(
        &EvalCase::new(case_id, format!("trace-{case_id}"), "evidence"),
        "cluster-a",
        1.0,
        "test",
    )
}

fn behavior_finding(finding_id: &str, operation: &str) -> BehaviorFinding {
    BehaviorFinding {
        schema_version: BEHAVIOR_FINDING_SCHEMA_VERSION.into(),
        finding_id: finding_id.into(),
        detector_id: "detector".into(),
        detector_version: "1".into(),
        trace_id: "trace-a".into(),
        kind: "tool_failure".into(),
        severity: FindingSeverity::High,
        recovery: RecoveryStatus::Unrecovered,
        confidence: None,
        certainty: FindingCertaintyV1::default(),
        failure_signature: "signature-a".into(),
        evidence: Vec::new(),
        created_at: "2026-07-15T00:00:00Z".into(),
        metadata: std::collections::BTreeMap::from([("operation".into(), operation.into())]),
    }
}

fn wait_for_rebuilds(rebuilds: &AtomicUsize, expected: usize) {
    let started = Instant::now();
    while rebuilds.load(Ordering::Acquire) < expected {
        assert!(started.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(2));
    }
}

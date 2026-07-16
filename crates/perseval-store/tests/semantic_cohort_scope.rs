use perseval_store::{WorkspaceStore, WorkspaceStoreLayout};
use tempfile::tempdir;
use traces_to_evals::{
    ClusterAssignment, ClusterModel, ClusterModelSource, ClusterQualityReport, DiscoveredCluster,
    EvalCase, ProjectName,
};

fn model(model_id: &str, project_id: &str, definition_id: &str, case_id: &str) -> ClusterModel {
    let project_name = ProjectName::new("perseval-test").unwrap();
    let case = EvalCase::new(case_id, format!("trace-{case_id}"), "failure evidence");
    let assignment = ClusterAssignment::new(&case, "cluster-a", 1.0, "fixture");
    let mut model = ClusterModel::new_with_project(
        &project_name,
        model_id,
        "2026-07-15T00:00:00Z",
        ClusterModelSource {
            case_count: 1,
            embedding_provider: Some("test".into()),
            embedding_model: Some("test".into()),
            embedding_dimensions: Some(2),
            projection_version: Some("safe.v1".into()),
            algorithm: "kmeans".into(),
            distance_metric: "cosine".into(),
            random_seed: 42,
        },
        vec![
            DiscoveredCluster::new("cluster-a", 1, vec![case_id.into()])
                .with_centroid(vec![1.0, 0.0]),
        ],
        vec![assignment],
        ClusterQualityReport {
            cluster_count: 1,
            assigned_case_count: 1,
            mean_distance: Some(0.0),
            silhouette_score: None,
            clusters: Vec::new(),
        },
    );
    model
        .metadata
        .insert("perseval_project_id".into(), project_id.to_string().into());
    model.metadata.insert(
        "perseval_analysis_definition_id".into(),
        definition_id.to_string().into(),
    );
    model
        .metadata
        .insert("perseval_scope_id".into(), "all-time-all-builds".into());
    model
}

#[test]
fn models_and_incremental_assignments_are_isolated_by_project() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    let first = model("model-a", "project-a", "definition-a", "finding-a");
    let second = model("model-b", "project-b", "definition-a", "finding-b");

    assert!(
        store
            .commit_feature_similarity_model_scoped(
                &first,
                "project-a",
                "definition-a",
                "all-time-all-builds",
                2,
            )
            .unwrap()
    );
    assert!(
        store
            .commit_feature_similarity_model_scoped(
                &second,
                "project-b",
                "definition-a",
                "all-time-all-builds",
                2,
            )
            .unwrap()
    );

    let new_case = EvalCase::new("finding-c", "trace-c", "new evidence");
    let assignment = ClusterAssignment::new(&new_case, "cluster-a", 0.9, "incremental");
    assert_eq!(
        store
            .append_active_feature_similarity_assignments(
                "project-a",
                "definition-a",
                "all-time-all-builds",
                std::slice::from_ref(&assignment),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .append_active_feature_similarity_assignments(
                "project-a",
                "definition-a",
                "all-time-all-builds",
                &[assignment],
            )
            .unwrap(),
        0
    );

    let project_a = store
        .active_feature_similarity_model_for_project("project-a")
        .unwrap()
        .unwrap();
    let project_b = store
        .active_feature_similarity_model_for_project("project-b")
        .unwrap()
        .unwrap();
    assert_eq!(project_a.model_id, "model-a");
    assert_eq!(project_a.assignments.len(), 2);
    assert_eq!(project_a.clusters[0].size, 2);
    assert_eq!(project_a.quality.assigned_case_count, 2);
    assert_eq!(
        project_a.metadata["perseval_incremental_assignment_count"],
        1
    );
    assert_eq!(project_b.model_id, "model-b");
    assert_eq!(project_b.assignments.len(), 1);
}

#[test]
fn incremental_assignment_targets_the_exact_immutable_scope() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    let all_time = model("all-time", "project-a", "definition-a", "finding-a");
    let mut release = model("release", "project-a", "definition-a", "finding-b");
    release
        .metadata
        .insert("perseval_scope_id".into(), "release-42".to_string().into());
    store
        .commit_feature_similarity_model_scoped(
            &all_time,
            "project-a",
            "definition-a",
            "all-time-all-builds",
            2,
        )
        .unwrap();
    store
        .commit_feature_similarity_model_scoped(
            &release,
            "project-a",
            "definition-a",
            "release-42",
            2,
        )
        .unwrap();

    let case = EvalCase::new("finding-c", "trace-c", "new evidence");
    let assignment = ClusterAssignment::new(&case, "cluster-a", 0.9, "incremental");
    assert_eq!(
        store
            .append_active_feature_similarity_assignments(
                "project-a",
                "definition-a",
                "all-time-all-builds",
                &[assignment],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .active_feature_similarity_model_for_scope(
                "project-a",
                "definition-a",
                "all-time-all-builds",
            )
            .unwrap()
            .unwrap()
            .assignments
            .len(),
        2
    );
    assert_eq!(
        store
            .active_feature_similarity_model_for_scope("project-a", "definition-a", "release-42",)
            .unwrap()
            .unwrap()
            .assignments
            .len(),
        1
    );
}

#[test]
fn scoped_model_history_is_bounded_without_deleting_other_projects() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    let other = model("other-model", "project-b", "definition-a", "other-finding");
    store
        .commit_feature_similarity_model_scoped(
            &other,
            "project-b",
            "definition-a",
            "all-time-all-builds",
            2,
        )
        .unwrap();

    for index in 0..4 {
        let next = model(
            &format!("project-a-model-{index}"),
            "project-a",
            "definition-a",
            &format!("finding-{index}"),
        );
        store
            .commit_feature_similarity_model_scoped(
                &next,
                "project-a",
                "definition-a",
                "all-time-all-builds",
                2,
            )
            .unwrap();
    }

    let diagnostics = store.pipeline_diagnostics().unwrap();
    assert_eq!(diagnostics.feature_similarity_models_built, 3);
    assert_eq!(diagnostics.feature_similarity_assignments_written, 3);
    assert_eq!(
        store
            .active_feature_similarity_model_for_project("project-a")
            .unwrap()
            .unwrap()
            .model_id,
        "project-a-model-3"
    );
    assert_eq!(
        store
            .active_feature_similarity_model_for_project("project-b")
            .unwrap()
            .unwrap()
            .model_id,
        "other-model"
    );
}

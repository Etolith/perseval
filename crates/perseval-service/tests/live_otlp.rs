use std::io::Write;
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::GzEncoder;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use perseval_service::ingest::otlp::{OtlpReceiverConfig, prepare_otlp_submission};
use perseval_service::store::{WorkspaceStore, WorkspaceStoreLayout};
use perseval_service::{
    AnalysisStatus, CandidateGenerationJobStatusV1, CreateProjectV1, EvalBatchSelectionSpecV1,
    EvalReviewDecisionV1, EvalReviewQueueStateV1, FailureFiltersV1, PersevalConfigV1,
    QueryScopeCriteriaV1, QueryScopeV1, ServiceRuntime, TraceChangeKind, TraceLifecycle,
};
use prost::Message;

fn request() -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![
                    KeyValue {
                        key: "service.name".into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("test-agent".into())),
                        }),
                        ..Default::default()
                    },
                    KeyValue {
                        key: "perseval.project.id".into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("test-agent".into())),
                        }),
                        ..Default::default()
                    },
                    KeyValue {
                        key: "gen_ai.conversation.id".into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("session-42".into())),
                        }),
                        ..Default::default()
                    },
                    KeyValue {
                        key: "service.version".into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("build-7".into())),
                        }),
                        ..Default::default()
                    },
                    KeyValue {
                        key: "deployment.environment.name".into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("staging".into())),
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: vec![1; 16],
                    span_id: vec![2; 8],
                    name: "agent.chat".into(),
                    start_time_unix_nano: 1_000_000,
                    end_time_unix_nano: 2_000_000,
                    attributes: vec![
                        KeyValue {
                            key: "gen_ai.operation.name".into(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue("chat".into())),
                            }),
                            ..Default::default()
                        },
                        KeyValue {
                            key: "gen_ai.input.messages".into(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue("secret input".into())),
                            }),
                            ..Default::default()
                        },
                        KeyValue {
                            key: "openinference.span.kind".into(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue("AGENT".into())),
                            }),
                            ..Default::default()
                        },
                        KeyValue {
                            key: "agent.final.status".into(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue("completed".into())),
                            }),
                            ..Default::default()
                        },
                        KeyValue {
                            key: "agent.outcome.claim.status".into(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue("succeeded".into())),
                            }),
                            ..Default::default()
                        },
                        KeyValue {
                            key: "agent.outcome.claim.operation".into(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue("checkout".into())),
                            }),
                            ..Default::default()
                        },
                    ],
                    status: Some(Status {
                        code: 1,
                        message: String::new(),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn request_for_project(project_id: &str, index: u64) -> ExportTraceServiceRequest {
    let mut request = request();
    let resource = request.resource_spans[0].resource.as_mut().unwrap();
    for attribute in &mut resource.attributes {
        if matches!(
            attribute.key.as_str(),
            "service.name" | "perseval.project.id"
        ) {
            attribute.value = Some(AnyValue {
                value: Some(any_value::Value::StringValue(project_id.into())),
            });
        }
    }
    let span = &mut request.resource_spans[0].scope_spans[0].spans[0];
    let mut trace_id = vec![0_u8; 16];
    trace_id[8..].copy_from_slice(&(index + 1).to_be_bytes());
    span.trace_id = trace_id;
    span.span_id = (index + 1).to_be_bytes().to_vec();
    request
}

#[allow(clippy::field_reassign_with_default)]
fn live_config(temp: &tempfile::TempDir) -> PersevalConfigV1 {
    let mut config = PersevalConfigV1::default();
    config.workspace_dir = temp.path().join("workspace");
    config.otlp.enabled = true;
    config.otlp.bind_addr = "127.0.0.1:0".parse().unwrap();
    config.lifecycle.idle_ms = 20;
    config.lifecycle.finalization_grace_ms = 20;
    config.lifecycle.sweep_ms = 5;
    config
}

#[tokio::test]
async fn transient_projection_failure_recovers_without_restart() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.otlp.enabled = false;
    config.lifecycle.idle_ms = 10_000;
    config.stream.microbatch_wait_ms = 5;
    config.stream.projection_retry_page = 1;
    config.stream.projection_retry_initial_ms = 10;
    config.stream.projection_retry_max_ms = 40;

    let layout = WorkspaceStoreLayout::new(&config.workspace_dir);
    let store = WorkspaceStore::open(&layout, &config.workspace_id).unwrap();
    store
        .create_project(&CreateProjectV1 {
            project_id: "test-agent".into(),
            display_name: "Test Agent".into(),
            artifact_namespace: "test-agent".into(),
        })
        .unwrap();
    let raw = request().encode_to_vec();
    let receiver_config = OtlpReceiverConfig {
        enabled: false,
        bind_addr: config.otlp.bind_addr,
        source_id: config.otlp.source_id.clone(),
        max_wire_bytes: config.otlp.max_wire_bytes,
        max_decoded_bytes: config.otlp.max_decoded_bytes,
        max_spans_per_request: config.otlp.max_spans_per_request,
        max_attributes_per_span: config.otlp.max_attributes_per_span,
        retry_after_seconds: config.otlp.retry_after_seconds,
    };
    let mut submission =
        prepare_otlp_submission(&receiver_config, raw, "application/x-protobuf", "identity")
            .unwrap();
    let receipt = store
        .journal_batch(
            &mut submission.batch,
            &submission.raw_wire_payload,
            &submission.wire_encoding,
            config.blobs.inline_attribute_bytes,
        )
        .unwrap();
    drop(store);

    let normalized_path = layout
        .blob_directory()
        .join(&receipt.normalized_blob.sha256[..2])
        .join(format!("{}.zst", receipt.normalized_blob.sha256));
    let held_path = normalized_path.with_extension("zst.held");
    std::fs::rename(&normalized_path, &held_path).unwrap();

    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let live = runtime.live().unwrap();
    let degraded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let health = live.source_health().unwrap();
            if health.projection_degraded && health.projection_retry_count > 0 {
                break health;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(degraded.projection_lag, 1);
    assert!(degraded.projection_backlog_age_ms > 0);
    assert!(degraded.projection_last_error.is_some());

    std::fs::rename(&held_path, &normalized_path).unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let health = live.source_health().unwrap();
            if health.projection_lag == 0 && !health.projection_degraded {
                break health;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(live.run_count().unwrap(), 1);
    assert!(recovered.projection_last_error.is_none());
    assert!(recovered.projection_retry_count > 0);
    runtime.shutdown();
}

#[tokio::test]
async fn imports_bounded_otlp_file_into_the_explicit_project() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.otlp.enabled = false;
    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let live = runtime.live().unwrap().clone();
    live.create_project(CreateProjectV1 {
        project_id: "import-destination".into(),
        display_name: "Import Destination".into(),
        artifact_namespace: "import-destination".into(),
    })
    .unwrap();
    let (_, subscription) = runtime.snapshot_and_subscribe().unwrap();
    let path = temp.path().join("trace.pb");
    std::fs::write(&path, request().encode_to_vec()).unwrap();

    let result = live.import_otlp_file("import-destination", &path).unwrap();
    assert_eq!(result.accepted_spans, 1);
    assert_eq!(result.rejected_spans, 0);
    assert!(!result.duplicate_request);

    let delta = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.summary.project_id, "import-destination");

    let duplicate = live.import_otlp_file("import-destination", &path).unwrap();
    assert!(duplicate.duplicate_request);
    live.create_project(CreateProjectV1 {
        project_id: "other-destination".into(),
        display_name: "Other Destination".into(),
        artifact_namespace: "other-destination".into(),
    })
    .unwrap();
    let other_project = live.import_otlp_file("other-destination", &path).unwrap();
    assert!(!other_project.duplicate_request);
    runtime.shutdown();
}

#[tokio::test]
async fn local_demo_reaches_a_repeated_failure_group() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.otlp.enabled = false;
    config.lifecycle.idle_ms = 20;
    config.lifecycle.finalization_grace_ms = 20;
    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let live = runtime.live().unwrap().clone();
    live.create_project(CreateProjectV1 {
        project_id: "demo-agent".into(),
        display_name: "Demo Agent".into(),
        artifact_namespace: "demo-agent".into(),
    })
    .unwrap();

    let imported = live.load_local_demo("demo-agent").unwrap();
    assert_eq!(imported.accepted_spans, 33);
    assert!(!imported.duplicate_request);

    let groups = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let groups = live
                .list_failure_groups(
                    &FailureFiltersV1 {
                        scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                            project_id: Some("demo-agent".into()),
                            ..QueryScopeCriteriaV1::default()
                        }),
                        ..FailureFiltersV1::default()
                    },
                    0,
                    10,
                )
                .unwrap();
            if groups
                .first()
                .is_some_and(|group| group.affected_run_count == 3)
            {
                break groups;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].detector_ids, vec!["terminal_tool_failure"]);
    assert_eq!(groups[0].occurrence_count, 9);
    assert_eq!(groups[0].affected_run_count, 3);
    assert_eq!(
        live.list_failure_occurrences(&groups[0].group_id, 0, 20)
            .unwrap()
            .len(),
        9
    );
    runtime.shutdown();
}

#[tokio::test]
async fn journals_projects_subscribes_and_finalizes_otlp() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.lifecycle.idle_ms = 100;
    config.lifecycle.finalization_grace_ms = 100;
    config.analysis.minimum_findings = 1;
    config.analysis.cohort_rebuild_debounce_ms = 10;
    config.analysis.feature_similarity_enabled = true;
    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let live = runtime.live().unwrap().clone();
    let project = live
        .create_project(CreateProjectV1 {
            project_id: "test-agent".into(),
            display_name: "Test Agent".into(),
            artifact_namespace: "test-agent".into(),
        })
        .unwrap();
    assert_eq!(live.list_projects().unwrap(), vec![project]);
    let address = live.source_health().unwrap().effective_address.unwrap();
    let (_, subscription) = runtime.snapshot_and_subscribe().unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(request().encode_to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let delta = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.summary.span_count, 1);
    assert_eq!(delta.summary.project_id, "test-agent");
    let spans = live
        .list_spans(&delta.logical_trace_id, delta.revision, 0, 10, None, false)
        .unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].category, "agent");
    assert!(spans[0].payload_refs.contains_key("gen_ai.input.messages"));

    let finalized = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let committed = subscription.recv().await.unwrap();
            if committed.change == TraceChangeKind::FindingsCommitted {
                break committed.summary;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(finalized.lifecycle, TraceLifecycle::Finalized);
    assert_eq!(finalized.project_id, "test-agent");
    assert_eq!(finalized.session_id.as_deref(), Some("session-42"));
    assert_eq!(finalized.build_id.as_deref(), Some("build-7"));
    assert_eq!(finalized.environment.as_deref(), Some("staging"));
    assert_eq!(
        finalized.identity_quality,
        perseval_service::IdentityQualityV1::Explicit
    );
    let scoped_runs = live
        .list_runs_filtered(
            &perseval_service::RunFiltersV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    project_id: Some("test-agent".into()),
                    session_id: Some("session-42".into()),
                    build_id: Some("build-7".into()),
                    environment: Some("staging".into()),
                    ..QueryScopeCriteriaV1::default()
                }),
                ..Default::default()
            },
            0,
            20,
        )
        .unwrap();
    assert_eq!(scoped_runs.len(), 1);
    assert!(finalized.finding_count > 0);
    let groups = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let groups = live
                .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
                .unwrap();
            if groups.first().is_some_and(|group| {
                live.get_failure_group(&group.group_id)
                    .unwrap()
                    .is_some_and(|detail| !detail.summary.feature_similarity_cohorts.is_empty())
            }) {
                break groups;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!groups.is_empty());
    assert!(groups[0].feature_similarity_cohorts.is_empty());
    assert!(
        !live
            .get_failure_group(&groups[0].group_id)
            .unwrap()
            .unwrap()
            .summary
            .feature_similarity_cohorts
            .is_empty()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let health = live.source_health().unwrap();
            if health.topology_pending == 0 && health.topology_running == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    live.rebuild_feature_cohorts(Some("test-agent")).unwrap();
    let occurrences = live
        .list_failure_occurrences(&groups[0].group_id, 0, 10)
        .unwrap();
    let evidence = live
        .get_finding_evidence(&groups[0].group_id, &occurrences[0].finding.finding_id)
        .unwrap()
        .unwrap();
    assert!(evidence.spans.len() <= 128);
    let preview = live
        .preview_eval_candidate(&occurrences[0].finding.finding_id)
        .unwrap()
        .unwrap();
    assert_eq!(preview.candidate.source_finding_ids.len(), 1);
    assert!(preview.candidate.source_cluster_refs.is_empty());

    let batch_preview = live
        .preview_eval_batch(
            "test-agent",
            &EvalBatchSelectionSpecV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    project_id: Some("test-agent".into()),
                    ..QueryScopeCriteriaV1::default()
                }),
                group_ids: vec![groups[0].group_id.clone()],
                policy: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(batch_preview.items.len(), 1);
    let queued = live
        .create_eval_batch(
            "test-agent",
            &batch_preview.preview_id,
            &batch_preview.selection_hash,
            "live-otlp-batch-1",
        )
        .unwrap();
    assert!(matches!(
        queued.status,
        CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
    ));
    let generation = (0..100)
        .find_map(|_| {
            let job = live
                .get_candidate_generation_job(&queued.job_id)
                .unwrap()
                .unwrap();
            if matches!(
                job.status,
                CandidateGenerationJobStatusV1::Queued | CandidateGenerationJobStatusV1::Running
            ) {
                std::thread::sleep(Duration::from_millis(10));
                None
            } else {
                Some(job)
            }
        })
        .expect("candidate generation finishes asynchronously");
    assert_eq!(generation.status, CandidateGenerationJobStatusV1::Succeeded);
    assert_eq!(generation.outcomes.len(), 1);
    let candidates = live
        .list_eval_candidates(Some("test-agent"), 0, 20)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    let reviewed = live
        .review_eval_candidate(
            "test-agent",
            &candidates[0].candidate.candidate_id,
            EvalReviewDecisionV1::Reject,
            Some("Not a stable regression case".into()),
        )
        .unwrap();
    assert_eq!(reviewed.queue_state, EvalReviewQueueStateV1::Rejected);
    assert!(reviewed.candidate.review.is_some());

    let duplicate = reqwest::Client::new()
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(request().encode_to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), reqwest::StatusCode::OK);

    let mut corrected_request = request();
    corrected_request.resource_spans[0].scope_spans[0].spans[0].name =
        "agent.chat.corrected".into();
    let corrected = reqwest::Client::new()
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(corrected_request.encode_to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(corrected.status(), reqwest::StatusCode::OK);
    let reopened = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = live.get_run(&delta.logical_trace_id).unwrap().unwrap();
            if run.revision == 2 {
                break run;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(reopened.revision, 2);
    assert_eq!(reopened.analysis_status, AnalysisStatus::Reanalyzing);
    assert!(matches!(
        reopened.lifecycle,
        TraceLifecycle::Reopened | TraceLifecycle::Quiescent | TraceLifecycle::Finalized
    ));
    let stale_groups = live
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert!(stale_groups.iter().any(|group| group.reanalyzing));
    runtime.shutdown();
    let restarted = ServiceRuntime::start_embedded(live_config(&temp)).unwrap();
    assert_eq!(
        restarted
            .live()
            .unwrap()
            .source_health()
            .unwrap()
            .accepted_spans,
        2
    );
    restarted.shutdown();
}

#[tokio::test]
async fn burst_builds_bounded_project_scoped_feature_models() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.lifecycle.idle_ms = 25;
    config.lifecycle.finalization_grace_ms = 25;
    config.analysis.minimum_findings = 3;
    config.analysis.cohort_rebuild_debounce_ms = 25;
    config.analysis.feature_similarity_enabled = true;
    let runtime = ServiceRuntime::start_embedded(config.clone()).unwrap();
    let live = runtime.live().unwrap().clone();
    for project_id in ["project-a", "project-b"] {
        live.create_project(CreateProjectV1 {
            project_id: project_id.into(),
            display_name: project_id.into(),
            artifact_namespace: project_id.into(),
        })
        .unwrap();
    }
    let address = live.source_health().unwrap().effective_address.unwrap();
    let client = reqwest::Client::new();
    let started = Instant::now();
    for index in 0..100 {
        let project_id = if index < 50 { "project-a" } else { "project-b" };
        let response = client
            .post(format!("http://{address}/v1/traces"))
            .header("content-type", "application/x-protobuf")
            .body(request_for_project(project_id, index).encode_to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }
    assert!(started.elapsed() < Duration::from_secs(10));

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let runs = live.list_runs(0, 200).unwrap();
            let health = live.source_health().unwrap();
            let projects_ready = ["project-a", "project-b"].into_iter().all(|project_id| {
                let filters = FailureFiltersV1 {
                    scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                        project_id: Some(project_id.into()),
                        ..QueryScopeCriteriaV1::default()
                    }),
                    ..FailureFiltersV1::default()
                };
                live.list_failure_groups(&filters, 0, 10)
                    .unwrap()
                    .first()
                    .is_some_and(|group| {
                        live.get_failure_group(&group.group_id)
                            .unwrap()
                            .is_some_and(|detail| {
                                !detail.summary.feature_similarity_cohorts.is_empty()
                            })
                    })
            });
            if runs.len() == 100
                && runs.iter().all(|run| run.finding_count > 0)
                && health.analysis_pending == 0
                && health.analysis_running == 0
                && health.cohort_assignment_pending == 0
                && health.cohort_rebuild_pending == 0
                && !health.cohort_running
                && health.topology_pending == 0
                && health.topology_running == 0
                && projects_ready
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown();

    let store = WorkspaceStore::open(
        &WorkspaceStoreLayout::new(&config.workspace_dir),
        &config.workspace_id,
    )
    .unwrap();
    let diagnostics = store.pipeline_diagnostics().unwrap();
    assert!(diagnostics.feature_similarity_models_built <= 2);
    assert_eq!(diagnostics.feature_similarity_assignments_written, 100);
    let project_a = store
        .active_feature_similarity_model_for_project("project-a")
        .unwrap()
        .unwrap();
    let project_b = store
        .active_feature_similarity_model_for_project("project-b")
        .unwrap()
        .unwrap();
    assert_ne!(project_a.model_id, project_b.model_id);
    assert_eq!(project_a.assignments.len(), 50);
    assert_eq!(project_b.assignments.len(), 50);
    assert_eq!(project_a.metadata["perseval_project_id"], "project-a");
    assert_eq!(project_b.metadata["perseval_project_id"], "project-b");
    assert_eq!(project_a.metadata["cluster_quality_evaluation"], "sampled");
    assert_eq!(project_b.metadata["cluster_quality_evaluation"], "sampled");
}

#[tokio::test]
async fn feature_similarity_does_not_change_exact_groups_or_draft_evals() {
    let (enabled_groups, enabled_preview, enabled_cohort_count) =
        feature_similarity_invariance_snapshot(true).await;
    let (disabled_groups, disabled_preview, disabled_cohort_count) =
        feature_similarity_invariance_snapshot(false).await;

    assert!(enabled_cohort_count > 0);
    assert_eq!(disabled_cohort_count, 0);
    assert_eq!(enabled_groups, disabled_groups);
    assert_eq!(enabled_preview, disabled_preview);
}

async fn feature_similarity_invariance_snapshot(
    enabled: bool,
) -> (serde_json::Value, serde_json::Value, usize) {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.lifecycle.idle_ms = 25;
    config.lifecycle.finalization_grace_ms = 25;
    config.analysis.minimum_findings = 3;
    config.analysis.cohort_rebuild_debounce_ms = 10;
    config.analysis.feature_similarity_enabled = enabled;
    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let live = runtime.live().unwrap().clone();
    live.create_project(CreateProjectV1 {
        project_id: "invariance-project".into(),
        display_name: "Invariance Project".into(),
        artifact_namespace: "invariance-project".into(),
    })
    .unwrap();
    let address = live.source_health().unwrap().effective_address.unwrap();
    let client = reqwest::Client::new();
    for index in 0..3 {
        let response = client
            .post(format!("http://{address}/v1/traces"))
            .header("content-type", "application/x-protobuf")
            .body(request_for_project("invariance-project", index).encode_to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    let mut groups = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let groups = live
                .list_failure_groups(
                    &FailureFiltersV1 {
                        scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                            project_id: Some("invariance-project".into()),
                            ..QueryScopeCriteriaV1::default()
                        }),
                        ..FailureFiltersV1::default()
                    },
                    0,
                    20,
                )
                .unwrap();
            let health = live.source_health().unwrap();
            let exact_groups_ready = groups
                .first()
                .is_some_and(|group| group.occurrence_count == 3);
            let feature_similarity_ready = !enabled
                || groups.first().is_some_and(|group| {
                    live.get_failure_group(&group.group_id)
                        .unwrap()
                        .is_some_and(|detail| !detail.summary.feature_similarity_cohorts.is_empty())
                });
            if live.run_count().unwrap() == 3
                && exact_groups_ready
                && feature_similarity_ready
                && health.analysis_pending == 0
                && health.analysis_running == 0
                && health.cohort_assignment_pending == 0
                && health.cohort_rebuild_pending == 0
                && !health.cohort_running
            {
                break groups;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        groups
            .iter()
            .all(|group| group.feature_similarity_cohorts.is_empty())
    );
    let cohort_count = groups
        .iter()
        .map(|group| {
            live.get_failure_group(&group.group_id)
                .unwrap()
                .map_or(0, |detail| detail.summary.feature_similarity_cohorts.len())
        })
        .sum();
    let group_ids = groups
        .iter()
        .map(|group| group.group_id.clone())
        .collect::<Vec<_>>();
    let mut preview = live
        .preview_eval_batch(
            "invariance-project",
            &EvalBatchSelectionSpecV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    project_id: Some("invariance-project".into()),
                    ..QueryScopeCriteriaV1::default()
                }),
                group_ids,
                policy: Default::default(),
            },
        )
        .unwrap();
    // Wall-clock creation time is audit metadata, not eval content or
    // selection. Normalize it so this assertion compares the draft itself.
    preview.created_at_unix_ms = 0;
    for group in &mut groups {
        group.feature_similarity_cohorts.clear();
    }
    let groups = serde_json::to_value(groups).unwrap();
    let preview = serde_json::to_value(preview).unwrap();
    runtime.shutdown();
    (groups, preview, cohort_count)
}

#[tokio::test]
async fn returns_retryable_overload_when_byte_budget_is_exhausted() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.stream.queue_bytes = 1;
    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let address = runtime
        .live()
        .unwrap()
        .source_health()
        .unwrap()
        .effective_address
        .unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(request().encode_to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "1");
    runtime.shutdown();
}

#[tokio::test]
async fn accepts_json_gzip_and_reports_permanent_transport_errors() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = ServiceRuntime::start_embedded(live_config(&temp)).unwrap();
    let address = runtime
        .live()
        .unwrap()
        .source_health()
        .unwrap()
        .effective_address
        .unwrap();
    let json = serde_json::to_vec(&request()).unwrap();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&json).unwrap();
    let compressed = encoder.finish().unwrap();
    let client = reqwest::Client::new();

    let accepted = client
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);

    let malformed = client
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(vec![0xff])
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);

    let unsupported = client
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "text/plain")
        .body("not otlp")
        .send()
        .await
        .unwrap();
    assert_eq!(
        unsupported.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    runtime.shutdown();
}

#[tokio::test]
async fn projects_concurrent_small_requests_as_one_microbatch() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = live_config(&temp);
    config.stream.microbatch_spans = 2;
    config.stream.microbatch_wait_ms = 100;
    config.lifecycle.idle_ms = 10_000;
    let runtime = ServiceRuntime::start_embedded(config).unwrap();
    let address = runtime
        .live()
        .unwrap()
        .source_health()
        .unwrap()
        .effective_address
        .unwrap();
    let (_, subscription) = runtime.snapshot_and_subscribe().unwrap();
    let mut second = request();
    second.resource_spans[0].scope_spans[0].spans[0].span_id = vec![3; 8];
    second.resource_spans[0].scope_spans[0].spans[0].name = "agent.tool".into();
    second.resource_spans[0].scope_spans[0].spans[0].start_time_unix_nano = 3_000_000;
    second.resource_spans[0].scope_spans[0].spans[0].end_time_unix_nano = 8_000_000;
    second.resource_spans[0].scope_spans[0].spans[0].status = Some(Status {
        code: 2,
        message: "failed".into(),
    });
    let client = reqwest::Client::new();
    let first_send = client
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(request().encode_to_vec())
        .send();
    let second_send = client
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(second.encode_to_vec())
        .send();
    let (first_response, second_response) = tokio::join!(first_send, second_send);
    assert_eq!(first_response.unwrap().status(), reqwest::StatusCode::OK);
    assert_eq!(second_response.unwrap().status(), reqwest::StatusCode::OK);
    let delta = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delta.summary.span_count, 2);
    assert_eq!(delta.summary.error_count, 1);
    assert_eq!(delta.summary.start_time_unix_nano, 1_000_000);
    assert_eq!(delta.summary.end_time_unix_nano, 8_000_000);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), subscription.recv())
            .await
            .is_err()
    );

    let mut corrected = request();
    corrected.resource_spans[0].scope_spans[0].spans[0].name = "agent.chat.corrected".into();
    corrected.resource_spans[0].scope_spans[0].spans[0].start_time_unix_nano = 4_000_000;
    corrected.resource_spans[0].scope_spans[0].spans[0].end_time_unix_nano = 5_000_000;
    corrected.resource_spans[0].scope_spans[0].spans[0].status = Some(Status {
        code: 2,
        message: "also failed".into(),
    });
    let response = client
        .post(format!("http://{address}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(corrected.encode_to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let corrected_delta = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(corrected_delta.summary.span_count, 2);
    assert_eq!(corrected_delta.summary.error_count, 2);
    assert_eq!(corrected_delta.summary.start_time_unix_nano, 3_000_000);
    assert_eq!(corrected_delta.summary.end_time_unix_nano, 8_000_000);
    runtime.shutdown();
}

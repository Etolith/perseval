#[test]
fn immutable_analysis_identity_rejects_changed_content() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    ingest(&store, "trace-collision", "root", None);
    let first = result("trace-collision", 1, Vec::new());
    store.commit_analysis(&first).unwrap();

    let mut changed = first.clone();
    changed
        .behavior
        .metadata
        .insert("changed".into(), Value::Bool(true));

    assert!(store.commit_analysis(&changed).is_err());
}

#[test]
fn changed_analysis_definition_requeues_finalized_results_without_replacing_them_early() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-stale", "root", None);
    finalize(&store);
    let request = store.pending_analysis_requests(10).unwrap().remove(0);
    store.mark_analysis_started(&request).unwrap();
    let first = result(
        "trace-stale",
        1,
        vec![finding("trace-stale", "finding-old", "signature-old")],
    );
    store.commit_analysis(&first).unwrap();

    assert!(
        store
            .enqueue_stale_analyses(&definition(&first))
            .unwrap()
            .is_empty()
    );

    let mut expected = definition(&first);
    expected
        .detector_versions
        .insert("false_success_claim".into(), "4".into());
    expected.grouping_version = "test.known_signature_group.v2".into();
    let deltas = store.enqueue_stale_analyses(&expected).unwrap();
    assert_eq!(deltas.len(), 1);
    assert_eq!(
        deltas[0].change,
        perseval_store::TraceChangeKind::Reanalyzing
    );
    assert_eq!(
        deltas[0].summary.analysis_status,
        AnalysisStatus::Reanalyzing
    );
    assert!(store.enqueue_stale_analyses(&expected).unwrap().is_empty());

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let active_before: String = control
        .query_row(
            "SELECT analysis_id FROM active_analysis_runs WHERE logical_trace_id = ?1",
            ["trace-stale"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_before, first.analysis_id);
    drop(control);

    let request = store.pending_analysis_requests(10).unwrap().remove(0);
    assert!(request.reanalysis);
    store.mark_analysis_started(&request).unwrap();
    let mut replacement = result("trace-stale", 1, Vec::new());
    replacement.identity.detector_versions = expected.detector_versions.clone();
    replacement.identity.grouping_version = expected.grouping_version.clone();
    replacement.detection_report.detector_versions = expected.detector_versions;
    replacement.analysis_id = replacement.identity.analysis_id();
    store.commit_analysis(&replacement).unwrap();

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let active_after: String = control
        .query_row(
            "SELECT analysis_id FROM active_analysis_runs WHERE logical_trace_id = ?1",
            ["trace-stale"],
            |row| row.get(0),
        )
        .unwrap();
    let version_count: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM analysis_runs WHERE logical_trace_id = ?1",
            ["trace-stale"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_after, replacement.analysis_id);
    assert_eq!(version_count, 2);
    assert_eq!(
        store
            .get_run("trace-stale")
            .unwrap()
            .unwrap()
            .analysis_status,
        AnalysisStatus::Ready
    );
}

#[test]
fn finalized_findings_drive_inbox_evidence_and_selected_candidate() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    ingest(&store, "trace-1", "root", None);
    let mut nearby = span("trace-1", "nearby-tool", Some("root"));
    nearby.category = "tool".into();
    let mut nearby_batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        received_at_unix_ms: 1,
        spans: vec![nearby],
        rejected_spans: 0,
        rejection_message: None,
    };
    let receipt = store
        .journal_batch(&mut nearby_batch, b"trace-1:nearby-tool", "test", 4096)
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();
    finalize(&store);

    let request = store.pending_analysis_requests(10).unwrap().remove(0);
    assert_eq!(request.revision, 1);
    store.mark_analysis_started(&request).unwrap();
    assert_eq!(
        store.get_run("trace-1").unwrap().unwrap().analysis_status,
        AnalysisStatus::Analyzing
    );
    store
        .commit_analysis(&result(
            "trace-1",
            1,
            vec![finding("trace-1", "finding-1", "sha256:same")],
        ))
        .unwrap();

    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].unrecovered_count, 1);
    assert_eq!(groups[0].subject.as_deref(), Some("cancel_card"));
    assert_eq!(
        groups[0]
            .presentation
            .as_ref()
            .map(|presentation| presentation.title.as_str()),
        Some("Success was claimed without supporting evidence")
    );
    let detail = store
        .get_failure_group(&groups[0].group_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.explanation,
        "Grouped because these findings have the same deterministic failure signature."
    );
    let occurrences = store
        .list_failure_occurrences(&groups[0].group_id, 0, 10)
        .unwrap();
    assert_eq!(occurrences[0].finding.finding_id, "finding-1");
    let evidence = store
        .get_finding_evidence(&groups[0].group_id, "finding-1", 8)
        .unwrap()
        .unwrap();
    assert_eq!(
        evidence
            .presentation
            .as_ref()
            .map(|presentation| presentation.finding_id.as_str()),
        Some("finding-1")
    );
    assert_eq!(evidence.spans.len(), 2);
    assert!(
        evidence
            .spans
            .iter()
            .any(|span| span.span_id == "nearby-tool")
    );
    let payload = evidence
        .spans
        .iter()
        .find(|span| span.span_id == "root")
        .unwrap()
        .payload_refs
        .get("input.value")
        .expect("known payload is externalized");
    assert_eq!(store.reveal_blob(&payload.sha256, 7).unwrap().len(), 7);

    let preview = store.preview_eval_candidate("finding-1").unwrap().unwrap();
    assert!(preview.evidence_packet.content_hash.starts_with("sha256:"));
    assert_eq!(preview.candidate.source_finding_ids, ["finding-1"]);

    let candidate = store
        .create_eval_candidate(&groups[0].group_id, "finding-1")
        .unwrap()
        .unwrap();
    assert_eq!(candidate.status, EvalCandidateStatus::Candidate);
    assert_eq!(candidate.source_finding_ids, ["finding-1"]);
    assert!(candidate.source_cluster_refs.is_empty());
}

#[test]
fn group_and_bulk_eval_generation_is_project_scoped_bounded_and_idempotent() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    for project_id in ["checkout", "support"] {
        store
            .create_project(&CreateProjectV1 {
                project_id: project_id.into(),
                display_name: project_id.into(),
                artifact_namespace: project_id.into(),
            })
            .unwrap();
    }
    for trace in ["checkout-new", "checkout-recovered"] {
        ingest_project(&store, "checkout", trace, "root", None);
    }
    ingest_project(&store, "support", "support-same-signature", "root", None);
    finalize(&store);
    let mut tampered_scope = scope(Some("checkout"));
    tampered_scope.criteria.build_id = Some("silently-widened".into());
    assert!(
        store
            .compare_runs(
                &RunComparisonRequestV1 {
                    schema_version: RUN_COMPARISON_REQUEST_SCHEMA_VERSION.into(),
                    scope: tampered_scope,
                    baseline_trace_id: "checkout-new".into(),
                    baseline_revision: 1,
                    candidate_trace_id: "checkout-recovered".into(),
                    candidate_revision: 1,
                },
                100_000,
                TraceAlignmentOptions::default(),
            )
            .is_err(),
        "a mutated scope must not be accepted under its old identity"
    );
    let comparison = store
        .compare_runs(
            &RunComparisonRequestV1 {
                schema_version: RUN_COMPARISON_REQUEST_SCHEMA_VERSION.into(),
                scope: scope(Some("checkout")),
                baseline_trace_id: "checkout-new".into(),
                baseline_revision: 1,
                candidate_trace_id: "checkout-recovered".into(),
                candidate_revision: 1,
            },
            100_000,
            TraceAlignmentOptions::default(),
        )
        .unwrap();
    assert!(comparison.first_meaningful_divergence.is_none());
    assert_eq!(comparison.common_prefix_steps, 1);
    assert_eq!(
        store
            .get_trace_comparison(&comparison.comparison_id)
            .unwrap(),
        Some(comparison)
    );
    for request in store.pending_analysis_requests(10).unwrap() {
        store.mark_analysis_started(&request).unwrap();
        let mut detected = finding(
            &request.logical_trace_id,
            &format!("finding-{}", request.logical_trace_id),
            "signature-shared-across-projects",
        );
        if request.logical_trace_id == "checkout-recovered" {
            detected.recovery = RecoveryStatus::Recovered;
            detected.created_at = "2026-07-11T13:00:00Z".into();
        }
        store
            .commit_analysis(&result(
                &request.logical_trace_id,
                request.revision,
                vec![detected],
            ))
            .unwrap();
    }

    let portfolio = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(
        portfolio.len(),
        2,
        "projects must not merge group authority"
    );
    assert_ne!(portfolio[0].project_id, portfolio[1].project_id);
    let checkout_group = portfolio
        .iter()
        .find(|group| group.project_id == "checkout")
        .unwrap();
    assert_eq!(checkout_group.occurrence_count, 2);
    assert_eq!(checkout_group.occurrence_trend.iter().sum::<u64>(), 2);
    let recurrence = checkout_group.recurrence.as_ref().unwrap();
    assert_eq!(
        recurrence
            .buckets
            .iter()
            .map(|bucket| bucket.eligible_run_count)
            .sum::<u64>(),
        2
    );

    let preview = store
        .preview_eval_batch(
            "checkout",
            &EvalBatchSelectionSpecV1 {
                scope: scope(Some("checkout")),
                group_ids: vec![checkout_group.group_id.clone()],
                policy: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(preview.project_id, "checkout");
    assert_eq!(preview.items.len(), 2);
    assert!(
        preview
            .items
            .iter()
            .all(|item| item.project_id == "checkout")
    );
    assert!(
        preview
            .items
            .iter()
            .all(|item| item.candidate.status == EvalCandidateStatus::Candidate)
    );
    assert!(preview.selection_hash.starts_with("sha256:"));

    let job = store
        .create_eval_batch(
            "checkout",
            &preview.preview_id,
            &preview.selection_hash,
            "inbox-selection-1",
        )
        .unwrap();
    assert_eq!(job.status, CandidateGenerationJobStatusV1::Succeeded);
    assert_eq!(job.outcomes.len(), 2);
    assert!(job.outcomes.iter().all(|outcome| {
        outcome.outcome == CandidateGenerationOutcomeKindV1::Created
            && outcome.project_id == "checkout"
    }));

    let retried = store
        .create_eval_batch(
            "checkout",
            &preview.preview_id,
            &preview.selection_hash,
            "inbox-selection-1",
        )
        .unwrap();
    assert_eq!(retried, job);
    let queued = store
        .queue_eval_batch(
            "checkout",
            &preview.preview_id,
            &preview.selection_hash,
            "inbox-selection-cancellable",
        )
        .unwrap();
    assert_eq!(queued.status, CandidateGenerationJobStatusV1::Queued);
    let cancelled = store
        .cancel_candidate_generation_job(&queued.job_id)
        .unwrap();
    assert_eq!(cancelled.status, CandidateGenerationJobStatusV1::Cancelled);
    assert_eq!(
        store
            .execute_candidate_generation_job(&queued.job_id)
            .unwrap()
            .status,
        CandidateGenerationJobStatusV1::Cancelled
    );
    let retry = store
        .retry_candidate_generation_job(&queued.job_id)
        .unwrap();
    assert_eq!(retry.status, CandidateGenerationJobStatusV1::Queued);
    let retry = store
        .execute_candidate_generation_job(&queued.job_id)
        .unwrap();
    assert_eq!(retry.status, CandidateGenerationJobStatusV1::Succeeded);
    assert!(
        retry
            .outcomes
            .iter()
            .all(|outcome| { outcome.outcome == CandidateGenerationOutcomeKindV1::AlreadyExists })
    );
    let candidates = store.list_eval_candidates(Some("checkout"), 0, 20).unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.queue_state == EvalReviewQueueStateV1::Pending)
    );
    let deferred_candidate_id = candidates[1].candidate.candidate_id.clone();
    let deferred_after_restart = store
        .review_eval_candidate(&ReviewEvalCandidateV1 {
            project_id: "checkout".into(),
            candidate_id: deferred_candidate_id.clone(),
            decision: EvalReviewDecisionV1::Defer,
            reviewer_ref: "test-reviewer".into(),
            reviewed_at: "2026-07-12T11:59:00Z".into(),
            reason: Some("Keep this in the review queue".into()),
        })
        .unwrap();
    assert_eq!(
        deferred_after_restart.queue_state,
        EvalReviewQueueStateV1::Deferred
    );
    let candidate_id = candidates[0].candidate.candidate_id.clone();
    let deferred = store
        .review_eval_candidate(&ReviewEvalCandidateV1 {
            project_id: "checkout".into(),
            candidate_id: candidate_id.clone(),
            decision: EvalReviewDecisionV1::Defer,
            reviewer_ref: "test-reviewer".into(),
            reviewed_at: "2026-07-12T12:00:00Z".into(),
            reason: Some("Needs a domain owner".into()),
        })
        .unwrap();
    assert_eq!(deferred.queue_state, EvalReviewQueueStateV1::Deferred);
    assert_eq!(deferred.candidate.status, EvalCandidateStatus::Candidate);
    let accepted = store
        .review_eval_candidate(&ReviewEvalCandidateV1 {
            project_id: "checkout".into(),
            candidate_id,
            decision: EvalReviewDecisionV1::Accept,
            reviewer_ref: "test-reviewer".into(),
            reviewed_at: "2026-07-12T12:01:00Z".into(),
            reason: Some("Grounded and actionable".into()),
        })
        .unwrap();
    assert_eq!(accepted.queue_state, EvalReviewQueueStateV1::Accepted);
    assert_eq!(accepted.candidate.status, EvalCandidateStatus::Accepted);
    let accepted_id = accepted.candidate.candidate_id.clone();
    assert_eq!(
        accepted.candidate.review.unwrap().reviewer_ref,
        "test-reviewer"
    );
    assert!(
        store
            .review_eval_candidate(&ReviewEvalCandidateV1 {
                project_id: "support".into(),
                candidate_id: candidates[1].candidate.candidate_id.clone(),
                decision: EvalReviewDecisionV1::Reject,
                reviewer_ref: "test-reviewer".into(),
                reviewed_at: "2026-07-12T12:02:00Z".into(),
                reason: None,
            })
            .is_err(),
        "review mutation must be project scoped"
    );
    assert!(
        store
            .preview_eval_batch(
                "all-projects",
                &EvalBatchSelectionSpecV1 {
                    scope: scope(None),
                    group_ids: vec![checkout_group.group_id.clone()],
                    policy: Default::default(),
                },
            )
            .is_err(),
        "portfolio scope must reject mutations"
    );
    assert!(
        store
            .preview_eval_batch(
                "checkout",
                &EvalBatchSelectionSpecV1 {
                    scope: scope(Some("support")),
                    group_ids: vec![checkout_group.group_id.clone()],
                    policy: Default::default(),
                },
            )
            .is_err(),
        "eval generation must reject a project that differs from the immutable scope"
    );

    drop(store);
    let reopened = WorkspaceStore::open(&layout, "test").unwrap();
    let persisted = reopened
        .list_eval_candidates(Some("checkout"), 0, 20)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.candidate.candidate_id == accepted_id)
        .unwrap();
    assert_eq!(persisted.queue_state, EvalReviewQueueStateV1::Accepted);
    assert_eq!(persisted.candidate.status, EvalCandidateStatus::Accepted);
    assert_eq!(
        persisted.candidate.review.unwrap().reviewer_ref,
        "test-reviewer"
    );
    let persisted_deferred = reopened
        .list_eval_candidates(Some("checkout"), 0, 20)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.candidate.candidate_id == deferred_candidate_id)
        .unwrap();
    assert_eq!(
        persisted_deferred.queue_state,
        EvalReviewQueueStateV1::Deferred
    );
    assert_eq!(
        persisted_deferred.deferred_reason.as_deref(),
        Some("Keep this in the review queue")
    );
}

#[test]
fn late_span_keeps_old_group_reanalyzing_until_atomic_replacement() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    ingest(&store, "trace-1", "root", None);
    finalize(&store);
    let request = store.pending_analysis_requests(10).unwrap().remove(0);
    store.mark_analysis_started(&request).unwrap();
    store
        .commit_analysis(&result(
            "trace-1",
            1,
            vec![finding("trace-1", "finding-1", "sha256:same")],
        ))
        .unwrap();
    let group_id = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap()[0]
        .group_id
        .clone();

    ingest(&store, "trace-1", "late", Some("root"));
    let stale = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(stale[0].group_id, group_id);
    assert!(stale[0].reanalyzing);
    finalize(&store);
    let request = store.pending_analysis_requests(10).unwrap().remove(0);
    assert!(request.reanalysis);
    store.mark_analysis_started(&request).unwrap();
    assert_eq!(
        store
            .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
            .unwrap()
            .len(),
        1
    );

    store
        .commit_analysis(&result("trace-1", 2, Vec::new()))
        .unwrap();
    assert!(
        store
            .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn groups_are_ranked_and_filtered_deterministically() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    for trace in ["trace-high", "trace-critical"] {
        ingest(&store, trace, "root", None);
    }
    finalize(&store);
    for request in store.pending_analysis_requests(10).unwrap() {
        store.mark_analysis_started(&request).unwrap();
        let mut detected = finding(
            &request.logical_trace_id,
            &format!("finding-{}", request.logical_trace_id),
            &format!("signature-{}", request.logical_trace_id),
        );
        if request.logical_trace_id == "trace-critical" {
            detected.severity = FindingSeverity::Critical;
            detected.detector_id = "policy_violation".into();
            detected
                .metadata
                .insert("subject".into(), Value::String("payment_policy".into()));
        }
        store
            .commit_analysis(&result(
                &request.logical_trace_id,
                request.revision,
                vec![detected],
            ))
            .unwrap();
    }

    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].severity, FindingSeverity::Critical);

    let filtered = store
        .list_failure_groups(
            &FailureFiltersV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    service_name: Some("checkout".into()),
                    ..QueryScopeCriteriaV1::default()
                }),
                severity: Some(FindingSeverity::High),
                recovery: Some(RecoveryStatus::Unrecovered),
                detector_id: Some("false_success_claim".into()),
                search: Some("cancel_card".into()),
                include_fully_dismissed: false,
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].severity, FindingSeverity::High);
}

#[test]
fn failure_groups_respect_environment_build_session_and_time_scope() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    ingest_scoped(&store, "trace-prod", "production", "v2", "session-a");
    ingest_scoped(&store, "trace-stage", "staging", "v3", "session-b");
    finalize(&store);
    for request in store.pending_analysis_requests(10).unwrap() {
        store.mark_analysis_started(&request).unwrap();
        store
            .commit_analysis(&result(
                &request.logical_trace_id,
                request.revision,
                vec![finding(
                    &request.logical_trace_id,
                    &format!("finding-{}", request.logical_trace_id),
                    "shared-signature",
                )],
            ))
            .unwrap();
    }

    let scoped = store
        .list_failure_groups(
            &FailureFiltersV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    project_id: Some("checkout".into()),
                    environment: Some("production".into()),
                    build_id: Some("v2".into()),
                    session_id: Some("session-a".into()),
                    ..QueryScopeCriteriaV1::default()
                }),
                ..FailureFiltersV1::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].occurrence_count, 1);
    let expected_scope = scoped[0].scope.clone();
    let detail = store
        .get_failure_group_in_scope(&expected_scope, &scoped[0].group_id)
        .unwrap()
        .unwrap();
    assert_eq!(detail.summary.scope, expected_scope);
    let occurrences = store
        .list_failure_occurrences_in_scope(&expected_scope, &scoped[0].group_id, 0, 10)
        .unwrap();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].scope, expected_scope);
    let evidence = store
        .get_finding_evidence_in_scope(
            &expected_scope,
            &scoped[0].group_id,
            &occurrences[0].finding.finding_id,
            8,
        )
        .unwrap()
        .unwrap();
    assert_eq!(evidence.occurrence.scope, expected_scope);

    assert!(
        store
            .list_failure_groups(
                &FailureFiltersV1 {
                    scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                        project_id: Some("checkout".into()),
                        environment: Some("production".into()),
                        build_id: Some("v3".into()),
                        ..QueryScopeCriteriaV1::default()
                    }),
                    ..FailureFiltersV1::default()
                },
                0,
                10,
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_failure_groups(
                &FailureFiltersV1 {
                    scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                        project_id: Some("checkout".into()),
                        started_after_unix_nano: Some(2),
                        ..QueryScopeCriteriaV1::default()
                    }),
                    ..FailureFiltersV1::default()
                },
                0,
                10,
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn finalized_topology_is_persisted_and_survives_reopen() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    {
        let store = WorkspaceStore::open(&layout, "test").unwrap();
        ingest(&store, "trace-topology", "root", None);
        ingest(&store, "trace-topology", "child", Some("root"));
        finalize(&store);
        assert_eq!(store.topology_counts().unwrap(), (1, 0));

        // Simulate a crash after one bounded chunk. Opening the workspace again must recover the
        // running job and replay it idempotently from the beginning.
        let job = store.claim_pending_topology().unwrap().unwrap();
        let rows = store.build_topology_projection(&job).unwrap();
        assert_eq!(rows.len(), 2);
        let _ = store
            .commit_topology_chunk(&job, &rows[..1], true, false)
            .unwrap();
        assert_eq!(store.topology_counts().unwrap(), (0, 1));
    }

    let reopened = WorkspaceStore::open(&layout, "test").unwrap();
    assert_eq!(reopened.topology_counts().unwrap(), (1, 0));
    project_pending_topologies(&reopened, 1);
    assert_eq!(reopened.topology_counts().unwrap(), (0, 0));
    assert!(
        reopened
            .deltas_after(0, 100)
            .unwrap()
            .iter()
            .any(|delta| {
                delta.logical_trace_id == "trace-topology"
                    && delta.change == perseval_store::TraceChangeKind::TopologyCommitted
            })
    );

    let spans = reopened
        .list_spans("trace-topology", 1, 0, 10, None, false)
        .unwrap();
    let root = spans.iter().find(|span| span.span_id == "root").unwrap();
    assert_eq!(root.depth, 0);
    assert!(root.has_children);
    let child = reopened
        .get_span("trace-topology", 1, "child")
        .unwrap()
        .unwrap();
    assert_eq!(child.depth, 1);
}

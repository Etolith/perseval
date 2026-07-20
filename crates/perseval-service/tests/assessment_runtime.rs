use std::collections::{BTreeMap, BTreeSet};
use std::thread;
use std::time::Duration;

use perseval_service::assessments::{FoundationAssessmentExecutor, LearnedAssessmentExecutor};
use perseval_service::{LiveTraceService, PersevalConfigV1};
use perseval_store::{
    AssessmentCommitV1, AssessmentItemStatusV1, CreateProjectV1,
    PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION, ProjectAssessmentPolicyV1, ReviewAuthorityV1,
    SPAN_UPSERT_SCHEMA_VERSION, SpanUpsertBatchV1, SpanUpsertV1, WorkspaceStore,
    WorkspaceStoreLayout,
};
use serde_json::json;
use tempfile::TempDir;
use traces_to_evals::{
    AGENT_CONTEXT_RELEASE_SCHEMA_VERSION, AgentArchitectureContextV1, AgentContextReleaseV1,
    AgentEvaluationContextV1, AgentIdentityContextV1, AgentIntentContextV1, AgentPolicyContextV1,
    ContextFieldMetadataV1, ContextFieldProvenanceV1, ContextFieldV1, ContextReviewStateV1,
    ContextSensitivityV1, EVALUATOR_RELEASE_SCHEMA_VERSION, EvaluationEvidenceCatalogV1,
    EvaluationEvidenceCitationV1, EvaluationEvidenceKindV1, EvaluationEvidenceLocationV1,
    EvaluationEvidenceRecordV1, EvaluationImplementationV1, EvaluationInputBoundsV1,
    EvaluationTargetKind, EvaluatorReleaseSpecV1, LEARNED_EVALUATION_SCHEMA_VERSION,
    LearnedEvaluationV1, LearnedTaskKind, LearnedVerdictV1, ProviderResponseEnvelopeV1,
};

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn field(id: &str, value: serde_json::Value, source_snapshot_id: &str) -> ContextFieldV1 {
    ContextFieldV1 {
        metadata: ContextFieldMetadataV1 {
            field_id: id.into(),
            provenance: ContextFieldProvenanceV1::SystemInferred,
            source_snapshot_id: source_snapshot_id.into(),
            source_locator: Some(format!("README.md#{id}")),
            captured_at: "2026-07-19T00:00:00Z".into(),
            fresh_until: None,
            review_state: ContextReviewStateV1::Approved,
            sensitivity: ContextSensitivityV1::Public,
            inference_confidence: Some(0.9),
        },
        value,
    }
}

fn context_release(source_snapshot_id: &str, purpose: &str) -> AgentContextReleaseV1 {
    AgentContextReleaseV1 {
        schema_version: AGENT_CONTEXT_RELEASE_SCHEMA_VERSION.into(),
        agent_id: "agent-a".into(),
        identity: AgentIdentityContextV1 {
            application_name: field("application_name", json!("Test Agent"), source_snapshot_id),
            owner: field("owner", json!("QA"), source_snapshot_id),
            environment: field("environment", json!("test"), source_snapshot_id),
            build_version_selectors: vec![field(
                "build_selector",
                json!("build-1"),
                source_snapshot_id,
            )],
            entry_points: Vec::new(),
            user_personas: Vec::new(),
            supported_domains: Vec::new(),
            languages: Vec::new(),
            risk_tier: field("risk_tier", json!("low"), source_snapshot_id),
        },
        intent: AgentIntentContextV1 {
            purpose: field("purpose", json!(purpose), source_snapshot_id),
            supported_tasks: vec![field(
                "supported_task",
                json!("Answer the test request"),
                source_snapshot_id,
            )],
            explicit_non_goals: Vec::new(),
            success_criteria: Vec::new(),
            acceptable_partial_completion: None,
            refusal_requirements: Vec::new(),
            escalation_requirements: Vec::new(),
        },
        capabilities: Vec::new(),
        architecture: AgentArchitectureContextV1::default(),
        policy: AgentPolicyContextV1::default(),
        evaluation_context: AgentEvaluationContextV1::default(),
    }
}

fn evaluator(name: &str, hash_byte: char) -> EvaluatorReleaseSpecV1 {
    EvaluatorReleaseSpecV1 {
        schema_version: EVALUATOR_RELEASE_SCHEMA_VERSION.into(),
        name: name.into(),
        task_kind: LearnedTaskKind::TaskCompletion,
        target_kind: EvaluationTargetKind::TraceRevision,
        implementation: EvaluationImplementationV1::LocalClassifier {
            model_artifact_id: digest(hash_byte),
            tokenizer_artifact_id: digest('b'),
            feature_schema_id: digest('c'),
            runtime_version: "test-runtime-v1".into(),
        },
        projection_release_id: digest('d'),
        context_projection_release_id: digest('e'),
        applicable_taxonomy_node_ids: BTreeSet::new(),
        input_bounds: EvaluationInputBoundsV1 {
            max_subjects: 1,
            max_evidence_items: 16,
            max_input_bytes: 100_000,
            max_output_bytes: 10_000,
        },
        evidence_schema_version: "traceeval.evidence.v1".into(),
        abstention_policy: json!({"context_unresolved": "abstain"}),
        code_artifact_hash: digest(hash_byte),
    }
}

fn span() -> SpanUpsertV1 {
    SpanUpsertV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        external_trace_id: "trace-a".into(),
        external_span_id: "span-a".into(),
        external_parent_span_id: None,
        logical_trace_id: "trace-a".into(),
        content_hash: String::new(),
        observed_at_unix_nano: 20,
        name: "agent.run".into(),
        category: "agent".into(),
        span_kind: 0,
        start_time_unix_nano: 10,
        end_time_unix_nano: 20,
        status_code: 1,
        status_message: String::new(),
        trace_state: String::new(),
        flags: 0,
        dropped_attributes_count: 0,
        dropped_events_count: 0,
        dropped_links_count: 0,
        resource: BTreeMap::from([
            ("perseval.project.id".into(), json!("project-a")),
            ("gen_ai.agent.id".into(), json!("agent-a")),
            ("service.version".into(), json!("build-1")),
            ("deployment.environment.name".into(), json!("test")),
        ]),
        scope: BTreeMap::new(),
        attributes: BTreeMap::new(),
        payload_refs: BTreeMap::new(),
        payload_identities: BTreeMap::new(),
        events: Vec::new(),
        links: Vec::new(),
        decoder_version: "test".into(),
        semantic_mapping_version: "test".into(),
    }
}

fn setup() -> (TempDir, WorkspaceStore) {
    let directory = tempfile::tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "workspace-a").unwrap();
    store
        .create_project(&CreateProjectV1 {
            project_id: "project-a".into(),
            display_name: "Project A".into(),
            artifact_namespace: "project-a".into(),
        })
        .unwrap();
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        received_at_unix_ms: 1,
        spans: vec![span()],
        rejected_spans: 0,
        rejection_message: None,
    };
    let receipt = store
        .journal_batch(&mut batch, b"assessment-runtime", "test", 4096)
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();
    store.advance_lifecycle(i64::MAX / 4, 0, 0).unwrap();
    store.advance_lifecycle(i64::MAX / 4, 0, 0).unwrap();
    assert_eq!(store.list_runs(0, 10).unwrap()[0].finding_count, 0);
    (directory, store)
}

fn activate_context(store: &WorkspaceStore) -> (String, String) {
    let source_snapshot_id = store
        .record_context_source_snapshot(
            "project-a",
            "repository",
            "README.md",
            &digest('f'),
            "public",
            &json!({"files": [{"path": "README.md", "hash": digest('f')}]}),
        )
        .unwrap();
    let release = context_release(&source_snapshot_id, "Answer the test request");
    let draft = store
        .create_agent_context_draft(
            "project-a",
            "agent-a",
            &source_snapshot_id,
            serde_json::to_value(&release).unwrap(),
            Vec::new(),
            Vec::new(),
            "codex",
            ReviewAuthorityV1::McpAgent,
        )
        .unwrap();
    let release_id = store
        .activate_agent_context_release(
            &draft.draft_id,
            &release,
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let rules = perseval_store::ContextBindingRuleSetV1 {
        project_id: "project-a".into(),
        selectors: vec![perseval_store::ContextBindingSelectorV1 {
            selector_id: "agent-build".into(),
            agent_id: Some("agent-a".into()),
            build_id: Some("build-1".into()),
            environment: Some("test".into()),
            context_release_id: release_id.clone(),
        }],
        reviewed_default_context_release_id: None,
    };
    let rule_id = store
        .activate_context_binding_rules(&rules, "human-reviewer", ReviewAuthorityV1::Human)
        .unwrap();
    store
        .bind_finalized_trace_context("project-a", "trace-a", 1, &rule_id, None)
        .unwrap();
    (release_id, rule_id)
}

#[test]
fn migration_18_and_zero_finding_assessment_round_trip() {
    let (directory, store) = setup();
    let control =
        rusqlite::Connection::open(WorkspaceStoreLayout::new(directory.path()).control_database())
            .unwrap();
    assert_eq!(
        control
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 18",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    for table in [
        "agent_context_releases",
        "taxonomy_releases",
        "evaluator_releases",
        "assessment_jobs",
        "assessment_attempts",
        "assessments",
        "assessment_cache_entries",
    ] {
        assert!(
            control
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap(),
            "migration 18 table {table} is missing"
        );
    }
    drop(control);

    activate_context(&store);
    let evaluator_id = store
        .activate_evaluator_release(
            "project-a",
            &evaluator("task completion foundation", 'a'),
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    store
        .set_project_assessment_policy(
            &ProjectAssessmentPolicyV1 {
                schema_version: PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION.into(),
                project_id: "project-a".into(),
                provider_enabled: false,
                daily_budget_micros: 0,
                per_attempt_budget_micros: 0,
                lease_duration_ms: 20,
                maximum_attempts: 3,
                updated_by: "human-reviewer".into(),
                updated_at_unix_ms: 1,
            },
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let job = store
        .enqueue_assessment_job(
            "project-a",
            &evaluator_id,
            &[("trace-a".into(), 1)],
            "first-run",
        )
        .unwrap();
    let claim = store.claim_next_assessment("worker-a", 0).unwrap().unwrap();
    assert!(claim.context_release_id.is_some());
    let commit = FoundationAssessmentExecutor.execute(&claim);
    let record = store
        .commit_assessment_attempt(&claim, &commit)
        .unwrap()
        .unwrap();
    assert_eq!(record.logical_trace_id, "trace-a");
    assert_eq!(record.revision, 1);
    assert_eq!(record.context_binding_id, claim.context_binding_id);
    assert_eq!(
        record.evaluation.unwrap().abstention_reason,
        Some(traces_to_evals::LearnedAbstentionReasonV1::ProviderUnavailable)
    );
    let export = store.export_assessment_job(&job.job_id).unwrap();
    assert_eq!(export.job.selection_hash, job.selection_hash);
    assert_eq!(export.items.len(), 1);
    assert_eq!(export.status_counts.get("provider_unavailable"), Some(&1));
    assert_eq!(export.items[0].logical_trace_id, "trace-a");
    assert_eq!(export.items[0].revision, 1);
    assert!(export.items[0].assessment.is_some());
}

#[test]
fn leases_cache_budget_and_human_activation_boundaries_are_durable() {
    let (_directory, store) = setup();
    activate_context(&store);
    let release = context_release(&digest('f'), "Changed purpose");
    assert!(
        store
            .activate_agent_context_release(
                "missing-draft",
                &release,
                "codex",
                ReviewAuthorityV1::McpAgent,
            )
            .is_err()
    );

    let evaluator_id = store
        .activate_evaluator_release(
            "project-a",
            &evaluator("lease evaluator", '1'),
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    store
        .set_project_assessment_policy(
            &ProjectAssessmentPolicyV1 {
                schema_version: PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION.into(),
                project_id: "project-a".into(),
                provider_enabled: true,
                daily_budget_micros: 50,
                per_attempt_budget_micros: 50,
                lease_duration_ms: 5,
                maximum_attempts: 3,
                updated_by: "human-reviewer".into(),
                updated_at_unix_ms: 1,
            },
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    store
        .enqueue_assessment_job(
            "project-a",
            &evaluator_id,
            &[("trace-a".into(), 1)],
            "lease-run",
        )
        .unwrap();
    let first = store.claim_next_assessment("worker-a", 0).unwrap().unwrap();
    assert!(
        store
            .claim_next_assessment("worker-b", 0)
            .unwrap()
            .is_none()
    );
    thread::sleep(Duration::from_millis(8));
    let recovered = store.claim_next_assessment("worker-b", 0).unwrap().unwrap();
    assert_eq!(recovered.item_id, first.item_id);
    assert_eq!(recovered.attempt_number, 2);
    let commit = FoundationAssessmentExecutor.execute(&recovered);
    store
        .commit_assessment_attempt(&recovered, &commit)
        .unwrap();

    // Same exact evaluator/context/projection is fulfilled from cache without a lease.
    store
        .enqueue_assessment_job(
            "project-a",
            &evaluator_id,
            &[("trace-a".into(), 1)],
            "cached-run",
        )
        .unwrap();
    assert!(
        store
            .claim_next_assessment("worker-c", 0)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .list_trace_assessments("project-a", "trace-a", 1)
            .unwrap()
            .len(),
        2
    );

    let budget_evaluator_id = store
        .activate_evaluator_release(
            "project-a",
            &evaluator("budget evaluator", '2'),
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let budget_job = store
        .enqueue_assessment_job(
            "project-a",
            &budget_evaluator_id,
            &[("trace-a".into(), 1)],
            "budget-run",
        )
        .unwrap();
    let blocked = store
        .claim_next_assessment("worker-d", 100)
        .unwrap()
        .unwrap();
    assert_eq!(
        blocked.preflight_status,
        Some(perseval_store::AssessmentItemStatusV1::BudgetBlocked)
    );
    let commit = FoundationAssessmentExecutor.execute(&blocked);
    store.commit_assessment_attempt(&blocked, &commit).unwrap();
    let health = store.assessment_runtime_health().unwrap();
    assert_eq!(health.budget_blocked, 1);
    let project_health = store
        .assessment_runtime_health_for_project(Some("project-a"))
        .unwrap();
    assert_eq!(project_health.budget_blocked, 1);
    store
        .create_project(&CreateProjectV1 {
            project_id: "project-b".into(),
            display_name: "Project B".into(),
            artifact_namespace: "project-b".into(),
        })
        .unwrap();
    assert_eq!(
        store
            .assessment_runtime_health_for_project(Some("project-b"))
            .unwrap()
            .terminal,
        0
    );
    let export = store.export_assessment_job(&budget_job.job_id).unwrap();
    assert_eq!(export.status_counts.get("budget_blocked"), Some(&1));
    assert_eq!(export.total_cost_micros, 0);
}

#[test]
fn cross_project_selection_is_rejected_before_projection() {
    let (_directory, store) = setup();
    let evaluator_id = store
        .activate_evaluator_release(
            "project-a",
            &evaluator("cross project", '3'),
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let error = store
        .enqueue_assessment_job(
            "other-project",
            &evaluator_id,
            &[("trace-a".into(), 1)],
            "cross-project",
        )
        .unwrap_err();
    assert!(error.to_string().contains("cross-project"));
}

#[test]
fn repository_source_change_invalidates_prepared_context_draft() {
    let (_directory, store) = setup();
    let first_snapshot = store
        .record_context_source_snapshot(
            "project-a",
            "repository",
            "README.md",
            &digest('4'),
            "public",
            &json!({"commit": "first"}),
        )
        .unwrap();
    let release = context_release(&first_snapshot, "Original purpose");
    let draft = store
        .create_agent_context_draft(
            "project-a",
            "agent-a",
            &first_snapshot,
            serde_json::to_value(&release).unwrap(),
            Vec::new(),
            Vec::new(),
            "codex",
            ReviewAuthorityV1::McpAgent,
        )
        .unwrap();
    store
        .record_context_source_snapshot(
            "project-a",
            "repository",
            "README.md",
            &digest('5'),
            "public",
            &json!({"commit": "second"}),
        )
        .unwrap();

    let error = store
        .activate_agent_context_release(
            &draft.draft_id,
            &release,
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap_err();
    assert!(error.to_string().contains("source changed"));
}

#[test]
fn approved_repository_prepares_a_sourced_draft_for_human_activation() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("agent-repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::write(
        repository.join("README.md"),
        "# Checkout Agent\n\nHelps customers inspect orders and safely request a return.\n\n## Tasks\n\n- Inspect an order for a customer\n- Safely request an eligible return\n\n## Capabilities\n\n- Look up an order by its public reference\n- Request human approval before refunding payment\n\n## Success criteria\n\n- Cite the inspected order before proposing a return\n\n## Non-goals\n\n- Never approve a refund without a human\n",
    )
    .unwrap();
    std::fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"checkout-agent\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(repository.join("expected-answer-fixture.md"), "gold label").unwrap();

    let mut config = PersevalConfigV1 {
        workspace_dir: directory.path().join("workspace"),
        workspace_id: "repository-draft-test".into(),
        ..Default::default()
    };
    config.otlp.enabled = false;
    let service = LiveTraceService::start(config).unwrap();
    service
        .create_project(CreateProjectV1 {
            project_id: "checkout".into(),
            display_name: "Checkout".into(),
            artifact_namespace: "checkout".into(),
        })
        .unwrap();
    let draft = service
        .prepare_agent_context_from_repository("checkout", &repository, "qa-reviewer")
        .unwrap();
    assert_eq!(
        draft
            .proposed_context
            .pointer("/identity/application_name/value")
            .and_then(serde_json::Value::as_str),
        Some("checkout-agent")
    );
    assert_eq!(
        draft
            .proposed_context
            .pointer("/intent/supported_tasks")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        draft
            .source_manifest
            .pointer("/files")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2),
        "held-out-looking fixture files must not enter the source snapshot"
    );
    let release_id = service
        .approve_agent_context_draft(&draft.draft_id, "qa-reviewer", ReviewAuthorityV1::Human)
        .unwrap();
    let governance = service
        .agent_context_governance_summary("checkout")
        .unwrap();
    assert_eq!(governance.active_release_count, 1);
    assert_eq!(
        governance.latest_context_release_id.as_deref(),
        Some(release_id.as_str())
    );
    let activated = governance
        .latest_context_release
        .as_ref()
        .expect("the current immutable specification must remain reviewable");
    assert_eq!(activated.intent.supported_tasks.len(), 2);
    assert_eq!(activated.capabilities.len(), 2);
    assert_eq!(activated.intent.success_criteria.len(), 1);
    assert_eq!(activated.intent.explicit_non_goals.len(), 1);
    assert_eq!(governance.drafts_in_review, 0);

    let taxonomy_draft_id = service
        .prepare_taxonomy_from_agent_context("checkout", "codex")
        .unwrap();
    let taxonomy = service.taxonomy_governance_summary("checkout").unwrap();
    assert_eq!(taxonomy.drafts_in_review, 1);
    let draft = taxonomy.latest_draft.as_ref().unwrap();
    assert_eq!(draft.draft_id, taxonomy_draft_id);
    assert_eq!(draft.proposal.nodes.len(), 6);
    assert!(
        service
            .approve_taxonomy_change_draft(
                &taxonomy_draft_id,
                "codex",
                ReviewAuthorityV1::McpAgent,
            )
            .is_err(),
        "an MCP agent must not activate issue definitions"
    );
    let taxonomy_release_id = service
        .approve_taxonomy_change_draft(&taxonomy_draft_id, "qa-reviewer", ReviewAuthorityV1::Human)
        .unwrap();
    let taxonomy = service.taxonomy_governance_summary("checkout").unwrap();
    assert_eq!(taxonomy.active_release_count, 1);
    assert_eq!(taxonomy.active_node_count, 6);
    assert_eq!(
        taxonomy.latest_release_id.as_deref(),
        Some(taxonomy_release_id.as_str())
    );

    let plain_repository = directory.path().join("plain-repository");
    std::fs::create_dir(&plain_repository).unwrap();
    std::fs::write(
        plain_repository.join("README.md"),
        "# Plain Agent\n\nAnswers customer questions using approved local documentation and cites the relevant source.",
    )
    .unwrap();
    service
        .create_project(CreateProjectV1 {
            project_id: "plain".into(),
            display_name: "Plain".into(),
            artifact_namespace: "plain".into(),
        })
        .unwrap();
    let fallback = service
        .prepare_agent_context_from_repository("plain", &plain_repository, "qa-reviewer")
        .unwrap();
    assert_eq!(
        fallback
            .proposed_context
            .pointer("/intent/supported_tasks")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1),
        "a sourced purpose must yield a reviewable fallback task when headings are absent"
    );
    service.shutdown();
}

#[test]
fn reviewed_default_backfill_is_exact_stale_safe_and_human_only() {
    let (_directory, store) = setup();
    let source_snapshot_id = store
        .record_context_source_snapshot(
            "project-a",
            "repository",
            "README.md",
            &digest('7'),
            "public",
            &json!({"files": [{"path": "README.md", "hash": digest('7')}]}),
        )
        .unwrap();
    let release = context_release(&source_snapshot_id, "Reviewed default test");
    let draft = store
        .create_agent_context_draft(
            "project-a",
            "agent-a",
            &source_snapshot_id,
            serde_json::to_value(&release).unwrap(),
            Vec::new(),
            Vec::new(),
            "codex",
            ReviewAuthorityV1::McpAgent,
        )
        .unwrap();
    let release_id = store
        .activate_agent_context_release(
            &draft.draft_id,
            &release,
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let first_preview = store
        .preview_context_backfill("project-a", &release_id)
        .unwrap();
    assert_eq!(
        first_preview.affected_revisions,
        vec![("trace-a".into(), 1)]
    );
    assert_eq!(first_preview.unresolved_revisions.len(), 1);
    assert!(
        store
            .apply_reviewed_default_context_backfill(
                "project-a",
                &release_id,
                &first_preview.selection_hash,
                "codex",
                ReviewAuthorityV1::McpAgent,
            )
            .is_err()
    );

    let mut second = span();
    second.external_trace_id = "trace-b".into();
    second.logical_trace_id = "trace-b".into();
    second.external_span_id = "span-b".into();
    second.start_time_unix_nano = 30;
    second.end_time_unix_nano = 40;
    second.observed_at_unix_nano = 40;
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        received_at_unix_ms: 2,
        spans: vec![second],
        rejected_spans: 0,
        rejection_message: None,
    };
    let receipt = store
        .journal_batch(&mut batch, b"assessment-runtime-b", "test", 4096)
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();
    store.advance_lifecycle(i64::MAX / 4, 0, 0).unwrap();
    store.advance_lifecycle(i64::MAX / 4, 0, 0).unwrap();

    let stale = store
        .apply_reviewed_default_context_backfill(
            "project-a",
            &release_id,
            &first_preview.selection_hash,
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap_err();
    assert!(stale.to_string().contains("stale"));
    assert_eq!(
        store
            .agent_context_governance_summary("project-a")
            .unwrap()
            .resolved_bindings,
        0
    );

    let current = store
        .preview_context_backfill("project-a", &release_id)
        .unwrap();
    assert_eq!(current.affected_revisions.len(), 2);
    let applied = store
        .apply_reviewed_default_context_backfill(
            "project-a",
            &release_id,
            &current.selection_hash,
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    assert_eq!(applied.bound_revisions, current.affected_revisions);
    assert_eq!(
        store
            .agent_context_governance_summary("project-a")
            .unwrap()
            .resolved_bindings,
        2
    );
}

/// Replays the authoritative leakage-safe clean-v4 workspace used in the Arize
/// head-to-head. The caller must point this at a disposable copy because opening
/// it applies the latest workspace migration and writes assessment artifacts.
#[test]
#[ignore = "requires a disposable copy of the private clean-v4 benchmark workspace"]
fn clean_v4_640_trace_accounting_certification() {
    let workspace = std::env::var("PERSEVAL_CLEAN_V4_CERT_WORKSPACE")
        .expect("PERSEVAL_CLEAN_V4_CERT_WORKSPACE must point at a disposable workspace copy");
    let layout = WorkspaceStoreLayout::new(&workspace);
    let store = WorkspaceStore::open(&layout, "default").unwrap();
    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let exact_revisions = control
        .prepare(
            "SELECT t.logical_trace_id, r.revision
             FROM logical_traces t JOIN trace_revisions r
               ON r.logical_trace_id = t.logical_trace_id
             WHERE t.workspace_id = 'default'
               AND t.project_id = 'arize-perseval-hf-benchmark'
               AND r.lifecycle = 'finalized'
             ORDER BY t.logical_trace_id, r.revision",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(exact_revisions.len(), 640);
    let runs = store.list_runs(0, 700).unwrap();
    assert_eq!(runs.len(), 640);
    assert!(runs.iter().all(|run| run.finding_count == 0));

    let evaluator_id = store
        .activate_evaluator_release(
            "arize-perseval-hf-benchmark",
            &evaluator("clean-v4 foundation accounting", '9'),
            "human-reviewer",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let job = store
        .enqueue_assessment_job(
            "arize-perseval-hf-benchmark",
            &evaluator_id,
            &exact_revisions,
            "clean-v4-pv01-certification",
        )
        .unwrap();
    while let Some(claim) = store.claim_next_assessment("cert-worker", 0).unwrap() {
        let commit = FoundationAssessmentExecutor.execute(&claim);
        store.commit_assessment_attempt(&claim, &commit).unwrap();
    }
    let export = store.export_assessment_job(&job.job_id).unwrap();
    assert_eq!(export.job.item_count, 640);
    assert_eq!(export.job.terminal_count, 640);
    assert_eq!(export.items.len(), 640);
    assert_eq!(export.status_counts.get("abstained"), Some(&640));
    assert_eq!(export.total_cost_micros, 0);
    assert!(export.items.iter().all(|item| {
        item.assessment
            .as_ref()
            .and_then(|record| record.evaluation.as_ref())
            .and_then(|evaluation| evaluation.abstention_reason)
            == Some(traces_to_evals::LearnedAbstentionReasonV1::ContextUnresolved)
    }));
    let health = store
        .assessment_runtime_health_for_project(Some("arize-perseval-hf-benchmark"))
        .unwrap();
    assert_eq!(health.abstained, 640);
    assert_eq!(health.context_unresolved, 640);
}

/// Materializes one transparent, offline reference result in a disposable
/// workspace so Computer Use can certify the successful-review UI without a
/// credential or network call. This does not install a production evaluator;
/// PV-02 supplies the first task-specific executable judge.
#[test]
#[ignore = "writes one offline reference assessment into a disposable QA workspace"]
fn successful_review_ui_reference_fixture() {
    const PROJECT_ID: &str = "arize-perseval-hf-benchmark";
    let workspace = std::env::var("PERSEVAL_PV01_SUCCESS_QA_WORKSPACE")
        .expect("PERSEVAL_PV01_SUCCESS_QA_WORKSPACE must point at a disposable workspace copy");
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(&workspace), "default").unwrap();
    let run = store
        .list_runs(0, 1)
        .unwrap()
        .into_iter()
        .next()
        .expect("QA workspace must contain a finalized trace");
    assert_eq!(run.project_id, PROJECT_ID);
    let evidence_span_id = store
        .span_tree_page(&run.logical_trace_id, run.revision, None, 0, 1)
        .unwrap()
        .rows
        .into_iter()
        .next()
        .expect("QA trace must contain a root span")
        .span_id;

    let source_snapshot_id = store
        .record_context_source_snapshot(
            PROJECT_ID,
            "qa_fixture",
            "offline-reference-context",
            &digest('7'),
            "public",
            &json!({"purpose": "Computer Use review certification", "network_calls": 0}),
        )
        .unwrap();
    let release = context_release(
        &source_snapshot_id,
        "Verify that a persisted learned-review result is inspectable.",
    );
    let draft = store
        .create_agent_context_draft(
            PROJECT_ID,
            &release.agent_id,
            &source_snapshot_id,
            serde_json::to_value(&release).unwrap(),
            Vec::new(),
            Vec::new(),
            "qa-fixture",
            ReviewAuthorityV1::Importer,
        )
        .unwrap();
    let context_release_id = store
        .activate_agent_context_release(
            &draft.draft_id,
            &release,
            "human-qa",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let preview = store
        .preview_context_backfill(PROJECT_ID, &context_release_id)
        .unwrap();
    store
        .apply_reviewed_default_context_backfill(
            PROJECT_ID,
            &context_release_id,
            &preview.selection_hash,
            "human-qa",
            ReviewAuthorityV1::Human,
        )
        .unwrap();

    store
        .set_project_assessment_policy(
            &ProjectAssessmentPolicyV1 {
                schema_version: PROJECT_ASSESSMENT_POLICY_SCHEMA_VERSION.into(),
                project_id: PROJECT_ID.into(),
                provider_enabled: true,
                daily_budget_micros: 10_000,
                per_attempt_budget_micros: 2_000,
                lease_duration_ms: 30_000,
                maximum_attempts: 1,
                updated_by: "human-qa".into(),
                updated_at_unix_ms: 1,
            },
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let evaluator_release_id = store
        .activate_evaluator_release(
            PROJECT_ID,
            &evaluator("PV-01 offline UI reference", '8'),
            "human-qa",
            ReviewAuthorityV1::Human,
        )
        .unwrap();
    let job = store
        .enqueue_assessment_job(
            PROJECT_ID,
            &evaluator_release_id,
            &[(run.logical_trace_id.clone(), run.revision)],
            "pv01-offline-ui-reference",
        )
        .unwrap();
    let claim = store
        .claim_next_assessment("qa-fixture-worker", 2_000)
        .unwrap()
        .expect("reference assessment must be claimable");
    assert_eq!(claim.job_id, job.job_id);
    assert_eq!(claim.preflight_status, None);
    assert_eq!(claim.reserved_cost_micros, 2_000);

    let evidence_key = "root-span".to_string();
    let location = EvaluationEvidenceLocationV1::Span {
        span_id: evidence_span_id,
    };
    let catalog = EvaluationEvidenceCatalogV1 {
        target_key: claim.logical_trace_id.clone(),
        target_revision: claim.revision.to_string(),
        projection_hash: claim.projection_hash.clone(),
        entries: BTreeMap::from([(
            evidence_key.clone(),
            EvaluationEvidenceRecordV1 {
                target_key: claim.logical_trace_id.clone(),
                target_revision: claim.revision.to_string(),
                projection_hash: claim.projection_hash.clone(),
                evidence_kind: EvaluationEvidenceKindV1::Span,
                location: location.clone(),
                applicable_criterion_ids: BTreeSet::new(),
            },
        )]),
    };
    let evaluation = LearnedEvaluationV1 {
        schema_version: LEARNED_EVALUATION_SCHEMA_VERSION.into(),
        evaluator_release_id: claim.evaluator_release_id.clone(),
        target_key: claim.logical_trace_id.clone(),
        target_revision: claim.revision.to_string(),
        trace_context_binding_id: claim.context_binding_id.clone(),
        projection_hash: claim.projection_hash.clone(),
        verdict: LearnedVerdictV1::Pass,
        label: Some("task_completed".into()),
        score: Some(0.94),
        model_reported_confidence: Some(0.88),
        explanation: "Offline reference fixture found the expected completed agent span. This result exists only to certify persistence and review UI behavior.".into(),
        evidence: vec![EvaluationEvidenceCitationV1 {
            evidence_key,
            evidence_kind: EvaluationEvidenceKindV1::Span,
            location,
            criterion_id: None,
        }],
        criteria: Vec::new(),
        abstention_reason: None,
    };
    let response = ProviderResponseEnvelopeV1 {
        provider: Some("offline-reference-fixture".into()),
        requested_model: "tte-reference-model-v1".into(),
        returned_model: Some("tte-reference-model-v1".into()),
        response_id: Some("offline-reference-result".into()),
        finish_reason: Some("fixture_complete".into()),
        system_fingerprint: None,
        service_tier: Some("local-test".into()),
        usage: None,
        request_hash: digest('4'),
        response_hash: digest('5'),
        attempts: 1,
        latency_ms: 17,
    };
    let record = store
        .commit_assessment_attempt(
            &claim,
            &AssessmentCommitV1 {
                status: AssessmentItemStatusV1::Succeeded,
                evaluation: Some(evaluation),
                evidence_catalog: Some(catalog),
                provider_response: Some(response),
                provider_failure: None,
                charged_cost_micros: 1_234,
                latency_ms: 17,
                retryable: false,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap()
        .expect("reference assessment must persist");
    assert_eq!(record.status, AssessmentItemStatusV1::Succeeded);
    assert_eq!(record.cost_micros, 1_234);
    assert_eq!(record.latency_ms, 17);
    assert_eq!(
        record.provider.as_deref(),
        Some("offline-reference-fixture")
    );
    assert_eq!(
        store
            .list_trace_assessments(PROJECT_ID, &run.logical_trace_id, run.revision)
            .unwrap()
            .first()
            .map(|record| record.assessment_id.as_str()),
        Some(record.assessment_id.as_str())
    );
}

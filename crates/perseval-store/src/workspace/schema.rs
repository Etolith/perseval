use super::*;

pub(super) fn migrate_control(connection: &SqliteConnection) -> Result<(), StoreError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY);",
    )?;
    for migration in CONTROL_MIGRATIONS {
        let applied = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [migration.version],
            |row| row.get::<_, bool>(0),
        )?;
        if applied {
            continue;
        }

        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO schema_migrations(version) VALUES (?1)",
            [migration.version],
        )?;
        (migration.apply)(&transaction)?;
        transaction.commit()?;
    }
    Ok(())
}

struct ControlMigration {
    version: i64,
    apply: fn(&SqliteConnection) -> Result<(), StoreError>,
}

const CONTROL_MIGRATIONS: &[ControlMigration] = &[
    // Versions 1-21 predate the incremental runner and are intentionally
    // retained as one idempotent compatibility baseline. Databases carrying
    // any earlier subset are reconciled once; databases already at v21 skip
    // the historical DDL entirely.
    ControlMigration {
        version: 21,
        apply: apply_control_v21_baseline,
    },
    ControlMigration {
        version: 22,
        apply: apply_control_v22_migration_metadata,
    },
];

fn apply_control_v21_baseline(connection: &SqliteConnection) -> Result<(), StoreError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (1);
         CREATE TABLE IF NOT EXISTS ingest_journal(
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL,
            source_id TEXT NOT NULL,
            raw_blob_hash TEXT NOT NULL,
            normalized_blob_hash TEXT NOT NULL,
            wire_encoding TEXT NOT NULL,
            received_at_unix_ms INTEGER NOT NULL,
            accepted_spans INTEGER NOT NULL,
            rejected_spans INTEGER NOT NULL,
            projected INTEGER NOT NULL DEFAULT 0,
            projected_at_unix_ms INTEGER,
            UNIQUE(workspace_id, source_id, raw_blob_hash)
         );
         CREATE TABLE IF NOT EXISTS projector_checkpoint(
            workspace_id TEXT PRIMARY KEY,
            journal_sequence INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS source_health(
            workspace_id TEXT NOT NULL,
            source_id TEXT NOT NULL,
            accepted_spans INTEGER NOT NULL DEFAULT 0,
            rejected_spans INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(workspace_id, source_id)
         );
         CREATE TABLE IF NOT EXISTS logical_traces(
            workspace_id TEXT NOT NULL,
            logical_trace_id TEXT PRIMARY KEY,
            source_id TEXT NOT NULL,
            external_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            lifecycle TEXT NOT NULL,
            title TEXT NOT NULL,
            service_name TEXT,
            environment TEXT,
            start_time_unix_nano INTEGER NOT NULL,
            end_time_unix_nano INTEGER NOT NULL,
            last_committed_unix_ms INTEGER NOT NULL,
            span_count INTEGER NOT NULL,
            error_count INTEGER NOT NULL,
            UNIQUE(workspace_id, source_id, external_trace_id)
         );
         CREATE TABLE IF NOT EXISTS trace_revisions(
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            lifecycle TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            finalized_at_unix_ms INTEGER,
            PRIMARY KEY(logical_trace_id, revision)
         );
         CREATE TABLE IF NOT EXISTS trace_delta_outbox(
            commit_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            delta_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_outbox_workspace_sequence
            ON trace_delta_outbox(workspace_id, commit_sequence);
         CREATE INDEX IF NOT EXISTS idx_traces_last_commit
            ON logical_traces(workspace_id, last_committed_unix_ms DESC);",
    )?;
    ensure_control_column(
        connection,
        "logical_traces",
        "analysis_status",
        "TEXT NOT NULL DEFAULT 'not_ready'",
    )?;
    ensure_control_column(
        connection,
        "logical_traces",
        "finding_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_control_column(
        connection,
        "logical_traces",
        "project_id",
        "TEXT NOT NULL DEFAULT 'unassigned'",
    )?;
    ensure_control_column(connection, "logical_traces", "session_id", "TEXT")?;
    ensure_control_column(connection, "logical_traces", "build_id", "TEXT")?;
    ensure_control_column(connection, "logical_traces", "agent_id", "TEXT")?;
    ensure_control_column(
        connection,
        "logical_traces",
        "identity_quality",
        "TEXT NOT NULL DEFAULT 'unknown'",
    )?;
    ensure_control_column(
        connection,
        "trace_revisions",
        "topology_status",
        "TEXT NOT NULL DEFAULT 'not_ready'",
    )?;
    ensure_control_column(
        connection,
        "trace_revisions",
        "topology_projection_version",
        "INTEGER",
    )?;
    ensure_control_column(connection, "trace_revisions", "topology_last_error", "TEXT")?;
    ensure_control_column(
        connection,
        "trace_revisions",
        "topology_updated_at_unix_ms",
        "INTEGER",
    )?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_trace_revisions_topology
            ON trace_revisions(topology_status, logical_trace_id, revision);",
    )?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS analysis_results(
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            adapter_id TEXT NOT NULL,
            adapter_version TEXT NOT NULL,
            behavior_json TEXT NOT NULL,
            findings_json TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            committed_at_unix_ms INTEGER NOT NULL,
            error TEXT,
            PRIMARY KEY(logical_trace_id, revision)
         );
         CREATE INDEX IF NOT EXISTS idx_analysis_active
            ON analysis_results(active, logical_trace_id, revision);
         CREATE TABLE IF NOT EXISTS analysis_runs(
            analysis_id TEXT PRIMARY KEY,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            identity_json TEXT NOT NULL,
            input_schema_version TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            adapter_id TEXT NOT NULL,
            adapter_version TEXT NOT NULL,
            detector_profile_id TEXT NOT NULL,
            detector_profile_version TEXT NOT NULL,
            detector_versions_json TEXT NOT NULL,
            grouping_version TEXT NOT NULL DEFAULT 'traceeval.known_signature_group.v1',
            risk_model_version TEXT NOT NULL DEFAULT 'perseval.risk_model.none.v1',
            behavior_json TEXT NOT NULL,
            detection_report_json TEXT NOT NULL,
            findings_json TEXT NOT NULL,
            committed_at_unix_ms INTEGER NOT NULL,
            error TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_analysis_runs_trace_revision
            ON analysis_runs(logical_trace_id, revision, committed_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS active_analysis_runs(
            logical_trace_id TEXT PRIMARY KEY,
            revision INTEGER NOT NULL,
            analysis_id TEXT NOT NULL,
            activated_at_unix_ms INTEGER NOT NULL,
            FOREIGN KEY(analysis_id) REFERENCES analysis_runs(analysis_id)
         );
         CREATE TABLE IF NOT EXISTS active_failure_findings(
            finding_id TEXT PRIMARY KEY,
            projection_schema_version TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v3',
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            project_id TEXT NOT NULL DEFAULT 'unassigned',
            service_name TEXT,
            environment TEXT,
            build_id TEXT,
            session_id TEXT,
            run_title TEXT NOT NULL DEFAULT '',
            run_started_at_unix_nano INTEGER NOT NULL DEFAULT 0,
            analysis_id TEXT NOT NULL,
            failure_signature TEXT NOT NULL,
            group_id TEXT NOT NULL,
            detector_id TEXT NOT NULL,
            detector_version TEXT NOT NULL,
            severity TEXT NOT NULL,
            recovery TEXT NOT NULL,
            subject TEXT,
            operation TEXT,
            created_at TEXT NOT NULL,
            finding_json TEXT NOT NULL,
            presentation_json TEXT,
            telemetry_gaps_json TEXT NOT NULL,
            adapter_id TEXT NOT NULL,
            adapter_version TEXT NOT NULL,
            FOREIGN KEY(analysis_id) REFERENCES analysis_runs(analysis_id)
         );
         CREATE INDEX IF NOT EXISTS idx_active_failure_findings_signature
            ON active_failure_findings(failure_signature, logical_trace_id);
         CREATE INDEX IF NOT EXISTS idx_active_failure_findings_filter
            ON active_failure_findings(detector_id, severity, recovery, created_at);
         CREATE INDEX IF NOT EXISTS idx_active_failure_findings_trace
            ON active_failure_findings(logical_trace_id, revision);
         CREATE TABLE IF NOT EXISTS active_failure_group_memberships(
            logical_trace_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            projection_schema_version TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v3',
            project_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            service_name TEXT,
            environment TEXT,
            build_id TEXT,
            session_id TEXT,
            run_title TEXT NOT NULL DEFAULT '',
            run_started_at_unix_nano INTEGER NOT NULL DEFAULT 0,
            analysis_id TEXT NOT NULL,
            failure_signature TEXT NOT NULL,
            subject TEXT,
            operation TEXT,
            presentation_json TEXT,
            telemetry_gaps_json TEXT NOT NULL DEFAULT '[]',
            telemetry_gap_count INTEGER NOT NULL DEFAULT 0,
            detector_ids_json TEXT NOT NULL,
            finding_ids_json TEXT NOT NULL,
            occurrence_count INTEGER NOT NULL,
            severity TEXT NOT NULL,
            recovered_count INTEGER NOT NULL,
            unrecovered_count INTEGER NOT NULL,
            unknown_recovery_count INTEGER NOT NULL,
            confirmed_count INTEGER NOT NULL DEFAULT 0,
            dismissed_count INTEGER NOT NULL DEFAULT 0,
            needs_context_count INTEGER NOT NULL DEFAULT 0,
            unreviewed_count INTEGER NOT NULL DEFAULT 0,
            stale_disposition_count INTEGER NOT NULL DEFAULT 0,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            PRIMARY KEY(logical_trace_id, group_id),
            FOREIGN KEY(analysis_id) REFERENCES analysis_runs(analysis_id)
         );
         CREATE INDEX IF NOT EXISTS idx_active_failure_group_memberships_group
            ON active_failure_group_memberships(project_id, group_id, logical_trace_id);
         CREATE INDEX IF NOT EXISTS idx_active_failure_group_memberships_rank
            ON active_failure_group_memberships(project_id, severity, unrecovered_count,
                                                 occurrence_count, last_seen_at);
         CREATE TABLE IF NOT EXISTS active_failure_group_detectors(
            logical_trace_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            detector_id TEXT NOT NULL,
            PRIMARY KEY(logical_trace_id, group_id, detector_id)
         );
         CREATE INDEX IF NOT EXISTS idx_active_failure_group_detectors_group
            ON active_failure_group_detectors(project_id, group_id, detector_id);
         CREATE TABLE IF NOT EXISTS active_failure_evidence_refs(
            finding_id TEXT NOT NULL,
            evidence_index INTEGER NOT NULL,
            analysis_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            evidence_kind TEXT NOT NULL,
            evidence_identity TEXT NOT NULL,
            span_id TEXT,
            role TEXT NOT NULL,
            explanation TEXT NOT NULL,
            PRIMARY KEY(finding_id, evidence_index)
         );
         CREATE INDEX IF NOT EXISTS idx_active_failure_evidence_identity
            ON active_failure_evidence_refs(finding_id, evidence_index, span_id);
         CREATE TABLE IF NOT EXISTS active_failure_diagnostics(
            finding_id TEXT NOT NULL,
            diagnostic_index INTEGER NOT NULL,
            analysis_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            diagnostic TEXT NOT NULL,
            PRIMARY KEY(finding_id, diagnostic_index)
         );
         CREATE INDEX IF NOT EXISTS idx_active_failure_diagnostics_finding
            ON active_failure_diagnostics(finding_id, diagnostic_index);
         CREATE TABLE IF NOT EXISTS finding_dispositions(
            workspace_id TEXT NOT NULL,
            finding_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            analysis_id TEXT NOT NULL,
            detector_id TEXT NOT NULL,
            detector_version TEXT NOT NULL,
            state TEXT NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            PRIMARY KEY(workspace_id, finding_id)
         );
         CREATE INDEX IF NOT EXISTS idx_finding_dispositions_group
            ON finding_dispositions(workspace_id, project_id, group_id, state);
         CREATE TABLE IF NOT EXISTS finding_disposition_events(
            event_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL,
            finding_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            analysis_id TEXT NOT NULL,
            detector_id TEXT NOT NULL,
            detector_version TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_finding_disposition_events_finding
            ON finding_disposition_events(workspace_id, finding_id, event_sequence);
         CREATE TABLE IF NOT EXISTS active_failure_projection_state(
            logical_trace_id TEXT PRIMARY KEY,
            revision INTEGER NOT NULL,
            analysis_id TEXT NOT NULL,
            projection_schema_version TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v2',
            projected_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS analysis_failures(
            failure_id INTEGER PRIMARY KEY AUTOINCREMENT,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            error TEXT NOT NULL,
            failed_at_unix_ms INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO analysis_runs(
            analysis_id, logical_trace_id, revision, identity_json,
            input_schema_version, projection_version, adapter_id, adapter_version,
            detector_profile_id, detector_profile_version, detector_versions_json,
            behavior_json, detection_report_json, findings_json, committed_at_unix_ms, error
         )
         SELECT 'legacy:' || logical_trace_id || ':' || revision,
                logical_trace_id, revision, '{}',
                'traceeval.legacy_trace_projection.v1', 'traceeval.legacy_trace_projection.v1',
                adapter_id, adapter_version, 'traceeval.legacy', '1', '{}',
                behavior_json, '{}', findings_json, committed_at_unix_ms, error
           FROM analysis_results WHERE error IS NULL;
         INSERT OR IGNORE INTO active_analysis_runs(
            logical_trace_id, revision, analysis_id, activated_at_unix_ms
         )
         SELECT logical_trace_id, revision,
                'legacy:' || logical_trace_id || ':' || revision, committed_at_unix_ms
           FROM analysis_results WHERE active = 1 AND error IS NULL;
         CREATE TABLE IF NOT EXISTS evidence_packets(
            packet_id TEXT PRIMARY KEY,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            finding_id TEXT NOT NULL,
            packet_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS eval_candidates(
            candidate_id TEXT PRIMARY KEY,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            finding_id TEXT NOT NULL,
            group_id TEXT NOT NULL,
            evidence_packet_id TEXT NOT NULL,
            candidate_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS eval_batch_previews(
            preview_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            selection_hash TEXT NOT NULL,
            preview_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_eval_batch_previews_project_created
            ON eval_batch_previews(project_id, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS candidate_generation_jobs(
            job_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            preview_id TEXT NOT NULL,
            selection_hash TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            status TEXT NOT NULL,
            job_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            UNIQUE(project_id, idempotency_key)
         );
         CREATE TABLE IF NOT EXISTS eval_candidate_dispositions(
            candidate_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            state TEXT NOT NULL,
            reviewer_ref TEXT NOT NULL,
            reason TEXT,
            updated_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_eval_candidate_dispositions_project
            ON eval_candidate_dispositions(project_id, state, updated_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS semantic_cluster_models(
            model_id TEXT PRIMARY KEY,
            model_json TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_semantic_cluster_models_active
            ON semantic_cluster_models(active, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS semantic_cluster_assignments(
            model_id TEXT NOT NULL,
            finding_id TEXT NOT NULL,
            cluster_id TEXT NOT NULL,
            confidence REAL NOT NULL,
            distance REAL,
            novelty INTEGER NOT NULL DEFAULT 0,
            method TEXT NOT NULL,
            PRIMARY KEY(model_id, finding_id),
            FOREIGN KEY(model_id) REFERENCES semantic_cluster_models(model_id)
         );
         CREATE INDEX IF NOT EXISTS idx_semantic_assignments_finding
            ON semantic_cluster_assignments(finding_id, model_id);
         CREATE TABLE IF NOT EXISTS projects(
            workspace_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            artifact_namespace TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            PRIMARY KEY(workspace_id, project_id),
            UNIQUE(workspace_id, artifact_namespace)
         );
         CREATE INDEX IF NOT EXISTS idx_projects_workspace_name
            ON projects(workspace_id, display_name COLLATE NOCASE);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_project
            ON logical_traces(workspace_id, project_id, last_committed_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS trace_comparisons(
            comparison_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            baseline_trace_id TEXT NOT NULL,
            baseline_revision INTEGER NOT NULL,
            candidate_trace_id TEXT NOT NULL,
            candidate_revision INTEGER NOT NULL,
            result_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_trace_comparisons_project_created
            ON trace_comparisons(project_id, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS pipeline_stage_metrics(
            workspace_id TEXT NOT NULL,
            stage TEXT NOT NULL,
            sample_count INTEGER NOT NULL DEFAULT 0,
            total_duration_nano INTEGER NOT NULL DEFAULT 0,
            max_duration_nano INTEGER NOT NULL DEFAULT 0,
            item_count INTEGER NOT NULL DEFAULT 0,
            byte_count INTEGER NOT NULL DEFAULT 0,
            rows_scanned INTEGER NOT NULL DEFAULT 0,
            rows_deserialized INTEGER NOT NULL DEFAULT 0,
            updated_at_unix_ms INTEGER NOT NULL,
            PRIMARY KEY(workspace_id, stage)
         );
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (2);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (3);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (4);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (5);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (6);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (7);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (8);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (9);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (10);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (11);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (12);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (13);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (14);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (15);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (16);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (17);",
    )?;
    ensure_control_column(
        connection,
        "analysis_runs",
        "grouping_version",
        "TEXT NOT NULL DEFAULT 'traceeval.known_signature_group.v1'",
    )?;
    ensure_control_column(
        connection,
        "semantic_cluster_models",
        "project_id",
        "TEXT NOT NULL DEFAULT 'unassigned'",
    )?;
    ensure_control_column(
        connection,
        "semantic_cluster_models",
        "analysis_definition_id",
        "TEXT NOT NULL DEFAULT 'legacy'",
    )?;
    ensure_control_column(
        connection,
        "semantic_cluster_models",
        "scope_id",
        "TEXT NOT NULL DEFAULT 'all-time-all-builds'",
    )?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_semantic_cluster_models_scope_active
            ON semantic_cluster_models(project_id, scope_id, active, created_at_unix_ms DESC);",
    )?;
    ensure_control_column(
        connection,
        "active_failure_findings",
        "projection_schema_version",
        "TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v3'",
    )?;
    ensure_control_column(
        connection,
        "active_failure_group_memberships",
        "projection_schema_version",
        "TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v3'",
    )?;
    for (column, declaration) in [
        ("project_id", "TEXT NOT NULL DEFAULT 'unassigned'"),
        ("service_name", "TEXT"),
        ("environment", "TEXT"),
        ("build_id", "TEXT"),
        ("session_id", "TEXT"),
        ("run_title", "TEXT NOT NULL DEFAULT ''"),
        ("run_started_at_unix_nano", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        ensure_control_column(connection, "active_failure_findings", column, declaration)?;
    }
    for (column, declaration) in [
        ("service_name", "TEXT"),
        ("environment", "TEXT"),
        ("build_id", "TEXT"),
        ("session_id", "TEXT"),
        ("run_title", "TEXT NOT NULL DEFAULT ''"),
        ("run_started_at_unix_nano", "INTEGER NOT NULL DEFAULT 0"),
        ("subject", "TEXT"),
        ("operation", "TEXT"),
        ("presentation_json", "TEXT"),
        ("telemetry_gaps_json", "TEXT NOT NULL DEFAULT '[]'"),
        ("telemetry_gap_count", "INTEGER NOT NULL DEFAULT 0"),
        ("confirmed_count", "INTEGER NOT NULL DEFAULT 0"),
        ("dismissed_count", "INTEGER NOT NULL DEFAULT 0"),
        ("needs_context_count", "INTEGER NOT NULL DEFAULT 0"),
        ("unreviewed_count", "INTEGER NOT NULL DEFAULT 0"),
        ("stale_disposition_count", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        ensure_control_column(
            connection,
            "active_failure_group_memberships",
            column,
            declaration,
        )?;
    }
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_active_failure_group_memberships_scope
            ON active_failure_group_memberships(project_id, service_name, environment,
                                                 build_id, session_id,
                                                 run_started_at_unix_nano, group_id);
         CREATE INDEX IF NOT EXISTS idx_active_failure_findings_group_project
            ON active_failure_findings(project_id, group_id, finding_id);",
    )?;
    ensure_control_column(
        connection,
        "active_failure_projection_state",
        "projection_schema_version",
        "TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v2'",
    )?;
    ensure_control_column(
        connection,
        "analysis_runs",
        "risk_model_version",
        "TEXT NOT NULL DEFAULT 'perseval.risk_model.none.v1'",
    )?;
    ensure_control_column(connection, "trace_comparisons", "scope_id", "TEXT")?;
    ensure_control_column(connection, "trace_comparisons", "scope_json", "TEXT")?;
    ensure_control_column(
        connection,
        "ingest_journal",
        "normalized_bytes",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Migration 18 deliberately keeps learned assessments in their own durable
    // graph. `analysis_runs` remains the compatibility store for deterministic
    // findings and is never used as an assessment eligibility gate.
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_context_source_snapshots(
            source_snapshot_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            source_locator TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            sensitivity TEXT NOT NULL,
            captured_at_unix_ms INTEGER NOT NULL,
            manifest_json TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_context_sources_project
            ON agent_context_source_snapshots(project_id, captured_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS agent_context_drafts(
            draft_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            source_snapshot_id TEXT NOT NULL,
            base_release_id TEXT,
            status TEXT NOT NULL,
            draft_json TEXT NOT NULL,
            source_manifest_json TEXT NOT NULL,
            source_snapshot_digest TEXT NOT NULL,
            created_by TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_context_drafts_project
            ON agent_context_drafts(project_id, status, updated_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS agent_context_releases(
            context_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            source_draft_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            activated_by TEXT NOT NULL,
            activated_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_context_releases_project_agent
            ON agent_context_releases(project_id, agent_id, activated_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS agent_context_field_provenance(
            context_release_id TEXT NOT NULL,
            field_id TEXT NOT NULL,
            provenance TEXT NOT NULL,
            source_snapshot_id TEXT NOT NULL,
            source_locator TEXT,
            review_state TEXT NOT NULL,
            sensitivity TEXT NOT NULL,
            confidence REAL,
            metadata_json TEXT NOT NULL,
            PRIMARY KEY(context_release_id, field_id)
         );
         CREATE TABLE IF NOT EXISTS agent_context_binding_rule_releases(
            binding_rule_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            rule_json TEXT NOT NULL,
            activated_by TEXT NOT NULL,
            activated_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS trace_context_bindings(
            binding_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            resolution TEXT NOT NULL,
            context_release_id TEXT,
            binding_rule_release_id TEXT NOT NULL,
            provenance TEXT NOT NULL,
            binding_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            UNIQUE(project_id, logical_trace_id, revision, binding_rule_release_id)
         );
         CREATE INDEX IF NOT EXISTS idx_context_bindings_target
            ON trace_context_bindings(project_id, logical_trace_id, revision, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS taxonomy_change_drafts(
            draft_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            base_release_id TEXT,
            status TEXT NOT NULL,
            draft_json TEXT NOT NULL,
            source_manifest_json TEXT NOT NULL,
            created_by TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS taxonomy_releases(
            taxonomy_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            source_draft_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            activated_by TEXT NOT NULL,
            activated_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_taxonomy_releases_project
            ON taxonomy_releases(project_id, activated_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS taxonomy_nodes(
            taxonomy_release_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            node_kind TEXT NOT NULL,
            node_json TEXT NOT NULL,
            PRIMARY KEY(taxonomy_release_id, node_id)
         );
         CREATE TABLE IF NOT EXISTS taxonomy_relations(
            taxonomy_release_id TEXT NOT NULL,
            relation_id TEXT NOT NULL,
            source_node_id TEXT NOT NULL,
            target_node_id TEXT NOT NULL,
            relation_kind TEXT NOT NULL,
            relation_json TEXT NOT NULL,
            PRIMARY KEY(taxonomy_release_id, relation_id)
         );
         CREATE TABLE IF NOT EXISTS taxonomy_assignments(
            assignment_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            taxonomy_release_id TEXT NOT NULL,
            target_key TEXT NOT NULL,
            target_revision TEXT NOT NULL,
            node_id TEXT NOT NULL,
            assignment_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_taxonomy_assignments_target
            ON taxonomy_assignments(project_id, target_key, target_revision);
         CREATE TABLE IF NOT EXISTS taxonomy_lineage(
            taxonomy_release_id TEXT NOT NULL,
            lineage_id TEXT NOT NULL,
            operation_kind TEXT NOT NULL,
            lineage_json TEXT NOT NULL,
            PRIMARY KEY(taxonomy_release_id, lineage_id)
         );
         CREATE TABLE IF NOT EXISTS evaluator_definitions(
            evaluator_definition_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            name TEXT NOT NULL,
            task_kind TEXT NOT NULL,
            created_by TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS evaluator_releases(
            evaluator_release_id TEXT PRIMARY KEY,
            evaluator_definition_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 0,
            activated_by TEXT,
            created_at_unix_ms INTEGER NOT NULL,
            activated_at_unix_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_evaluator_releases_project_active
            ON evaluator_releases(project_id, active, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS project_assessment_policies(
            project_id TEXT PRIMARY KEY,
            policy_json TEXT NOT NULL,
            provider_enabled INTEGER NOT NULL,
            daily_budget_micros INTEGER NOT NULL,
            per_attempt_budget_micros INTEGER NOT NULL,
            lease_duration_ms INTEGER NOT NULL,
            maximum_attempts INTEGER NOT NULL,
            updated_by TEXT NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS assessment_targets(
            target_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            session_snapshot_id TEXT,
            target_kind TEXT NOT NULL,
            target_key TEXT NOT NULL,
            target_revision TEXT NOT NULL,
            finalized_at_unix_ms INTEGER NOT NULL,
            UNIQUE(project_id, logical_trace_id, revision, target_kind)
         );
         CREATE TABLE IF NOT EXISTS assessment_projections(
            projection_hash TEXT PRIMARY KEY,
            target_id TEXT NOT NULL,
            projection_release_id TEXT NOT NULL,
            context_projection_release_id TEXT NOT NULL,
            projection_class TEXT NOT NULL,
            projection_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS assessment_jobs(
            job_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            selection_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            cancel_requested INTEGER NOT NULL DEFAULT 0,
            item_count INTEGER NOT NULL,
            terminal_count INTEGER NOT NULL DEFAULT 0,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            UNIQUE(project_id, idempotency_key)
         );
         CREATE INDEX IF NOT EXISTS idx_assessment_jobs_status
            ON assessment_jobs(status, created_at_unix_ms);
         CREATE TABLE IF NOT EXISTS assessment_job_items(
            item_id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            context_binding_id TEXT NOT NULL,
            context_release_id TEXT,
            projection_hash TEXT NOT NULL,
            estimated_cost_micros INTEGER NOT NULL DEFAULT 0,
            cache_key TEXT NOT NULL,
            status TEXT NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            lease_owner TEXT,
            lease_expires_at_unix_ms INTEGER,
            next_attempt_at_unix_ms INTEGER NOT NULL DEFAULT 0,
            terminal_reason TEXT,
            created_at_unix_ms INTEGER NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            UNIQUE(job_id, target_id)
         );
         CREATE INDEX IF NOT EXISTS idx_assessment_items_claim
            ON assessment_job_items(status, next_attempt_at_unix_ms, lease_expires_at_unix_ms, created_at_unix_ms);
         CREATE TABLE IF NOT EXISTS assessment_attempts(
            attempt_id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            attempt_number INTEGER NOT NULL,
            lease_owner TEXT NOT NULL,
            requested_provider TEXT,
            requested_model TEXT,
            returned_model TEXT,
            request_hash TEXT,
            response_hash TEXT,
            provider_response_id TEXT,
            status TEXT NOT NULL,
            retryable INTEGER NOT NULL DEFAULT 0,
            reserved_cost_micros INTEGER NOT NULL DEFAULT 0,
            charged_cost_micros INTEGER NOT NULL DEFAULT 0,
            latency_ms INTEGER NOT NULL DEFAULT 0,
            failure_json TEXT,
            started_at_unix_ms INTEGER NOT NULL,
            finished_at_unix_ms INTEGER,
            UNIQUE(item_id, attempt_number)
         );
         CREATE TABLE IF NOT EXISTS assessments(
            assessment_id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL UNIQUE,
            project_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            context_binding_id TEXT NOT NULL,
            context_release_id TEXT,
            projection_hash TEXT NOT NULL,
            provider TEXT,
            requested_model TEXT,
            returned_model TEXT,
            status TEXT NOT NULL,
            verdict TEXT,
            label TEXT,
            score REAL,
            confidence REAL,
            explanation TEXT,
            abstention_reason TEXT,
            evaluation_json TEXT,
            cost_micros INTEGER NOT NULL DEFAULT 0,
            latency_ms INTEGER NOT NULL DEFAULT 0,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_assessments_trace
            ON assessments(project_id, logical_trace_id, revision, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS assessment_evidence_refs(
            assessment_id TEXT NOT NULL,
            evidence_index INTEGER NOT NULL,
            evidence_key TEXT NOT NULL,
            evidence_kind TEXT NOT NULL,
            criterion_id TEXT,
            location_json TEXT NOT NULL,
            PRIMARY KEY(assessment_id, evidence_index)
         );
         CREATE TABLE IF NOT EXISTS assessment_cache_entries(
            cache_key TEXT PRIMARY KEY,
            evaluator_release_id TEXT NOT NULL,
            context_binding_id TEXT NOT NULL,
            projection_hash TEXT NOT NULL,
            provider_model_identity TEXT NOT NULL,
            assessment_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS assessment_daily_budgets(
            project_id TEXT NOT NULL,
            utc_day TEXT NOT NULL,
            reserved_micros INTEGER NOT NULL DEFAULT 0,
            charged_micros INTEGER NOT NULL DEFAULT 0,
            updated_at_unix_ms INTEGER NOT NULL,
            PRIMARY KEY(project_id, utc_day)
         );
         CREATE TABLE IF NOT EXISTS task_completion_release_configs(
            evaluator_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            context_release_id TEXT NOT NULL,
            context_projection_release_id TEXT NOT NULL,
            projection_release_id TEXT NOT NULL,
            config_json TEXT NOT NULL,
            activated_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_task_completion_configs_project
            ON task_completion_release_configs(project_id, activated_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS assessment_sampling_policies(
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            policy_json TEXT NOT NULL,
            updated_at_unix_ms INTEGER NOT NULL,
            PRIMARY KEY(project_id, evaluator_release_id)
         );
         CREATE TABLE IF NOT EXISTS annotation_schema_releases(
            annotation_schema_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_annotation_schemas_project
            ON annotation_schema_releases(project_id, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS annotation_cases(
            case_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            annotation_schema_release_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            context_binding_id TEXT NOT NULL,
            safe_projection_hash TEXT NOT NULL,
            leakage_group_id TEXT NOT NULL,
            case_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            UNIQUE(annotation_schema_release_id, target_id, safe_projection_hash)
         );
         CREATE INDEX IF NOT EXISTS idx_annotation_cases_project
            ON annotation_cases(project_id, logical_trace_id, revision);
         CREATE TABLE IF NOT EXISTS annotation_case_evidence(
            case_id TEXT NOT NULL,
            evidence_key TEXT NOT NULL,
            span_id TEXT NOT NULL,
            PRIMARY KEY(case_id, evidence_key),
            UNIQUE(case_id, span_id)
         );
         CREATE TABLE IF NOT EXISTS review_split_releases(
            split_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            annotation_schema_release_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS review_split_groups(
            split_release_id TEXT NOT NULL,
            leakage_group_id TEXT NOT NULL,
            split TEXT NOT NULL,
            PRIMARY KEY(split_release_id, leakage_group_id)
         );
         CREATE TABLE IF NOT EXISTS review_queues(
            queue_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            annotation_schema_release_id TEXT NOT NULL,
            split_release_id TEXT NOT NULL,
            mode TEXT NOT NULL,
            queue_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_review_queues_project
            ON review_queues(project_id, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS review_tasks(
            task_id TEXT PRIMARY KEY,
            queue_id TEXT NOT NULL,
            case_id TEXT NOT NULL,
            project_id TEXT NOT NULL,
            logical_trace_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            assessment_id TEXT NOT NULL,
            leakage_group_id TEXT NOT NULL,
            split TEXT NOT NULL,
            status TEXT NOT NULL,
            task_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            UNIQUE(queue_id, case_id),
            UNIQUE(case_id, assessment_id)
         );
         CREATE INDEX IF NOT EXISTS idx_review_tasks_queue
            ON review_tasks(queue_id, status, created_at_unix_ms);
         CREATE TABLE IF NOT EXISTS review_assignments(
            task_id TEXT NOT NULL,
            reviewer_id TEXT NOT NULL,
            reviewer_ordinal INTEGER NOT NULL,
            assigned_at_unix_ms INTEGER NOT NULL,
            submitted_annotation_revision_id TEXT,
            PRIMARY KEY(task_id, reviewer_id),
            UNIQUE(task_id, reviewer_ordinal)
         );
         CREATE TABLE IF NOT EXISTS annotations(
            annotation_id TEXT PRIMARY KEY,
            case_id TEXT NOT NULL,
            annotation_schema_release_id TEXT NOT NULL,
            reviewer_id TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            UNIQUE(case_id, annotation_schema_release_id, reviewer_id)
         );
         CREATE TABLE IF NOT EXISTS annotation_revisions(
            revision_id TEXT PRIMARY KEY,
            annotation_id TEXT NOT NULL,
            case_id TEXT NOT NULL,
            source_task_id TEXT NOT NULL,
            reviewer_id TEXT NOT NULL,
            annotation_revision INTEGER NOT NULL,
            supersedes_revision_id TEXT,
            label TEXT NOT NULL,
            annotation_json TEXT NOT NULL,
            submitted_at_unix_ms INTEGER NOT NULL,
            UNIQUE(annotation_id, annotation_revision)
         );
         CREATE INDEX IF NOT EXISTS idx_annotations_task
            ON annotation_revisions(source_task_id, reviewer_id, annotation_revision DESC);
         CREATE TABLE IF NOT EXISTS adjudication_revisions(
            revision_id TEXT PRIMARY KEY,
            adjudication_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            adjudication_revision INTEGER NOT NULL,
            supersedes_revision_id TEXT,
            adjudication_json TEXT NOT NULL,
            adjudicated_at_unix_ms INTEGER NOT NULL,
            UNIQUE(adjudication_id, adjudication_revision)
         );
         CREATE TABLE IF NOT EXISTS adjudication_inputs(
            adjudication_revision_id TEXT NOT NULL,
            annotation_revision_id TEXT NOT NULL,
            PRIMARY KEY(adjudication_revision_id, annotation_revision_id)
         );
         CREATE TABLE IF NOT EXISTS calibration_releases(
            calibration_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            annotation_schema_release_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_calibration_releases_project
            ON calibration_releases(project_id, evaluator_release_id, created_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS calibration_release_members(
            calibration_release_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            assessment_id TEXT NOT NULL,
            leakage_group_id TEXT NOT NULL,
            split TEXT NOT NULL,
            member_role TEXT NOT NULL,
            member_json TEXT NOT NULL,
            PRIMARY KEY(calibration_release_id, task_id)
         );
         CREATE TABLE IF NOT EXISTS calibration_reports(
            report_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            calibration_release_id TEXT NOT NULL,
            threshold_policy_release_id TEXT NOT NULL,
            split_release_id TEXT NOT NULL,
            split TEXT NOT NULL,
            report_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_calibration_report_once
            ON calibration_reports(calibration_release_id, split);
         CREATE TABLE IF NOT EXISTS threshold_policy_releases(
            threshold_policy_release_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            calibration_release_id TEXT NOT NULL,
            release_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_threshold_policies_project
            ON threshold_policy_releases(project_id, evaluator_release_id, created_at_unix_ms DESC);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_threshold_policy_calibration_once
            ON threshold_policy_releases(calibration_release_id);
         CREATE TABLE IF NOT EXISTS threshold_policy_activations(
            activation_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            evaluator_release_id TEXT NOT NULL,
            threshold_policy_release_id TEXT NOT NULL,
            activation_json TEXT NOT NULL,
            activated_at_unix_ms INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_threshold_activations_project
            ON threshold_policy_activations(project_id, evaluator_release_id, activated_at_unix_ms DESC);
         CREATE TABLE IF NOT EXISTS assessment_decisions(
            decision_id TEXT PRIMARY KEY,
            assessment_id TEXT NOT NULL,
            calibration_release_id TEXT NOT NULL,
            threshold_policy_release_id TEXT NOT NULL,
            decision_json TEXT NOT NULL,
            created_at_unix_ms INTEGER NOT NULL,
            UNIQUE(assessment_id, calibration_release_id, threshold_policy_release_id)
         );
         CREATE INDEX IF NOT EXISTS idx_assessment_decisions_assessment
            ON assessment_decisions(assessment_id, created_at_unix_ms DESC);
         CREATE TRIGGER IF NOT EXISTS immutable_annotation_schema_release_update
            BEFORE UPDATE ON annotation_schema_releases BEGIN
               SELECT RAISE(ABORT, 'annotation schema releases are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_annotation_schema_release_delete
            BEFORE DELETE ON annotation_schema_releases BEGIN
               SELECT RAISE(ABORT, 'annotation schema releases are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_annotation_revision_update
            BEFORE UPDATE ON annotation_revisions BEGIN
               SELECT RAISE(ABORT, 'annotation revisions are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_annotation_revision_delete
            BEFORE DELETE ON annotation_revisions BEGIN
               SELECT RAISE(ABORT, 'annotation revisions are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_annotation_case_evidence_update
            BEFORE UPDATE ON annotation_case_evidence BEGIN
               SELECT RAISE(ABORT, 'annotation case evidence is immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_annotation_case_evidence_delete
            BEFORE DELETE ON annotation_case_evidence BEGIN
               SELECT RAISE(ABORT, 'annotation case evidence is immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_adjudication_revision_update
            BEFORE UPDATE ON adjudication_revisions BEGIN
               SELECT RAISE(ABORT, 'adjudication revisions are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_adjudication_revision_delete
            BEFORE DELETE ON adjudication_revisions BEGIN
               SELECT RAISE(ABORT, 'adjudication revisions are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_calibration_release_update
            BEFORE UPDATE ON calibration_releases BEGIN
               SELECT RAISE(ABORT, 'calibration releases are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_calibration_release_delete
            BEFORE DELETE ON calibration_releases BEGIN
               SELECT RAISE(ABORT, 'calibration releases are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_calibration_report_update
            BEFORE UPDATE ON calibration_reports BEGIN
               SELECT RAISE(ABORT, 'calibration reports are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_calibration_report_delete
            BEFORE DELETE ON calibration_reports BEGIN
               SELECT RAISE(ABORT, 'calibration reports are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_threshold_policy_release_update
            BEFORE UPDATE ON threshold_policy_releases BEGIN
               SELECT RAISE(ABORT, 'threshold policy releases are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_threshold_policy_release_delete
            BEFORE DELETE ON threshold_policy_releases BEGIN
               SELECT RAISE(ABORT, 'threshold policy releases are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_threshold_policy_activation_update
            BEFORE UPDATE ON threshold_policy_activations BEGIN
               SELECT RAISE(ABORT, 'threshold policy activations are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_threshold_policy_activation_delete
            BEFORE DELETE ON threshold_policy_activations BEGIN
               SELECT RAISE(ABORT, 'threshold policy activations are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_assessment_decision_update
            BEFORE UPDATE ON assessment_decisions BEGIN
               SELECT RAISE(ABORT, 'assessment decisions are immutable');
            END;
         CREATE TRIGGER IF NOT EXISTS immutable_assessment_decision_delete
            BEFORE DELETE ON assessment_decisions BEGIN
               SELECT RAISE(ABORT, 'assessment decisions are immutable');
            END;
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (18);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (19);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (20);",
    )?;
    ensure_control_column(
        connection,
        "assessment_job_items",
        "estimated_cost_micros",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_traces_workspace_started_desc
            ON logical_traces(workspace_id, start_time_unix_nano DESC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_started_asc
            ON logical_traces(workspace_id, start_time_unix_nano ASC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_spans
            ON logical_traces(workspace_id, span_count DESC, start_time_unix_nano DESC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_findings
            ON logical_traces(workspace_id, finding_count DESC, start_time_unix_nano DESC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_project_started_desc
            ON logical_traces(workspace_id, project_id, start_time_unix_nano DESC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_project_started_asc
            ON logical_traces(workspace_id, project_id, start_time_unix_nano ASC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_project_spans
            ON logical_traces(workspace_id, project_id, span_count DESC, start_time_unix_nano DESC, logical_trace_id ASC);
         CREATE INDEX IF NOT EXISTS idx_traces_workspace_project_findings
            ON logical_traces(workspace_id, project_id, finding_count DESC, start_time_unix_nano DESC, logical_trace_id ASC);
         INSERT OR IGNORE INTO schema_migrations(version) VALUES (21);",
    )?;
    Ok(())
}

fn apply_control_v22_migration_metadata(connection: &SqliteConnection) -> Result<(), StoreError> {
    ensure_control_column(
        connection,
        "schema_migrations",
        "name",
        "TEXT NOT NULL DEFAULT 'legacy-baseline'",
    )?;
    ensure_control_column(
        connection,
        "schema_migrations",
        "applied_at_unix_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    connection.execute(
        "UPDATE schema_migrations
            SET name = 'incremental-migration-metadata-v22',
                applied_at_unix_ms = ?1
          WHERE version = 22",
        [now_unix_ms()],
    )?;
    Ok(())
}

fn ensure_control_column(
    connection: &SqliteConnection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), StoreError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|name| name == column) {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
    }
    Ok(())
}

pub(super) fn migrate_analytics(connection: &DuckConnection) -> Result<(), StoreError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS spans(
            logical_trace_id VARCHAR NOT NULL,
            revision BIGINT NOT NULL,
            span_id VARCHAR NOT NULL,
            span_version BIGINT NOT NULL,
            is_current BOOLEAN NOT NULL,
            parent_span_id VARCHAR,
            name VARCHAR NOT NULL,
            category VARCHAR NOT NULL,
            start_time_unix_nano BIGINT NOT NULL,
            end_time_unix_nano BIGINT NOT NULL,
            duration_nano BIGINT NOT NULL,
            status_code INTEGER NOT NULL,
            status_message VARCHAR NOT NULL,
            content_hash VARCHAR NOT NULL,
            attributes_json VARCHAR NOT NULL,
            payload_refs_json VARCHAR NOT NULL,
            resource_json VARCHAR NOT NULL,
            scope_json VARCHAR NOT NULL,
            PRIMARY KEY(logical_trace_id, revision, span_id, span_version)
         );
         CREATE TABLE IF NOT EXISTS span_events(
            logical_trace_id VARCHAR, revision BIGINT, span_id VARCHAR,
            span_version BIGINT, event_index BIGINT, name VARCHAR,
            timestamp_unix_nano BIGINT, attributes_json VARCHAR
         );
         CREATE TABLE IF NOT EXISTS span_links(
            logical_trace_id VARCHAR, revision BIGINT, span_id VARCHAR,
            span_version BIGINT, link_index BIGINT, linked_trace_id VARCHAR,
            linked_span_id VARCHAR, attributes_json VARCHAR
         );
         CREATE TABLE IF NOT EXISTS payload_blobs(
            sha256 VARCHAR PRIMARY KEY,
            original_bytes BIGINT NOT NULL,
            compressed BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS projected_journal_sequences(
            workspace_id VARCHAR NOT NULL,
            journal_sequence BIGINT NOT NULL,
            projected_at_unix_ms BIGINT NOT NULL,
            PRIMARY KEY(workspace_id, journal_sequence)
         );
         CREATE INDEX IF NOT EXISTS idx_spans_current_identity
            ON spans(logical_trace_id, revision, span_id, is_current);",
    )?;
    connection.execute_batch(
        "ALTER TABLE spans ADD COLUMN IF NOT EXISTS topology_order BIGINT;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS topology_depth BIGINT;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS topology_has_children BOOLEAN;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS topology_projection_version BIGINT;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS payload_identities_json VARCHAR;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS source_id VARCHAR;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS decoder_version VARCHAR;
         ALTER TABLE spans ADD COLUMN IF NOT EXISTS semantic_mapping_version VARCHAR;
         ALTER TABLE span_events ADD COLUMN IF NOT EXISTS evidence_identity VARCHAR;
         ALTER TABLE span_events ADD COLUMN IF NOT EXISTS dropped_attributes_count BIGINT;
         ALTER TABLE span_links ADD COLUMN IF NOT EXISTS evidence_identity VARCHAR;
         ALTER TABLE span_links ADD COLUMN IF NOT EXISTS trace_state VARCHAR;
         ALTER TABLE span_links ADD COLUMN IF NOT EXISTS dropped_attributes_count BIGINT;
         ALTER TABLE span_links ADD COLUMN IF NOT EXISTS flags BIGINT;",
    )?;
    Ok(())
}

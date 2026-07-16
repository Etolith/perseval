use super::*;

pub(super) fn migrate_control(connection: &SqliteConnection) -> Result<(), StoreError> {
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

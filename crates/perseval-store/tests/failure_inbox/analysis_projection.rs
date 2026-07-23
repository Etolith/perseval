#[test]
fn reanalysis_is_append_only_and_switches_the_active_identity_atomically() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-immutable", "root", None);

    let first = result(
        "trace-immutable",
        1,
        vec![finding("trace-immutable", "finding-old", "signature-old")],
    );
    store.commit_analysis(&first).unwrap();

    let mut second = result(
        "trace-immutable",
        1,
        vec![finding("trace-immutable", "finding-new", "signature-new")],
    );
    second.identity.detector_profile_version = "2".into();
    second.detection_report.profile.profile_version = "2".into();
    second.analysis_id = second.identity.analysis_id();
    store.commit_analysis(&second).unwrap();

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let run_count: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM analysis_runs WHERE logical_trace_id = ?1 AND revision = 1",
            ["trace-immutable"],
            |row| row.get(0),
        )
        .unwrap();
    let active_id: String = control
        .query_row(
            "SELECT analysis_id FROM active_analysis_runs WHERE logical_trace_id = ?1",
            ["trace-immutable"],
            |row| row.get(0),
        )
        .unwrap();
    let old_findings: String = control
        .query_row(
            "SELECT findings_json FROM analysis_runs WHERE analysis_id = ?1",
            [&first.analysis_id],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(run_count, 2);
    assert_eq!(active_id, second.analysis_id);
    assert!(old_findings.contains("finding-old"));
    assert!(!old_findings.contains("finding-new"));
}

#[test]
fn inbox_reads_do_not_deserialize_aggregate_analysis_findings() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-indexed", "root", None);
    store
        .commit_analysis(&result(
            "trace-indexed",
            1,
            vec![finding(
                "trace-indexed",
                "finding-indexed",
                "signature-indexed",
            )],
        ))
        .unwrap();

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute(
            "UPDATE analysis_runs SET findings_json = 'deliberately invalid json'",
            [],
        )
        .unwrap();
    drop(control);

    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(groups.len(), 1);
    let group_id = &groups[0].group_id;
    assert!(store.get_failure_group(group_id).unwrap().is_some());
    assert_eq!(
        store
            .list_failure_occurrences(group_id, 0, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.failure_filter_options().unwrap().0,
        ["false_success_claim"]
    );
    assert!(
        store
            .get_finding_evidence(group_id, "finding-indexed", 8)
            .unwrap()
            .is_some()
    );
}

#[test]
fn inbox_summary_reads_only_materialized_scalar_columns() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-summary-index", "root", None);
    store
        .commit_analysis(&result(
            "trace-summary-index",
            1,
            vec![finding(
                "trace-summary-index",
                "finding-summary-index",
                "signature-summary-index",
            )],
        ))
        .unwrap();

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute(
            "UPDATE active_failure_findings SET finding_json = 'deliberately invalid json'",
            [],
        )
        .unwrap();
    drop(control);

    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].failure_signature, "signature-summary-index");
    assert_eq!(groups[0].occurrence_count, 1);
}

#[test]
fn complete_group_pagination_reaches_beyond_first_five_hundred() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-501-groups", "root", None);
    let findings = (0..501)
        .map(|index| {
            finding(
                "trace-501-groups",
                &format!("finding-{index:03}"),
                &format!("signature-{index:03}"),
            )
        })
        .collect();
    store
        .commit_analysis(&result("trace-501-groups", 1, findings))
        .unwrap();

    let first_page = store
        .list_failure_group_page(&FailureFiltersV1::default(), 0, 500)
        .unwrap();
    let second_page = store
        .list_failure_group_page(&FailureFiltersV1::default(), 500, 500)
        .unwrap();
    assert_eq!(first_page.offset, 0);
    assert_eq!(first_page.total, 501);
    assert_eq!(first_page.rows.len(), 500);
    assert_eq!(second_page.offset, 500);
    assert_eq!(second_page.total, 501);
    assert_eq!(second_page.rows.len(), 1);
    assert_ne!(first_page.rows[499].group_id, second_page.rows[0].group_id);

    let mut timings_ms = (0..10)
        .map(|_| {
            let started = Instant::now();
            let page = store
                .list_failure_group_page(&FailureFiltersV1::default(), 0, 200)
                .unwrap();
            assert_eq!(page.total, 501);
            assert_eq!(page.rows.len(), 200);
            started.elapsed().as_secs_f64() * 1_000.0
        })
        .collect::<Vec<_>>();
    timings_ms.sort_by(f64::total_cmp);
    let p95_ms = timings_ms[9];
    eprintln!(
        "finding-rich 501-group Inbox warm p95 ({}): {p95_ms:.3} ms",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    if !cfg!(debug_assertions) {
        assert!(
            p95_ms < 150.0,
            "finding-rich 501-group Inbox p95 {p95_ms:.3} ms exceeds 150 ms"
        );
    }

    let measured_group = first_page.rows[0].clone();
    let occurrence = store
        .list_failure_occurrences_in_scope(
            &measured_group.scope,
            &measured_group.group_id,
            0,
            1,
        )
        .unwrap()
        .remove(0);
    let mut detail_timings_ms = (0..10)
        .map(|_| {
            let started = Instant::now();
            assert!(
                store
                    .get_failure_group_in_scope(
                        &measured_group.scope,
                        &measured_group.group_id,
                    )
                    .unwrap()
                    .is_some()
            );
            started.elapsed().as_secs_f64() * 1_000.0
        })
        .collect::<Vec<_>>();
    detail_timings_ms.sort_by(f64::total_cmp);
    let detail_p95_ms = detail_timings_ms[9];
    let mut evidence_timings_ms = (0..10)
        .map(|_| {
            let started = Instant::now();
            assert!(
                store
                    .get_finding_evidence_in_scope(
                        &measured_group.scope,
                        &measured_group.group_id,
                        &occurrence.finding.finding_id,
                        128,
                    )
                    .unwrap()
                    .is_some()
            );
            started.elapsed().as_secs_f64() * 1_000.0
        })
        .collect::<Vec<_>>();
    evidence_timings_ms.sort_by(f64::total_cmp);
    let evidence_p95_ms = evidence_timings_ms[9];
    eprintln!(
        "finding-rich group detail warm p95: {detail_p95_ms:.3} ms; bounded evidence warm p95: {evidence_p95_ms:.3} ms"
    );
    if !cfg!(debug_assertions) {
        assert!(
            detail_p95_ms < 200.0,
            "group detail p95 {detail_p95_ms:.3} ms exceeds 200 ms"
        );
        assert!(
            evidence_p95_ms < 200.0,
            "bounded evidence p95 {evidence_p95_ms:.3} ms exceeds 200 ms"
        );
    }
}

#[test]
fn one_large_group_keeps_summary_detail_and_occurrence_pages_bounded() {
    const FINDING_COUNT: usize = 10_000;
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-large-group", "root", None);
    let findings = (0..FINDING_COUNT)
        .map(|index| {
            finding(
                "trace-large-group",
                &format!("large-finding-{index:05}"),
                "signature-one-large-group",
            )
        })
        .collect();
    store
        .commit_analysis(&result("trace-large-group", 1, findings))
        .unwrap();

    let page = store
        .list_failure_group_page(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].occurrence_count, FINDING_COUNT as u64);
    let group = page.rows[0].clone();
    assert!(
        store
            .get_failure_group_in_scope(&group.scope, &group.group_id)
            .unwrap()
            .is_some()
    );

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute(
            "UPDATE active_failure_findings
                SET finding_json = 'deliberately invalid json outside requested page'
              WHERE finding_id = 'large-finding-09999'",
            [],
        )
        .unwrap();
    let (evidence_refs, diagnostics, membership_diagnostics): (i64, i64, i64) = control
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM active_failure_evidence_refs),
                (SELECT COUNT(*) FROM active_failure_diagnostics),
                (SELECT SUM(telemetry_gap_count) FROM active_failure_group_memberships)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(evidence_refs, FINDING_COUNT as i64);
    assert_eq!(diagnostics, membership_diagnostics * FINDING_COUNT as i64);
    drop(control);

    let occurrences = store
        .list_failure_occurrences_in_scope(&group.scope, &group.group_id, 0, 25)
        .unwrap();
    assert_eq!(occurrences.len(), 25);
    assert_eq!(occurrences[0].finding.finding_id, "large-finding-00000");
    assert_eq!(occurrences[24].finding.finding_id, "large-finding-00024");
}

#[test]
fn recurrence_uses_one_shared_eligible_run_denominator() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    for (trace_id, started_at) in [
        ("trace-affected-a", 5_i64),
        ("trace-affected-b", 15_i64),
        ("trace-clean", 65_i64),
    ] {
        ingest(&store, trace_id, "root", None);
        let control = rusqlite::Connection::open(layout.control_database()).unwrap();
        control
            .execute(
                "UPDATE logical_traces SET start_time_unix_nano = ?2 WHERE external_trace_id = ?1",
                rusqlite::params![trace_id, started_at],
            )
            .unwrap();
        drop(control);
        let findings = if trace_id == "trace-clean" {
            Vec::new()
        } else {
            vec![finding(
                trace_id,
                &format!("finding-{trace_id}"),
                "signature-recurrence",
            )]
        };
        store
            .commit_analysis(&result(trace_id, 1, findings))
            .unwrap();
    }

    let groups = store
        .list_failure_groups(
            &FailureFiltersV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    started_after_unix_nano: Some(0),
                    started_before_unix_nano: Some(69),
                    ..QueryScopeCriteriaV1::default()
                }),
                ..FailureFiltersV1::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(groups.len(), 1);
    let recurrence = groups[0]
        .recurrence
        .as_ref()
        .expect("ready runs provide a denominator-backed recurrence series");
    assert_eq!(
        recurrence
            .buckets
            .iter()
            .map(|bucket| bucket.eligible_run_count)
            .sum::<u64>(),
        3
    );
    assert_eq!(
        recurrence
            .buckets
            .iter()
            .map(|bucket| bucket.affected_run_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(
        recurrence
            .buckets
            .iter()
            .map(|bucket| bucket.finding_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(groups[0].occurrence_trend.iter().sum::<u64>(), 2);
    assert_eq!(recurrence.buckets.len(), 3);
    assert_eq!(recurrence.buckets[0].affected_run_count, 2);
    assert_eq!(
        recurrence.buckets[0].recurrence_rate_basis_points,
        Some(10_000)
    );
    assert_eq!(recurrence.buckets[2].eligible_run_count, 1);
    assert_eq!(recurrence.buckets[2].affected_run_count, 0);
    assert_eq!(recurrence.buckets[2].recurrence_rate_basis_points, Some(0));

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let (memberships, occurrences) = control
        .query_row(
            "SELECT COUNT(*), SUM(occurrence_count)
               FROM active_failure_group_memberships
              WHERE project_id = 'checkout'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(memberships, 2);
    assert_eq!(occurrences, 2);
}

#[test]
fn finding_disposition_is_reversible_persistent_scoped_and_version_aware() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    ingest(&store, "trace-review", "root", None);
    let first = result(
        "trace-review",
        1,
        vec![finding(
            "trace-review",
            "finding-review",
            "signature-review",
        )],
    );
    store.commit_analysis(&first).unwrap();
    let group = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap()
        .remove(0);

    let confirmed = store
        .set_finding_disposition(
            &group.scope,
            &group.group_id,
            "finding-review",
            FindingDispositionStateV1::Confirmed,
        )
        .unwrap();
    assert_eq!(confirmed.state, FindingDispositionStateV1::Confirmed);
    assert!(
        store
            .set_finding_disposition(
                &scope(Some("support")),
                &group.group_id,
                "finding-review",
                FindingDispositionStateV1::Dismissed,
            )
            .is_err(),
        "a review cannot cross its immutable project scope"
    );

    drop(store);
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    let reviewed = store
        .get_failure_group_in_scope(&group.scope, &group.group_id)
        .unwrap()
        .unwrap();
    assert_eq!(reviewed.summary.confirmed_count, 1);
    assert_eq!(reviewed.summary.unreviewed_count, 0);
    let occurrence = store
        .list_failure_occurrences_in_scope(&group.scope, &group.group_id, 0, 10)
        .unwrap()
        .remove(0);
    assert_eq!(
        occurrence.disposition.as_ref().map(|value| value.state),
        Some(FindingDispositionStateV1::Confirmed)
    );
    assert!(!occurrence.disposition_stale);

    let mut replacement = result(
        "trace-review",
        1,
        vec![finding(
            "trace-review",
            "finding-review",
            "signature-review",
        )],
    );
    replacement.identity.detector_profile_version = "2".into();
    replacement.detection_report.profile.profile_version = "2".into();
    replacement.analysis_id = replacement.identity.analysis_id();
    store.commit_analysis(&replacement).unwrap();
    let stale = store
        .get_failure_group_in_scope(&group.scope, &group.group_id)
        .unwrap()
        .unwrap();
    assert_eq!(stale.summary.confirmed_count, 0);
    assert_eq!(stale.summary.unreviewed_count, 1);
    assert_eq!(stale.summary.stale_disposition_count, 1);
    let occurrence = store
        .list_failure_occurrences_in_scope(&group.scope, &group.group_id, 0, 10)
        .unwrap()
        .remove(0);
    assert!(occurrence.disposition_stale);

    store
        .set_finding_disposition(
            &group.scope,
            &group.group_id,
            "finding-review",
            FindingDispositionStateV1::Dismissed,
        )
        .unwrap();
    assert!(
        store
            .list_failure_groups(
                &FailureFiltersV1 {
                    scope: group.scope.clone(),
                    ..FailureFiltersV1::default()
                },
                0,
                10,
            )
            .unwrap()
            .is_empty(),
        "fully dismissed groups leave the default actionable Inbox"
    );
    let dismissed = store
        .list_failure_groups(
            &FailureFiltersV1 {
                scope: group.scope.clone(),
                include_fully_dismissed: true,
                ..FailureFiltersV1::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(dismissed[0].dismissed_count, 1);
    store
        .set_finding_disposition(
            &group.scope,
            &group.group_id,
            "finding-review",
            FindingDispositionStateV1::NeedsContext,
        )
        .unwrap();
    assert!(
        store
            .undo_finding_disposition(&group.scope, &group.group_id, "finding-review")
            .unwrap()
    );
    let unreviewed = store
        .get_failure_group_in_scope(&group.scope, &group.group_id)
        .unwrap()
        .unwrap();
    assert_eq!(unreviewed.summary.needs_context_count, 0);
    assert_eq!(unreviewed.summary.unreviewed_count, 1);
    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let event_count: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM finding_disposition_events WHERE finding_id = 'finding-review'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 4);
}

#[test]
fn reopening_rebuilds_a_missing_active_failure_projection() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    {
        let store = WorkspaceStore::open(&layout, "test").unwrap();
        ingest(&store, "trace-backfill", "root", None);
        store
            .commit_analysis(&result(
                "trace-backfill",
                1,
                vec![finding(
                    "trace-backfill",
                    "finding-backfill",
                    "signature-backfill",
                )],
            ))
            .unwrap();
    }

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute("DELETE FROM active_failure_findings", [])
        .unwrap();
    control
        .execute("DELETE FROM active_failure_group_memberships", [])
        .unwrap();
    control
        .execute("DELETE FROM active_failure_projection_state", [])
        .unwrap();
    drop(control);

    let store = WorkspaceStore::open(&layout, "test").unwrap();
    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].failure_signature, "signature-backfill");

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let projection_rows: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM active_failure_projection_state",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(projection_rows, 1);
    let membership_rows: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM active_failure_group_memberships",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(membership_rows, 1);
}

#[test]
fn reopening_rebuilds_an_outdated_active_failure_projection() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    {
        let store = WorkspaceStore::open(&layout, "test").unwrap();
        ingest(&store, "trace-outdated-projection", "root", None);
        store
            .commit_analysis(&result(
                "trace-outdated-projection",
                1,
                vec![finding(
                    "trace-outdated-projection",
                    "finding-outdated-projection",
                    "signature-outdated-projection",
                )],
            ))
            .unwrap();
    }

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute(
            "UPDATE active_failure_projection_state
                SET projection_schema_version = 'perseval.active_failure_projection.legacy'",
            [],
        )
        .unwrap();
    control
        .execute(
            "UPDATE active_failure_findings SET presentation_json = NULL",
            [],
        )
        .unwrap();
    drop(control);

    let store = WorkspaceStore::open(&layout, "test").unwrap();
    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert!(groups[0].presentation.is_some());
    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    let version: String = control
        .query_row(
            "SELECT projection_schema_version FROM active_failure_projection_state",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION);
}

#[test]
fn opening_a_v2_failure_projection_adds_v3_columns_and_backfills_atomically() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    {
        let store = WorkspaceStore::open(&layout, "test").unwrap();
        ingest(&store, "trace-v2-migration", "root", None);
        store
            .commit_analysis(&result(
                "trace-v2-migration",
                1,
                vec![finding(
                    "trace-v2-migration",
                    "finding-v2-migration",
                    "signature-v2-migration",
                )],
            ))
            .unwrap();
    }

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute_batch(
            "DELETE FROM schema_migrations WHERE version = 23;
             DROP TABLE active_failure_evidence_refs;
             DROP TABLE active_failure_diagnostics;
             DROP TABLE active_failure_group_detectors;
             DROP TABLE active_failure_findings;
             DROP TABLE active_failure_group_memberships;
             CREATE TABLE active_failure_findings(
                finding_id TEXT PRIMARY KEY,
                projection_schema_version TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v2',
                logical_trace_id TEXT NOT NULL, revision INTEGER NOT NULL,
                analysis_id TEXT NOT NULL, failure_signature TEXT NOT NULL,
                group_id TEXT NOT NULL, detector_id TEXT NOT NULL,
                detector_version TEXT NOT NULL, severity TEXT NOT NULL,
                recovery TEXT NOT NULL, subject TEXT, operation TEXT,
                created_at TEXT NOT NULL, finding_json TEXT NOT NULL,
                presentation_json TEXT, telemetry_gaps_json TEXT NOT NULL,
                adapter_id TEXT NOT NULL, adapter_version TEXT NOT NULL
             );
             CREATE TABLE active_failure_group_memberships(
                logical_trace_id TEXT NOT NULL, group_id TEXT NOT NULL,
                projection_schema_version TEXT NOT NULL DEFAULT 'perseval.active_failure_projection.v2',
                project_id TEXT NOT NULL, revision INTEGER NOT NULL,
                analysis_id TEXT NOT NULL, failure_signature TEXT NOT NULL,
                detector_ids_json TEXT NOT NULL, finding_ids_json TEXT NOT NULL,
                occurrence_count INTEGER NOT NULL, severity TEXT NOT NULL,
                recovered_count INTEGER NOT NULL, unrecovered_count INTEGER NOT NULL,
                unknown_recovery_count INTEGER NOT NULL, first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL,
                PRIMARY KEY(logical_trace_id, group_id)
             );
             UPDATE active_failure_projection_state
                SET projection_schema_version = 'perseval.active_failure_projection.v2';",
        )
        .unwrap();
    drop(control);

    let store = WorkspaceStore::open(&layout, "test").unwrap();
    let groups = store
        .list_failure_groups(&FailureFiltersV1::default(), 0, 10)
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].failure_signature, "signature-v2-migration");
    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    for (table, column) in [
        ("active_failure_findings", "project_id"),
        ("active_failure_findings", "run_started_at_unix_nano"),
        ("active_failure_group_memberships", "presentation_json"),
        ("active_failure_group_memberships", "unreviewed_count"),
    ] {
        let found: bool = control
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .any(|name| name.unwrap() == column);
        assert!(found, "{table}.{column} was not migrated");
    }
    let evidence_rows: i64 = control
        .query_row(
            "SELECT COUNT(*) FROM active_failure_evidence_refs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence_rows, 1);
}

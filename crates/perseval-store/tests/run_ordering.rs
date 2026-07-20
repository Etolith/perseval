use std::collections::BTreeMap;

use perseval_store::{
    RunFiltersV1, RunOrderV1, SPAN_UPSERT_SCHEMA_VERSION, SpanUpsertBatchV1, SpanUpsertV1,
    WorkspaceStore, WorkspaceStoreLayout,
};
use rusqlite::{Connection, params};

fn span(trace_id: &str, span_id: &str, start_time: u64) -> SpanUpsertV1 {
    SpanUpsertV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        external_trace_id: trace_id.into(),
        external_span_id: span_id.into(),
        external_parent_span_id: None,
        logical_trace_id: trace_id.into(),
        content_hash: String::new(),
        observed_at_unix_nano: start_time + 2,
        name: span_id.into(),
        category: "agent".into(),
        span_kind: 0,
        start_time_unix_nano: start_time,
        end_time_unix_nano: start_time + 1,
        status_code: 1,
        status_message: String::new(),
        trace_state: String::new(),
        flags: 0,
        dropped_attributes_count: 0,
        dropped_events_count: 0,
        dropped_links_count: 0,
        resource: BTreeMap::new(),
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

fn ingest(store: &WorkspaceStore, trace_id: &str, starts: &[u64]) {
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "test".into(),
        received_at_unix_ms: starts[0] as i64,
        spans: starts
            .iter()
            .enumerate()
            .map(|(index, start)| span(trace_id, &format!("span-{index}"), *start))
            .collect(),
        rejected_spans: 0,
        rejection_message: None,
    };
    let receipt = store
        .journal_batch(
            &mut batch,
            format!("{trace_id}:{starts:?}").as_bytes(),
            "test",
            4_096,
        )
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();
}

#[test]
fn run_order_is_global_and_applied_before_pagination() {
    let workspace = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(workspace.path()), "test").unwrap();

    // Commit order deliberately disagrees with trace chronology.
    ingest(&store, "newest", &[300]);
    ingest(&store, "middle", &[200, 201]);
    ingest(&store, "oldest", &[100]);

    assert_eq!(store.lifecycle_counts().unwrap(), (3, 0, 0, 0));

    let newest = store.list_runs(0, 3).unwrap();
    assert_eq!(
        newest
            .iter()
            .map(|run| run.logical_trace_id.as_str())
            .collect::<Vec<_>>(),
        ["newest", "middle", "oldest"]
    );

    let oldest_first_page = store
        .list_runs_filtered_ordered(&RunFiltersV1::default(), RunOrderV1::Oldest, 0, 1)
        .unwrap();
    let oldest_second_page = store
        .list_runs_filtered_ordered(&RunFiltersV1::default(), RunOrderV1::Oldest, 1, 1)
        .unwrap();
    assert_eq!(oldest_first_page[0].logical_trace_id, "oldest");
    assert_eq!(oldest_second_page[0].logical_trace_id, "middle");

    let most_spans = store
        .list_runs_filtered_ordered(&RunFiltersV1::default(), RunOrderV1::MostSpans, 0, 1)
        .unwrap();
    assert_eq!(most_spans[0].logical_trace_id, "middle");
}

#[test]
fn run_order_indexes_cover_workspace_and_project_sorts() {
    let workspace = tempfile::tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(workspace.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    drop(store);

    let connection = Connection::open(layout.control_database()).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'index' AND name LIKE 'idx_traces_workspace_%'",
        )
        .unwrap();
    let indexes = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .unwrap();

    for expected in [
        "idx_traces_workspace_started_desc",
        "idx_traces_workspace_started_asc",
        "idx_traces_workspace_spans",
        "idx_traces_workspace_findings",
        "idx_traces_workspace_project_started_desc",
        "idx_traces_workspace_project_started_asc",
        "idx_traces_workspace_project_spans",
        "idx_traces_workspace_project_findings",
    ] {
        assert!(
            indexes.contains(expected),
            "missing run-order index {expected}"
        );
    }

    let migrated = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 21)",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap();
    assert!(migrated);

    for (project_predicate, project_id, ordering, expected_index) in [
        (
            "?2 = ''",
            "",
            "start_time_unix_nano DESC, logical_trace_id ASC",
            "idx_traces_workspace_started_desc",
        ),
        (
            "?2 = ''",
            "",
            "start_time_unix_nano ASC, logical_trace_id ASC",
            "idx_traces_workspace_started_asc",
        ),
        (
            "?2 = ''",
            "",
            "span_count DESC, start_time_unix_nano DESC, logical_trace_id ASC",
            "idx_traces_workspace_spans",
        ),
        (
            "?2 = ''",
            "",
            "finding_count DESC, start_time_unix_nano DESC, logical_trace_id ASC",
            "idx_traces_workspace_findings",
        ),
        (
            "project_id = ?2",
            "project-a",
            "start_time_unix_nano DESC, logical_trace_id ASC",
            "idx_traces_workspace_project_started_desc",
        ),
        (
            "project_id = ?2",
            "project-a",
            "start_time_unix_nano ASC, logical_trace_id ASC",
            "idx_traces_workspace_project_started_asc",
        ),
        (
            "project_id = ?2",
            "project-a",
            "span_count DESC, start_time_unix_nano DESC, logical_trace_id ASC",
            "idx_traces_workspace_project_spans",
        ),
        (
            "project_id = ?2",
            "project-a",
            "finding_count DESC, start_time_unix_nano DESC, logical_trace_id ASC",
            "idx_traces_workspace_project_findings",
        ),
    ] {
        let plan = connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN
                 SELECT logical_trace_id FROM logical_traces
                  WHERE workspace_id = ?1 AND {project_predicate}
                  ORDER BY {ordering} LIMIT ?3 OFFSET ?4"
            ))
            .unwrap()
            .query_map(params!["test", project_id, 100_i64, 0_i64], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains(expected_index),
            "expected {expected_index} in query plan:\n{plan}"
        );
        assert!(
            !plan.contains("USE TEMP B-TREE FOR ORDER BY"),
            "ordering should stay index-backed:\n{plan}"
        );
    }
}

use std::collections::BTreeMap;

use perseval_store::{
    RunFiltersV1, RunOrderV1, SPAN_UPSERT_SCHEMA_VERSION, SpanUpsertBatchV1, SpanUpsertV1,
    WorkspaceStore, WorkspaceStoreLayout,
};

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

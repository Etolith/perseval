use std::collections::BTreeMap;

use perseval_store::{
    SPAN_UPSERT_SCHEMA_VERSION, SpanEventV1, SpanLinkV1, SpanUpsertBatchV1, SpanUpsertV1,
    WorkspaceStore, WorkspaceStoreLayout,
};
use serde_json::{Value, json};
use tempfile::tempdir;
use traces_to_evals::{
    AgentBehaviorNormalizer, FactQuality, OpenInferenceBehaviorNormalizer, SourceSpanStatus,
    ToolCallStatus,
};

fn tool_span(span_id: &str, status_code: i32) -> SpanUpsertV1 {
    SpanUpsertV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "otlp-http".into(),
        external_trace_id: "00112233445566778899aabbccddeeff".into(),
        external_span_id: span_id.into(),
        external_parent_span_id: None,
        logical_trace_id: "logical-trace".into(),
        content_hash: String::new(),
        observed_at_unix_nano: 2_000_000_000,
        name: "browser.search".into(),
        category: "tool".into(),
        span_kind: 3,
        start_time_unix_nano: 1_000_000_000,
        end_time_unix_nano: 1_001_500_000,
        status_code,
        status_message: "top-secret status message".into(),
        trace_state: String::new(),
        flags: 1,
        dropped_attributes_count: 0,
        dropped_events_count: 0,
        dropped_links_count: 0,
        resource: BTreeMap::from([("service.name".into(), json!("test-agent"))]),
        scope: BTreeMap::from([("name".into(), json!("test-instrumentation"))]),
        attributes: BTreeMap::from([
            ("gen_ai.operation.name".into(), json!("search")),
            ("gen_ai.tool.name".into(), json!("browser")),
            (
                "gen_ai.tool.call.arguments".into(),
                json!({"query": "top-secret query", "page": 2}),
            ),
            ("private.customer".into(), json!("small-secret")),
        ]),
        payload_refs: BTreeMap::new(),
        payload_identities: BTreeMap::new(),
        events: vec![SpanEventV1 {
            name: "progress".into(),
            timestamp_unix_nano: 1_001_000_000,
            attributes: BTreeMap::from([
                ("exception.type".into(), json!("NetworkError")),
                ("private.event".into(), json!("event-secret")),
            ]),
            dropped_attributes_count: 0,
        }],
        links: vec![SpanLinkV1 {
            trace_id: "ffeeddccbbaa99887766554433221100".into(),
            span_id: "0102030405060708".into(),
            trace_state: "vendor=state".into(),
            attributes: BTreeMap::from([("private.link".into(), json!("link-secret"))]),
            dropped_attributes_count: 0,
            flags: 1,
        }],
        decoder_version: "otlp-http.v1".into(),
        semantic_mapping_version: "otel-genai.v1".into(),
    }
}

#[test]
fn durable_roundtrip_is_lossless_for_safe_facts_and_never_reveals_payloads() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "otlp-http".into(),
        received_at_unix_ms: 1,
        spans: vec![
            tool_span("0101010101010101", 1),
            tool_span("0202020202020202", 2),
        ],
        rejected_spans: 0,
        rejection_message: None,
    };

    let receipt = store
        .journal_batch(
            &mut batch,
            b"opaque-wire-payload",
            "application/x-protobuf",
            4096,
        )
        .unwrap();
    assert!(
        batch.spans[0]
            .attributes
            .contains_key("gen_ai.tool.call.arguments")
    );
    assert!(batch.spans[0].payload_identities.is_empty());
    store.project_journal(receipt.journal_sequence).unwrap();

    let input = store.load_behavior_input("logical-trace", 1).unwrap();
    let fingerprint = input.trace.spans[0].payload_identities["gen_ai.tool.call.arguments"]
        .fingerprint
        .clone();
    assert!(fingerprint.starts_with("sha256:"));
    let encoded = serde_json::to_string(&input).unwrap();
    for secret in ["top-secret", "small-secret", "event-secret", "link-secret"] {
        assert!(!encoded.contains(secret));
    }
    assert_eq!(input.trace.spans.len(), 2);
    assert_eq!(input.trace.spans[0].source_status, SourceSpanStatus::Ok);
    assert_eq!(input.trace.spans[1].source_status, SourceSpanStatus::Error);
    assert_eq!(
        input.trace.spans[0].start_time_unix_nano,
        Some(1_000_000_000)
    );
    assert_eq!(input.trace.spans[0].duration_nano, Some(1_500_000));
    assert_eq!(
        input.trace.spans[0].payload_identities["gen_ai.tool.call.arguments"].fingerprint,
        fingerprint
    );
    assert_eq!(
        input.trace.spans[0].payload_identities["gen_ai.tool.call.arguments"].quality,
        FactQuality::Explicit
    );
    assert!(
        input.trace.spans[0].events[0]
            .identity
            .starts_with("event:sha256:")
    );
    assert!(
        input.trace.spans[0].links[0]
            .identity
            .starts_with("link:sha256:")
    );

    let behavior = OpenInferenceBehaviorNormalizer::default()
        .normalize_input(&input)
        .unwrap();
    assert_eq!(behavior.tool_calls[0].status, ToolCallStatus::Succeeded);
    assert_eq!(behavior.tool_calls[1].status, ToolCallStatus::Failed);
    assert_eq!(behavior.tool_calls[0].duration_nano, Some(1_500_000));
    assert_eq!(
        behavior.tool_calls[0].invocation_fingerprint,
        Some(fingerprint)
    );
    assert!(
        behavior.tool_calls[0]
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "span_event")
    );
    assert!(
        behavior.tool_calls[0]
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "span_link")
    );
}

#[test]
fn canonical_payload_fingerprint_ignores_object_key_order() {
    let directory = tempdir().unwrap();
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "test").unwrap();
    let mut first = tool_span("0101010101010101", 1);
    first.attributes.insert(
        "tool.arguments".into(),
        Value::Object(
            [("b".into(), json!(2)), ("a".into(), json!(1))]
                .into_iter()
                .collect(),
        ),
    );
    let mut second = tool_span("0202020202020202", 1);
    second.attributes.insert(
        "tool.arguments".into(),
        Value::Object(
            [("a".into(), json!(1)), ("b".into(), json!(2))]
                .into_iter()
                .collect(),
        ),
    );
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "otlp-http".into(),
        received_at_unix_ms: 1,
        spans: vec![first, second],
        rejected_spans: 0,
        rejection_message: None,
    };

    let receipt = store
        .journal_batch(&mut batch, b"wire", "application/x-protobuf", 4096)
        .unwrap();
    assert!(
        batch
            .spans
            .iter()
            .all(|span| span.payload_identities.is_empty())
    );
    store.project_journal(receipt.journal_sequence).unwrap();
    let input = store.load_behavior_input("logical-trace", 1).unwrap();
    let fingerprints = input
        .trace
        .spans
        .iter()
        .map(|span| {
            span.payload_identities["tool.arguments"]
                .fingerprint
                .clone()
        })
        .collect::<Vec<_>>();

    assert_eq!(fingerprints[0], fingerprints[1]);
}

#[test]
fn reconciles_control_state_after_analytics_commits_first() {
    let directory = tempdir().unwrap();
    let layout = WorkspaceStoreLayout::new(directory.path());
    let store = WorkspaceStore::open(&layout, "test").unwrap();
    let mut batch = SpanUpsertBatchV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: "otlp-http".into(),
        received_at_unix_ms: 1,
        spans: vec![
            tool_span("0101010101010101", 1),
            tool_span("0202020202020202", 2),
        ],
        rejected_spans: 0,
        rejection_message: None,
    };
    let receipt = store
        .journal_batch(
            &mut batch,
            b"analytics-first-control-recovery",
            "application/x-protobuf",
            4096,
        )
        .unwrap();

    let control = rusqlite::Connection::open(layout.control_database()).unwrap();
    control
        .execute_batch(
            "CREATE TRIGGER fail_projection_control
             BEFORE INSERT ON trace_delta_outbox
             BEGIN
               SELECT RAISE(FAIL, 'injected failure after analytics commit');
             END;",
        )
        .unwrap();
    let error = store
        .project_journal(receipt.journal_sequence)
        .expect_err("control commit should fail after DuckDB commits");
    assert!(error.to_string().contains("injected failure"));
    assert!(store.list_runs(0, 10).unwrap().is_empty());

    control
        .execute_batch("DROP TRIGGER fail_projection_control;")
        .unwrap();
    store.project_journal(receipt.journal_sequence).unwrap();

    let runs = store.list_runs(0, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].span_count, 2);
    assert_eq!(runs[0].error_count, 1);
    let spans = store
        .list_spans("logical-trace", 1, 0, 10, None, false)
        .unwrap();
    assert_eq!(spans.len(), 2);

    let projected: i64 = control
        .query_row(
            "SELECT projected FROM ingest_journal WHERE sequence = ?1",
            [receipt.journal_sequence as i64],
            |row| row.get(0),
        )
        .unwrap();
    let outbox_rows: i64 = control
        .query_row("SELECT COUNT(*) FROM trace_delta_outbox", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(projected, 1);
    assert_eq!(outbox_rows, 1);
}

use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::Path;

use duckdb::{AccessMode, Config, Connection as DuckConnection};
use flate2::read::GzDecoder;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use perseval_store::{SpanUpsertBatchV1, WorkspaceStore, WorkspaceStoreLayout};
use prost::Message;
use rusqlite::Connection as SqliteConnection;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::guard::{GuardSummary, inspect_fact_map, inspect_json_document};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IsolationAuditReport {
    pub schema_version: &'static str,
    pub journal_batches: u64,
    pub raw_otlp_batches: u64,
    pub normalized_batches: u64,
    pub sqlite_json_documents: u64,
    pub duckdb_json_documents: u64,
    pub blob_json_documents: u64,
    pub blobs_inspected: u64,
    pub packed_payload_json_documents: u64,
    pub packed_payload_blobs_inspected: u64,
    pub spans: u64,
    pub inspected_facts: u64,
    pub violations: u64,
}

#[derive(Debug)]
struct JournalBlobRow {
    sequence: u64,
    raw_hash: String,
    normalized_hash: String,
    wire_encoding: String,
}

pub fn audit_workspace(workspace: &Path) -> Result<IsolationAuditReport, Box<dyn Error>> {
    let layout = WorkspaceStoreLayout::new(workspace);
    let control = open_sqlite_read_only(&layout.control_database())?;
    let journals = load_journal_blobs(&control)?;
    if journals.is_empty() {
        return Err("workspace isolation audit found no journal batches".into());
    }

    let mut summary = GuardSummary {
        traces: 0,
        spans: 0,
        inspected_facts: 0,
    };
    let store = WorkspaceStore::open(&layout, "default")?;
    for journal in &journals {
        let raw = store.reveal_blob(&journal.raw_hash, usize::MAX)?;
        inspect_raw_otlp(
            &raw,
            &journal.wire_encoding,
            &format!("raw OTLP journal sequence {}", journal.sequence),
            &mut summary,
        )?;

        let normalized = store.reveal_blob(&journal.normalized_hash, usize::MAX)?;
        let batch: SpanUpsertBatchV1 = serde_json::from_slice(&normalized)?;
        inspect_normalized_batch(&batch, &journal.normalized_hash, &mut summary)?;
    }

    let (blobs_inspected, blob_json_documents) =
        inspect_blob_json_documents(&store, &layout, &mut summary)?;
    drop(store);

    let sqlite_json_documents = inspect_sqlite_json(&control, &mut summary)?;
    drop(control);
    let duckdb_json_documents = inspect_duckdb_json(&layout, &mut summary)?;
    let (packed_payload_blobs_inspected, packed_payload_json_documents) =
        inspect_packed_payloads(&layout, &mut summary)?;

    Ok(IsolationAuditReport {
        schema_version: "perseval.benchmark_isolation_audit.v3",
        journal_batches: journals.len() as u64,
        raw_otlp_batches: journals.len() as u64,
        normalized_batches: journals.len() as u64,
        sqlite_json_documents,
        duckdb_json_documents,
        blob_json_documents,
        blobs_inspected,
        packed_payload_json_documents,
        packed_payload_blobs_inspected,
        spans: summary.spans,
        inspected_facts: summary.inspected_facts,
        violations: 0,
    })
}

fn inspect_packed_payloads(
    layout: &WorkspaceStoreLayout,
    summary: &mut GuardSummary,
) -> Result<(u64, u64), Box<dyn Error>> {
    let path = layout.analytics_directory().join("traces.duckdb");
    let config = Config::default().access_mode(AccessMode::ReadOnly)?;
    let analytics = DuckConnection::open_with_flags(path, config)?;
    let mut statement = analytics.prepare("SELECT sha256, compressed FROM payload_blobs")?;
    let payloads = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut documents = 0_u64;
    for (hash, compressed) in &payloads {
        let bytes = zstd::stream::decode_all(compressed.as_slice())?;
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            inspect_json_document(&value, &format!("packed payload {hash}"), summary)?;
            documents = documents.saturating_add(1);
        }
    }
    Ok((payloads.len() as u64, documents))
}

fn open_sqlite_read_only(path: &Path) -> Result<SqliteConnection, Box<dyn Error>> {
    Ok(SqliteConnection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?)
}

fn load_journal_blobs(control: &SqliteConnection) -> Result<Vec<JournalBlobRow>, Box<dyn Error>> {
    let mut statement = control.prepare(
        "SELECT sequence, raw_blob_hash, normalized_blob_hash, wire_encoding
         FROM ingest_journal ORDER BY sequence",
    )?;
    Ok(statement
        .query_map([], |row| {
            Ok(JournalBlobRow {
                sequence: row.get::<_, i64>(0)?.max(0) as u64,
                raw_hash: row.get(1)?,
                normalized_hash: row.get(2)?,
                wire_encoding: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn inspect_raw_otlp(
    raw: &[u8],
    wire_encoding: &str,
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    let (content_type, content_encoding) = wire_encoding
        .rsplit_once('+')
        .unwrap_or((wire_encoding, "identity"));
    let decoded = match content_encoding {
        "identity" | "" => raw.to_vec(),
        "gzip" => {
            let mut decoder = GzDecoder::new(raw);
            let mut decoded = Vec::new();
            decoder.read_to_end(&mut decoded)?;
            decoded
        }
        other => return Err(format!("unsupported journal encoding {other:?}").into()),
    };
    let request: ExportTraceServiceRequest = match content_type {
        "application/x-protobuf" | "application/protobuf" => {
            ExportTraceServiceRequest::decode(decoded.as_slice())?
        }
        "application/json" => serde_json::from_slice(&decoded)?,
        other => return Err(format!("unsupported journal content type {other:?}").into()),
    };
    inspect_otlp_request(&request, context, summary)
}

fn inspect_otlp_request(
    request: &ExportTraceServiceRequest,
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    for resource_spans in &request.resource_spans {
        if let Some(resource) = &resource_spans.resource {
            inspect_otlp_attributes(&resource.attributes, context, summary)?;
        }
        for scope_spans in &resource_spans.scope_spans {
            if let Some(scope) = &scope_spans.scope {
                inspect_otlp_attributes(&scope.attributes, context, summary)?;
            }
            for span in &scope_spans.spans {
                inspect_otlp_attributes(&span.attributes, context, summary)?;
                for event in &span.events {
                    inspect_otlp_attributes(&event.attributes, context, summary)?;
                }
                for link in &span.links {
                    inspect_otlp_attributes(&link.attributes, context, summary)?;
                }
            }
        }
    }
    Ok(())
}

fn inspect_otlp_attributes(
    attributes: &[KeyValue],
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    let facts = attributes
        .iter()
        .map(|attribute| {
            (
                attribute.key.clone(),
                attribute
                    .value
                    .as_ref()
                    .map(any_value_json)
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    inspect_fact_map(&facts, context, summary)
}

fn any_value_json(value: &AnyValue) -> Value {
    match &value.value {
        Some(any_value::Value::StringValue(value)) => Value::String(value.clone()),
        Some(any_value::Value::BoolValue(value)) => Value::Bool(*value),
        Some(any_value::Value::IntValue(value)) => (*value).into(),
        Some(any_value::Value::DoubleValue(value)) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(any_value::Value::BytesValue(value)) => Value::String(hex::encode(value)),
        Some(any_value::Value::ArrayValue(value)) => {
            Value::Array(value.values.iter().map(any_value_json).collect())
        }
        Some(any_value::Value::KvlistValue(value)) => Value::Object(
            value
                .values
                .iter()
                .map(|entry| {
                    (
                        entry.key.clone(),
                        entry
                            .value
                            .as_ref()
                            .map(any_value_json)
                            .unwrap_or(Value::Null),
                    )
                })
                .collect::<Map<_, _>>(),
        ),
        Some(any_value::Value::StringValueStrindex(value)) => (*value).into(),
        None => Value::Null,
    }
}

fn inspect_normalized_batch(
    batch: &SpanUpsertBatchV1,
    hash: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    for span in &batch.spans {
        let context = format!(
            "normalized batch {hash} trace {} span {}",
            span.logical_trace_id, span.external_span_id
        );
        inspect_fact_map(&span.resource, &context, summary)?;
        inspect_fact_map(&span.scope, &context, summary)?;
        inspect_fact_map(&span.attributes, &context, summary)?;
        for event in &span.events {
            inspect_fact_map(&event.attributes, &context, summary)?;
        }
        for link in &span.links {
            inspect_fact_map(&link.attributes, &context, summary)?;
        }
    }
    summary.spans = summary.spans.saturating_add(batch.spans.len() as u64);
    Ok(())
}

fn inspect_blob_json_documents(
    store: &WorkspaceStore,
    layout: &WorkspaceStoreLayout,
    summary: &mut GuardSummary,
) -> Result<(u64, u64), Box<dyn Error>> {
    let mut blobs = 0_u64;
    let mut documents = 0_u64;
    for prefix in fs::read_dir(layout.blob_directory())? {
        let prefix = prefix?;
        if !prefix.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(prefix.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let filename = entry.file_name();
            let filename = filename.to_string_lossy();
            let Some(hash) = filename.strip_suffix(".zst") else {
                continue;
            };
            blobs = blobs.saturating_add(1);
            let bytes = store.reveal_blob(hash, usize::MAX)?;
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                inspect_json_document(&value, &format!("JSON blob {hash}"), summary)?;
                documents = documents.saturating_add(1);
            }
        }
    }
    Ok((blobs, documents))
}

fn inspect_sqlite_json(
    control: &SqliteConnection,
    summary: &mut GuardSummary,
) -> Result<u64, Box<dyn Error>> {
    let queries = [
        (
            "trace_delta_outbox.delta_json",
            "SELECT delta_json FROM trace_delta_outbox",
        ),
        (
            "analysis_results.behavior_json",
            "SELECT behavior_json FROM analysis_results",
        ),
        (
            "analysis_results.findings_json",
            "SELECT findings_json FROM analysis_results",
        ),
        (
            "evidence_packets.packet_json",
            "SELECT packet_json FROM evidence_packets",
        ),
        (
            "eval_candidates.candidate_json",
            "SELECT candidate_json FROM eval_candidates",
        ),
        (
            "eval_batch_previews.preview_json",
            "SELECT preview_json FROM eval_batch_previews",
        ),
        (
            "candidate_generation_jobs.job_json",
            "SELECT job_json FROM candidate_generation_jobs",
        ),
        (
            "feature_similarity_models.model_json",
            "SELECT model_json FROM semantic_cluster_models",
        ),
        (
            "trace_comparisons.result_json",
            "SELECT result_json FROM trace_comparisons",
        ),
    ];
    let mut documents = 0_u64;
    for (name, query) in queries {
        let mut statement = control.prepare(query)?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for value in values {
            let document: Value = serde_json::from_str(&value)
                .map_err(|error| format!("invalid stored JSON in {name}: {error}"))?;
            inspect_json_document(&document, name, summary)?;
            documents = documents.saturating_add(1);
        }
    }
    Ok(documents)
}

fn inspect_duckdb_json(
    layout: &WorkspaceStoreLayout,
    summary: &mut GuardSummary,
) -> Result<u64, Box<dyn Error>> {
    let path = layout.analytics_directory().join("traces.duckdb");
    let config = Config::default().access_mode(AccessMode::ReadOnly)?;
    let analytics = DuckConnection::open_with_flags(path, config)?;
    let queries = [
        ("spans.attributes_json", "SELECT attributes_json FROM spans"),
        (
            "spans.payload_refs_json",
            "SELECT payload_refs_json FROM spans",
        ),
        ("spans.resource_json", "SELECT resource_json FROM spans"),
        ("spans.scope_json", "SELECT scope_json FROM spans"),
        (
            "span_events.attributes_json",
            "SELECT attributes_json FROM span_events",
        ),
        (
            "span_links.attributes_json",
            "SELECT attributes_json FROM span_links",
        ),
    ];
    let mut documents = 0_u64;
    for (name, query) in queries {
        let mut statement = analytics.prepare(query)?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for value in values {
            let document: Value = serde_json::from_str(&value)
                .map_err(|error| format!("invalid stored JSON in {name}: {error}"))?;
            inspect_json_document(&document, name, summary)?;
            documents = documents.saturating_add(1);
        }
    }
    Ok(documents)
}

#[cfg(test)]
mod tests {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use opentelemetry_proto::tonic::common::v1::any_value;
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
    use std::io::Write;

    use super::*;

    fn request_with_attribute(key: &str, value: &str) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: key.into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(value.into())),
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }
    }

    fn summary() -> GuardSummary {
        GuardSummary {
            traces: 0,
            spans: 0,
            inspected_facts: 0,
        }
    }

    #[test]
    fn raw_protobuf_audit_rejects_a_target_attribute() {
        let bytes = request_with_attribute("benchmark.resolved", "true").encode_to_vec();
        let error = inspect_raw_otlp(
            &bytes,
            "application/x-protobuf+identity",
            "test request",
            &mut summary(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("held-out target key"));
    }

    #[test]
    fn raw_gzip_protobuf_audit_accepts_withheld_disclosure() {
        let bytes = request_with_attribute("benchmark.label_visibility", "withheld-sidecar")
            .encode_to_vec();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();

        inspect_raw_otlp(
            &compressed,
            "application/x-protobuf+gzip",
            "test request",
            &mut summary(),
        )
        .unwrap();
    }
}

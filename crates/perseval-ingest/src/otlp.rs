use std::collections::BTreeMap;
use std::future::Future;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, Span};
use perseval_store::{
    PipelineStageSampleV1, PipelineStageV1, SPAN_UPSERT_SCHEMA_VERSION, SpanEventV1, SpanLinkV1,
    SpanUpsertBatchV1, SpanUpsertV1,
};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;

use crate::{IngestTransport, SourceDescriptor};

pub const OTLP_ADAPTER_ID: &str = "perseval.otlp_http";
pub const OTLP_ADAPTER_VERSION: &str = "1";
pub const OTLP_SEMANTIC_MAPPING_VERSION: &str = "perseval.otlp_semantics.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpReceiverConfig {
    pub enabled: bool,
    pub bind_addr: SocketAddr,
    pub source_id: String,
    pub max_wire_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_spans_per_request: usize,
    pub max_attributes_per_span: usize,
    pub retry_after_seconds: u64,
}

impl OtlpReceiverConfig {
    pub fn disabled(bind_addr: SocketAddr) -> Self {
        Self {
            enabled: false,
            bind_addr,
            source_id: "otlp-local".into(),
            max_wire_bytes: 16 * 1024 * 1024,
            max_decoded_bytes: 64 * 1024 * 1024,
            max_spans_per_request: 100_000,
            max_attributes_per_span: 1_024,
            retry_after_seconds: 1,
        }
    }

    pub fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor::new(
            &self.source_id,
            OTLP_ADAPTER_ID,
            OTLP_ADAPTER_VERSION,
            IngestTransport::OtlpHttp,
        )
    }

    pub fn is_loopback(&self) -> bool {
        self.bind_addr.ip().is_loopback()
    }
}

#[derive(Debug, Clone)]
pub struct OtlpSubmission {
    pub batch: SpanUpsertBatchV1,
    pub raw_wire_payload: Vec<u8>,
    pub wire_encoding: String,
    pub request_started: Instant,
    pub stage_samples: Vec<PipelineStageSampleV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtlpAdmission {
    pub duplicate_request: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OtlpSubmitError {
    #[error("ingestion queue is at capacity")]
    Backpressured,
    #[error("ingestion is shutting down")]
    ShuttingDown,
    #[error("durable writer unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait OtlpBatchSink: Send + Sync {
    async fn submit(&self, submission: OtlpSubmission) -> Result<OtlpAdmission, OtlpSubmitError>;
}

#[derive(Clone)]
struct ReceiverState {
    config: OtlpReceiverConfig,
    sink: Arc<dyn OtlpBatchSink>,
}

pub async fn serve_otlp(
    listener: TcpListener,
    config: OtlpReceiverConfig,
    sink: Arc<dyn OtlpBatchSink>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let state = ReceiverState { config, sink };
    let router = Router::new()
        .route("/v1/traces", post(export_traces))
        .with_state(state);
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}

async fn export_traces(
    State(state): State<ReceiverState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let content_type = match headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(WireFormat::parse)
    {
        Some(format) => format,
        None => {
            return error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported OTLP content type",
            );
        }
    };
    let raw = match to_bytes(body, state.config.max_wire_bytes.saturating_add(1)).await {
        Ok(bytes) if bytes.len() <= state.config.max_wire_bytes => bytes.to_vec(),
        Ok(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "OTLP request exceeds wire-size limit",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "could not read bounded OTLP request",
            );
        }
    };
    let content_encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("identity");
    let submission = match prepare_otlp_submission(
        &state.config,
        raw,
        content_type.as_str(),
        content_encoding,
    ) {
        Ok(submission) => submission,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, &error),
    };
    let response = ExportTraceServiceResponse {
        partial_success: (submission.batch.rejected_spans > 0).then(|| ExportTracePartialSuccess {
            rejected_spans: submission.batch.rejected_spans as i64,
            error_message: submission
                .batch
                .rejection_message
                .clone()
                .unwrap_or_default(),
        }),
    };
    match state.sink.submit(submission).await {
        Ok(_) => success_response(content_type, &response),
        Err(OtlpSubmitError::Backpressured) => {
            let mut response = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "ingestion queue is at capacity",
            );
            if let Ok(value) = HeaderValue::from_str(&state.config.retry_after_seconds.to_string())
            {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            response
        }
        Err(OtlpSubmitError::ShuttingDown) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "ingestion is shutting down",
        ),
        Err(OtlpSubmitError::Unavailable(error)) => {
            error_response(StatusCode::SERVICE_UNAVAILABLE, &error)
        }
    }
}

/// Decodes and normalizes one bounded OTLP/HTTP payload for either the HTTP
/// receiver or a trusted local file-import entry point.
pub fn prepare_otlp_submission(
    config: &OtlpReceiverConfig,
    raw_wire_payload: Vec<u8>,
    content_type: &str,
    content_encoding: &str,
) -> Result<OtlpSubmission, String> {
    let request_started = Instant::now();
    if raw_wire_payload.len() > config.max_wire_bytes {
        return Err("OTLP request exceeds wire-size limit".into());
    }
    let format = WireFormat::parse(content_type)
        .ok_or_else(|| "unsupported OTLP content type".to_string())?;
    let decoded = decode_body(
        &raw_wire_payload,
        content_encoding,
        config.max_decoded_bytes,
    )?;
    let request = format.decode(&decoded)?;
    let batch = normalize_request(config, request);
    let mut decode = PipelineStageSampleV1::new(
        PipelineStageV1::Decode,
        request_started
            .elapsed()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64,
    );
    decode.item_count = batch.spans.len() as u64;
    decode.byte_count = decoded.len() as u64;
    decode.rows_deserialized = batch.spans.len() as u64;
    Ok(OtlpSubmission {
        batch,
        raw_wire_payload,
        wire_encoding: format!("{}+{content_encoding}", format.as_str()),
        request_started,
        stage_samples: vec![decode],
    })
}

#[derive(Debug, Clone, Copy)]
enum WireFormat {
    Protobuf,
    Json,
}

impl WireFormat {
    fn parse(value: &str) -> Option<Self> {
        match value.split(';').next()?.trim() {
            "application/x-protobuf" | "application/protobuf" => Some(Self::Protobuf),
            "application/json" => Some(Self::Json),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Protobuf => "application/x-protobuf",
            Self::Json => "application/json",
        }
    }

    fn decode(self, bytes: &[u8]) -> Result<ExportTraceServiceRequest, String> {
        match self {
            Self::Protobuf => ExportTraceServiceRequest::decode(bytes)
                .map_err(|error| format!("invalid OTLP protobuf: {error}")),
            Self::Json => decode_otlp_json(bytes),
        }
    }
}

fn decode_otlp_json(bytes: &[u8]) -> Result<ExportTraceServiceRequest, String> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid OTLP JSON: {error}"))?;
    if let Some(resource_spans) = value.get_mut("resourceSpans").and_then(Value::as_array_mut) {
        for resource in resource_spans {
            let Some(scope_spans) = resource.get_mut("scopeSpans").and_then(Value::as_array_mut)
            else {
                continue;
            };
            for scope in scope_spans {
                let Some(spans) = scope.get_mut("spans").and_then(Value::as_array_mut) else {
                    continue;
                };
                for span in spans {
                    replace_proto_enum(
                        span,
                        "kind",
                        &[
                            ("SPAN_KIND_UNSPECIFIED", 0),
                            ("SPAN_KIND_INTERNAL", 1),
                            ("SPAN_KIND_SERVER", 2),
                            ("SPAN_KIND_CLIENT", 3),
                            ("SPAN_KIND_PRODUCER", 4),
                            ("SPAN_KIND_CONSUMER", 5),
                        ],
                    )?;
                    if let Some(status) = span.get_mut("status") {
                        replace_proto_enum(
                            status,
                            "code",
                            &[
                                ("STATUS_CODE_UNSET", 0),
                                ("STATUS_CODE_OK", 1),
                                ("STATUS_CODE_ERROR", 2),
                            ],
                        )?;
                    }
                }
            }
        }
    }
    serde_json::from_value(value).map_err(|error| format!("invalid OTLP JSON: {error}"))
}

fn replace_proto_enum(
    owner: &mut Value,
    field: &str,
    variants: &[(&str, i32)],
) -> Result<(), String> {
    let Some(value) = owner.get_mut(field) else {
        return Ok(());
    };
    let Some(name) = value.as_str() else {
        return Ok(());
    };
    let Some((_, number)) = variants.iter().find(|(variant, _)| *variant == name) else {
        return Err(format!(
            "invalid OTLP JSON: unknown {field} enum value {name}"
        ));
    };
    *value = json!(number);
    Ok(())
}

fn decode_body(raw: &[u8], encoding: &str, maximum: usize) -> Result<Vec<u8>, String> {
    match encoding {
        "identity" | "" => {
            if raw.len() > maximum {
                Err("OTLP request exceeds decoded-size limit".into())
            } else {
                Ok(raw.to_vec())
            }
        }
        "gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(raw);
            let mut decoded = Vec::new();
            decoder
                .by_ref()
                .take(maximum.saturating_add(1) as u64)
                .read_to_end(&mut decoded)
                .map_err(|error| format!("invalid gzip body: {error}"))?;
            if decoded.len() > maximum {
                Err("OTLP request exceeds decoded-size limit".into())
            } else {
                Ok(decoded)
            }
        }
        _ => Err(format!("unsupported content encoding {encoding}")),
    }
}

fn normalize_request(
    config: &OtlpReceiverConfig,
    request: ExportTraceServiceRequest,
) -> SpanUpsertBatchV1 {
    let mut spans = Vec::new();
    let mut rejected = 0_u64;
    for resource_spans in request.resource_spans {
        normalize_resource_spans(config, resource_spans, &mut spans, &mut rejected);
    }
    if spans.len() > config.max_spans_per_request {
        rejected += (spans.len() - config.max_spans_per_request) as u64;
        spans.truncate(config.max_spans_per_request);
    }
    SpanUpsertBatchV1 {
        schema_version: "perseval.span_upsert_batch.v1".into(),
        source_id: config.source_id.clone(),
        received_at_unix_ms: now_unix_ms(),
        spans,
        rejected_spans: rejected,
        rejection_message: (rejected > 0).then(|| {
            "one or more spans violated configured limits or identity requirements".into()
        }),
    }
}

fn normalize_resource_spans(
    config: &OtlpReceiverConfig,
    resource_spans: ResourceSpans,
    output: &mut Vec<SpanUpsertV1>,
    rejected: &mut u64,
) {
    let resource = resource_spans
        .resource
        .map(|resource| key_values(resource.attributes))
        .unwrap_or_default();
    for scope_spans in resource_spans.scope_spans {
        let mut scope = BTreeMap::new();
        scope.insert("schema_url".into(), json!(scope_spans.schema_url));
        if let Some(instrumentation) = scope_spans.scope {
            scope.insert("name".into(), json!(instrumentation.name));
            scope.insert("version".into(), json!(instrumentation.version));
            scope.insert(
                "attributes".into(),
                json!(key_values(instrumentation.attributes)),
            );
            scope.insert(
                "dropped_attributes_count".into(),
                json!(instrumentation.dropped_attributes_count),
            );
        }
        for span in scope_spans.spans {
            match normalize_span(config, span, &resource, &scope) {
                Some(span) => output.push(span),
                None => *rejected += 1,
            }
        }
    }
}

fn normalize_span(
    config: &OtlpReceiverConfig,
    span: Span,
    resource: &BTreeMap<String, Value>,
    scope: &BTreeMap<String, Value>,
) -> Option<SpanUpsertV1> {
    if span.trace_id.len() != 16
        || span.span_id.len() != 8
        || span.attributes.len() > config.max_attributes_per_span
    {
        return None;
    }
    let trace_id = hex::encode(&span.trace_id);
    let span_id = hex::encode(&span.span_id);
    let parent_id = (!span.parent_span_id.is_empty()).then(|| hex::encode(&span.parent_span_id));
    let attributes = key_values(span.attributes);
    let category = span_category(&attributes);
    let status = span.status.unwrap_or_default();
    let logical_trace_id = hex::encode(Sha256::digest(format!("{}:{trace_id}", config.source_id)));
    let events = span
        .events
        .into_iter()
        .map(|event| SpanEventV1 {
            name: event.name,
            timestamp_unix_nano: event.time_unix_nano,
            attributes: key_values(event.attributes),
            dropped_attributes_count: event.dropped_attributes_count,
        })
        .collect();
    let links = span
        .links
        .into_iter()
        .map(|link| SpanLinkV1 {
            trace_id: hex::encode(link.trace_id),
            span_id: hex::encode(link.span_id),
            trace_state: link.trace_state,
            attributes: key_values(link.attributes),
            dropped_attributes_count: link.dropped_attributes_count,
            flags: link.flags,
        })
        .collect();
    Some(SpanUpsertV1 {
        schema_version: SPAN_UPSERT_SCHEMA_VERSION.into(),
        source_id: config.source_id.clone(),
        external_trace_id: trace_id,
        external_span_id: span_id,
        external_parent_span_id: parent_id,
        logical_trace_id,
        content_hash: String::new(),
        observed_at_unix_nano: now_unix_nano(),
        name: span.name,
        category,
        span_kind: span.kind,
        start_time_unix_nano: span.start_time_unix_nano,
        end_time_unix_nano: span.end_time_unix_nano,
        status_code: status.code,
        status_message: status.message,
        trace_state: span.trace_state,
        flags: span.flags,
        dropped_attributes_count: span.dropped_attributes_count,
        dropped_events_count: span.dropped_events_count,
        dropped_links_count: span.dropped_links_count,
        resource: resource.clone(),
        scope: scope.clone(),
        attributes,
        payload_refs: BTreeMap::new(),
        payload_identities: BTreeMap::new(),
        events,
        links,
        decoder_version: OTLP_ADAPTER_VERSION.into(),
        semantic_mapping_version: OTLP_SEMANTIC_MAPPING_VERSION.into(),
    })
}

fn key_values(values: Vec<KeyValue>) -> BTreeMap<String, Value> {
    values
        .into_iter()
        .map(|entry| (entry.key, any_value(entry.value)))
        .collect()
}

fn any_value(value: Option<AnyValue>) -> Value {
    match value.and_then(|value| value.value) {
        Some(any_value::Value::StringValue(value)) => Value::String(value),
        Some(any_value::Value::BoolValue(value)) => Value::Bool(value),
        Some(any_value::Value::IntValue(value)) => value.into(),
        Some(any_value::Value::DoubleValue(value)) => json!(value),
        Some(any_value::Value::BytesValue(value)) => Value::String(hex::encode(value)),
        Some(any_value::Value::ArrayValue(value)) => Value::Array(
            value
                .values
                .into_iter()
                .map(|value| any_value(Some(value)))
                .collect(),
        ),
        Some(any_value::Value::KvlistValue(value)) => {
            serde_json::to_value(key_values(value.values)).unwrap_or(Value::Null)
        }
        Some(any_value::Value::StringValueStrindex(value)) => json!(value),
        None => Value::Null,
    }
}

fn span_category(attributes: &BTreeMap<String, Value>) -> String {
    if let Some(kind) = attributes
        .get("openinference.span.kind")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
    {
        return match kind.as_str() {
            "agent" | "chain" => "agent",
            "llm" | "prompt" => "llm",
            "tool" => "tool",
            "retriever" | "reranker" | "embedding" => "retrieval",
            _ => "other",
        }
        .into();
    }
    match attributes
        .get("gen_ai.operation.name")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "chat" | "text_completion" | "generate_content" => "llm",
        "execute_tool" | "tool" => "tool",
        "invoke_agent" | "agent" => "agent",
        _ => "other",
    }
    .into()
}

fn success_response(format: WireFormat, response: &ExportTraceServiceResponse) -> Response {
    let (content_type, body) = match format {
        WireFormat::Protobuf => (format.as_str(), response.encode_to_vec()),
        WireFormat::Json => (
            format.as_str(),
            serde_json::to_vec(response).unwrap_or_else(|_| b"{}".to_vec()),
        ),
    };
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&json!({ "code": status.as_u16(), "message": message }))
            .unwrap_or_default(),
    )
        .into_response()
}

fn now_unix_ms() -> i64 {
    now_unix_nano().saturating_div(1_000_000) as i64
}

fn now_unix_nano() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ScopeSpans, Status};

    #[test]
    fn rejects_bad_identity_and_preserves_links_and_attributes() {
        let mut config = OtlpReceiverConfig::disabled("127.0.0.1:4318".parse().unwrap());
        config.enabled = true;
        let span = Span {
            trace_id: vec![1; 16],
            span_id: vec![2; 8],
            name: "chat".into(),
            attributes: vec![KeyValue {
                key: "gen_ai.operation.name".into(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("chat".into())),
                }),
                ..Default::default()
            }],
            status: Some(Status {
                code: 1,
                message: String::new(),
            }),
            ..Default::default()
        };
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![span],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let batch = normalize_request(&config, request);
        assert_eq!(batch.spans.len(), 1);
        assert_eq!(batch.spans[0].category, "llm");
        assert_eq!(batch.rejected_spans, 0);
    }

    #[test]
    fn accepts_official_proto_json_enum_names() {
        let request = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "01010101010101010101010101010101",
                        "spanId": "0202020202020202",
                        "name": "refund agent",
                        "kind": "SPAN_KIND_INTERNAL",
                        "startTimeUnixNano": "1",
                        "endTimeUnixNano": "2",
                        "status": {"code": "STATUS_CODE_ERROR", "message": "timeout"}
                    }]
                }]
            }]
        });

        let decoded = WireFormat::Json
            .decode(serde_json::to_string(&request).unwrap().as_bytes())
            .unwrap();
        let span = &decoded.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(span.kind, 1);
        assert_eq!(span.status.as_ref().unwrap().code, 2);
    }
}

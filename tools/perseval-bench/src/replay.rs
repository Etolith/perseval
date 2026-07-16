use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::Instant;

use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{
    AnyValue, ArrayValue, KeyValue, KeyValueList, any_value,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span as OtlpSpan, Status};
use prost::Message;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use traces_to_evals::{Span, SpanKind, Trace};

use crate::guard::guard_fixture;

const DEFAULT_TIMESTAMP_OFFSET_NANO: u64 = 1_782_864_000_000_000_000;

#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub endpoint: String,
    pub fixture: std::path::PathBuf,
    pub project: String,
    pub batch_size: usize,
    pub timestamp_offset_nano: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayReport {
    pub schema_version: &'static str,
    pub traces: u64,
    pub spans: u64,
    pub requests: u64,
    pub rejected_spans: u64,
    pub elapsed_ms: f64,
    pub spans_per_second: f64,
    pub acknowledgement_latency_ms: LatencySummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct LatencySummary {
    pub samples: u64,
    pub minimum: f64,
    pub median: f64,
    pub p95: f64,
    pub maximum: f64,
}

impl ReplayOptions {
    pub fn new(endpoint: String, fixture: std::path::PathBuf, project: String) -> Self {
        Self {
            endpoint,
            fixture,
            project,
            batch_size: 2_048,
            timestamp_offset_nano: DEFAULT_TIMESTAMP_OFFSET_NANO,
        }
    }
}

pub async fn replay(options: &ReplayOptions) -> Result<ReplayReport, Box<dyn Error>> {
    if options.batch_size == 0 {
        return Err("replay batch size must be greater than zero".into());
    }
    guard_fixture(&options.fixture)?;
    let headers = otlp_headers()?;
    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/traces", options.endpoint.trim_end_matches('/'));
    let started = Instant::now();
    let mut report = ReplayReport {
        schema_version: "perseval.benchmark_replay_report.v2",
        traces: 0,
        spans: 0,
        requests: 0,
        rejected_spans: 0,
        elapsed_ms: 0.0,
        spans_per_second: 0.0,
        acknowledgement_latency_ms: LatencySummary::default(),
    };
    let mut acknowledgement_latencies = Vec::new();
    for (line_index, line) in BufReader::new(File::open(&options.fixture)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let trace: Trace = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid trace JSON on line {} of {}: {error}",
                line_index + 1,
                options.fixture.display()
            )
        })?;
        let trace_id = stable_id(&trace.id, 16);
        let resources = resource_attributes(&trace, &options.project);
        for chunk in trace.spans.chunks(options.batch_size) {
            let spans = chunk
                .iter()
                .map(|span| to_otlp_span(span, &trace_id, options.timestamp_offset_nano))
                .collect();
            let request = ExportTraceServiceRequest {
                resource_spans: vec![ResourceSpans {
                    resource: Some(Resource {
                        attributes: resources.clone(),
                        ..Default::default()
                    }),
                    scope_spans: vec![ScopeSpans {
                        spans,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            };
            let mut builder = client
                .post(&endpoint)
                .header("content-type", "application/x-protobuf");
            for (name, value) in &headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            let acknowledgement_started = Instant::now();
            let response = builder.body(request.encode_to_vec()).send().await?;
            let status = response.status();
            let body = response.bytes().await?;
            if !status.is_success() {
                return Err(format!(
                    "OTLP request {} failed with {status}: {}",
                    report.requests + 1,
                    String::from_utf8_lossy(&body[..body.len().min(500)])
                )
                .into());
            }
            if !body.is_empty() {
                let decoded = ExportTraceServiceResponse::decode(body.as_ref())?;
                if let Some(partial) = decoded.partial_success {
                    report.rejected_spans = report
                        .rejected_spans
                        .saturating_add(partial.rejected_spans.max(0) as u64);
                }
            }
            acknowledgement_latencies
                .push(acknowledgement_started.elapsed().as_secs_f64() * 1_000.0);
            report.requests += 1;
            report.spans += chunk.len() as u64;
        }
        report.traces += 1;
    }
    report.elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    report.spans_per_second = if report.elapsed_ms == 0.0 {
        0.0
    } else {
        report.spans as f64 / (report.elapsed_ms / 1_000.0)
    };
    report.acknowledgement_latency_ms = latency_summary(&mut acknowledgement_latencies);
    Ok(report)
}

fn latency_summary(values: &mut [f64]) -> LatencySummary {
    if values.is_empty() {
        return LatencySummary::default();
    }
    values.sort_by(f64::total_cmp);
    let percentile_index = |percentile: f64| {
        ((values.len() as f64 * percentile).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)
    };
    LatencySummary {
        samples: values.len() as u64,
        minimum: values[0],
        median: values[percentile_index(0.5)],
        p95: values[percentile_index(0.95)],
        maximum: values[values.len() - 1],
    }
}

fn resource_attributes(trace: &Trace, project: &str) -> Vec<KeyValue> {
    let metadata_string = |key: &str, fallback: &str| {
        trace
            .metadata
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned()
    };
    vec![
        string_attribute("service.name", project),
        string_attribute("openinference.project.name", project),
        string_attribute(
            "deployment.environment.name",
            &metadata_string("environment", "benchmark"),
        ),
        string_attribute("perseval.dataset", &metadata_string("source", "unknown")),
        string_attribute(
            "perseval.dataset.revision",
            &metadata_string("dataset_revision", "unknown"),
        ),
        string_attribute("benchmark.trace_id", &trace.id),
        string_attribute("benchmark.label_visibility", "withheld-sidecar"),
    ]
}

fn to_otlp_span(span: &Span, trace_id: &[u8], timestamp_offset_nano: u64) -> OtlpSpan {
    let mut attributes = vec![string_attribute(
        "openinference.span.kind",
        match span.kind {
            SpanKind::Llm => "LLM",
            SpanKind::Agent => "AGENT",
            SpanKind::Tool => "TOOL",
            SpanKind::Chain => "CHAIN",
            SpanKind::Retriever => "RETRIEVER",
            SpanKind::Reranker => "RERANKER",
            SpanKind::Embedding => "EMBEDDING",
            SpanKind::Guardrail => "GUARDRAIL",
            SpanKind::Evaluator => "EVALUATOR",
            SpanKind::Prompt => "PROMPT",
            SpanKind::Other => "UNKNOWN",
        },
    )];
    if let Some(input) = &span.input {
        attributes.push(string_attribute("input.value", input));
    }
    if let Some(output) = &span.output {
        attributes.push(string_attribute("output.value", output));
    }
    attributes.extend(span.attributes.iter().map(|(key, value)| KeyValue {
        key: key.clone(),
        value: Some(any_value(value)),
        ..Default::default()
    }));
    OtlpSpan {
        trace_id: trace_id.to_vec(),
        span_id: stable_id(&span.id, 8),
        parent_span_id: span
            .parent_id
            .as_deref()
            .map(|id| stable_id(id, 8))
            .unwrap_or_default(),
        name: span.name.clone(),
        start_time_unix_nano: timestamp_offset_nano.saturating_add(parse_time(&span.started_at)),
        end_time_unix_nano: timestamp_offset_nano.saturating_add(parse_time(&span.ended_at)),
        attributes,
        status: Some(Status {
            code: if span.error.is_some() { 2 } else { 1 },
            message: span.error.clone().unwrap_or_default(),
        }),
        ..Default::default()
    }
}

fn any_value(value: &Value) -> AnyValue {
    let value = match value {
        Value::Null => any_value::Value::StringValue("null".into()),
        Value::Bool(value) => any_value::Value::BoolValue(*value),
        Value::Number(value) => value
            .as_i64()
            .map(any_value::Value::IntValue)
            .unwrap_or_else(|| any_value::Value::DoubleValue(value.as_f64().unwrap_or_default())),
        Value::String(value) => any_value::Value::StringValue(value.clone()),
        Value::Array(values) => any_value::Value::ArrayValue(ArrayValue {
            values: values.iter().map(any_value).collect(),
        }),
        Value::Object(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| (key.clone(), value))
                .collect::<BTreeMap<_, _>>();
            any_value::Value::KvlistValue(KeyValueList {
                values: sorted
                    .into_iter()
                    .map(|(key, value)| KeyValue {
                        key,
                        value: Some(any_value(value)),
                        ..Default::default()
                    })
                    .collect(),
            })
        }
    };
    AnyValue { value: Some(value) }
}

fn stable_id(value: &str, bytes: usize) -> Vec<u8> {
    Sha256::digest(value.as_bytes())[..bytes].to_vec()
}

fn parse_time(value: &Option<String>) -> u64 {
    value
        .as_deref()
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

fn string_attribute(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
        ..Default::default()
    }
}

fn otlp_headers() -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let Some(raw) = env::var_os("OTEL_EXPORTER_OTLP_HEADERS") else {
        return Ok(Vec::new());
    };
    raw.to_string_lossy()
        .split(',')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            let (name, value) = entry
                .split_once('=')
                .ok_or_else(|| format!("invalid OTLP header entry: {entry:?}"))?;
            Ok((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn converts_json_attributes_without_stringifying_types() {
        assert!(matches!(
            any_value(&json!(true)).value,
            Some(any_value::Value::BoolValue(true))
        ));
        assert!(matches!(
            any_value(&json!(42)).value,
            Some(any_value::Value::IntValue(42))
        ));
        assert!(matches!(
            any_value(&json!(["a", "b"])).value,
            Some(any_value::Value::ArrayValue(_))
        ));
    }

    #[test]
    fn reports_nearest_rank_acknowledgement_percentiles() {
        let mut values = (1..=100).map(f64::from).rev().collect::<Vec<_>>();
        let summary = latency_summary(&mut values);
        assert_eq!(summary.samples, 100);
        assert_eq!(summary.minimum, 1.0);
        assert_eq!(summary.median, 50.0);
        assert_eq!(summary.p95, 95.0);
        assert_eq!(summary.maximum, 100.0);
    }

    #[test]
    fn stable_ids_match_the_fixture_contract() {
        assert_eq!(
            hex::encode(stable_id("swesmith-003079c48b2e4f19a2aba236", 16)),
            "9262c67eb9050a693210e78e26452e01"
        );
    }
}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use traces_to_evals::io::jsonl::JsonlFile;
use traces_to_evals::{SpanKind, Trace};

pub const TRACE_FILE_ENV: &str = "PERSEVAL_TRACE_FILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRequest {
    pub offset: u64,
    pub limit: u32,
}

impl PageRequest {
    pub fn bounded(offset: u64, limit: u32, maximum: u32) -> Self {
        Self {
            offset,
            limit: limit.min(maximum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanCategory {
    Agent,
    Llm,
    Tool,
    Retrieval,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanView {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub category: SpanCategory,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub attributes: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceView {
    pub id: String,
    pub title: String,
    pub observed_at: Option<String>,
    pub environment: Option<String>,
    pub source: Option<String>,
    pub duration_ms: u64,
    pub cost: Option<f64>,
    pub score: Option<f64>,
    pub cluster: Option<String>,
    pub divergence: Option<String>,
    pub spans: Vec<SpanView>,
}

impl TraceView {
    pub fn failed(&self) -> bool {
        self.spans.iter().any(|span| span.error.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TraceCatalog {
    pub traces: Vec<TraceView>,
    pub configured_path: Option<PathBuf>,
}

impl TraceCatalog {
    pub fn from_environment() -> traces_to_evals::Result<Self> {
        let Some(path) = std::env::var_os(TRACE_FILE_ENV).map(PathBuf::from) else {
            return Ok(Self::default());
        };
        Self::from_jsonl(&path).map(|traces| Self {
            traces,
            configured_path: Some(path),
        })
    }

    pub fn from_jsonl(path: &Path) -> traces_to_evals::Result<Vec<TraceView>> {
        let traces: Vec<Trace> = JsonlFile::new(path).read_all()?;
        Ok(traces.into_iter().map(normalize_trace).collect())
    }
}

fn normalize_trace(trace: Trace) -> TraceView {
    let trace_start = trace
        .spans
        .iter()
        .filter_map(|span| timestamp_ms(span.started_at.as_deref()))
        .min();
    let trace_end = trace
        .spans
        .iter()
        .filter_map(|span| timestamp_ms(span.ended_at.as_deref()))
        .max();

    let title = metadata_string(&trace.metadata, &["name", "title", "operation.name"])
        .or_else(|| trace.spans.first().map(|span| span.name.clone()))
        .unwrap_or_else(|| trace.id.clone());
    let environment = metadata_string(&trace.metadata, &["environment", "deployment.environment"]);
    let source = metadata_string(
        &trace.metadata,
        &["source", "perseval.source", "service.name"],
    );
    let observed_at = metadata_string(&trace.metadata, &["observed_at", "timestamp"])
        .or_else(|| trace.spans.first().and_then(|span| span.started_at.clone()));
    let duration_ms = metadata_u64(&trace.metadata, &["duration_ms"]).unwrap_or_else(|| {
        trace_end
            .zip(trace_start)
            .map(|(end, start)| end.saturating_sub(start))
            .unwrap_or_default()
    });

    let spans = trace
        .spans
        .into_iter()
        .map(|span| {
            let started_at = timestamp_ms(span.started_at.as_deref());
            let ended_at = timestamp_ms(span.ended_at.as_deref());
            let start_ms = started_at
                .zip(trace_start)
                .map(|(start, root)| start.saturating_sub(root))
                .unwrap_or_default();
            let duration_ms =
                metadata_u64(&span.attributes, &["duration_ms"]).unwrap_or_else(|| {
                    ended_at
                        .zip(started_at)
                        .map(|(end, start)| end.saturating_sub(start))
                        .unwrap_or_default()
                });
            SpanView {
                id: span.id,
                parent_id: span.parent_id,
                name: span.name,
                category: match span.kind {
                    SpanKind::Agent | SpanKind::Chain => SpanCategory::Agent,
                    SpanKind::Llm | SpanKind::Prompt => SpanCategory::Llm,
                    SpanKind::Tool => SpanCategory::Tool,
                    SpanKind::Retriever | SpanKind::Reranker | SpanKind::Embedding => {
                        SpanCategory::Retrieval
                    }
                    _ => SpanCategory::Other,
                },
                start_ms,
                duration_ms,
                error: span.error,
                input: span.input,
                output: span.output,
                attributes: span
                    .attributes
                    .into_iter()
                    .map(|(key, value)| (key, display_value(&value)))
                    .collect(),
            }
        })
        .collect();

    TraceView {
        id: trace.id,
        title,
        observed_at,
        environment,
        source,
        duration_ms,
        cost: metadata_f64(&trace.metadata, &["cost", "total_cost"]),
        score: metadata_f64(&trace.metadata, &["score", "evaluation.score"]),
        cluster: metadata_string(&trace.metadata, &["cluster", "cluster_id", "shape_cluster"]),
        divergence: metadata_string(
            &trace.metadata,
            &["first_divergence", "divergence", "failure_summary"],
        ),
        spans,
    }
}

fn timestamp_ms(value: Option<&str>) -> Option<u64> {
    let value = value?;
    value.parse::<u64>().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .and_then(|timestamp| timestamp.timestamp_millis().try_into().ok())
    })
}

fn metadata_string(map: &BTreeMap<String, serde_json::Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| match value {
            serde_json::Value::String(value) => Some(value.clone()),
            serde_json::Value::Null => None,
            value => Some(display_value(value)),
        })
    })
}

fn metadata_u64(map: &BTreeMap<String, serde_json::Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        map.get(*key)
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
    })
}

fn metadata_f64(map: &BTreeMap<String, serde_json::Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        map.get(*key)
            .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
    })
}

fn display_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use traces_to_evals::{Span, SpanKind};

    #[test]
    fn bounds_page_size_at_the_service_boundary() {
        assert_eq!(PageRequest::bounded(0, 5_000, 500).limit, 500);
    }

    #[test]
    fn normalizes_metadata_and_span_specific_attributes() {
        let mut trace = Trace::new("trace-1").with_span(Span {
            kind: SpanKind::Tool,
            error: Some("declined".to_string()),
            started_at: Some("1000".to_string()),
            ended_at: Some("1250".to_string()),
            attributes: BTreeMap::from([(
                "service.name".to_string(),
                serde_json::Value::String("payments".to_string()),
            )]),
            ..Span::new("span-1", "authorize")
        });
        trace.metadata.insert(
            "environment".to_string(),
            serde_json::Value::String("test".to_string()),
        );

        let view = normalize_trace(trace);
        assert_eq!(view.environment.as_deref(), Some("test"));
        assert_eq!(view.duration_ms, 250);
        assert_eq!(view.spans[0].attributes[0].1, "payments");
        assert!(view.failed());
    }
}

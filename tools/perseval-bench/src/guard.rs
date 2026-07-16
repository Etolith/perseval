use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;
use traces_to_evals::{AgentBehaviorTrace, Trace};

const ALLOWED_DISCLOSURE_KEYS: &[&str] = &["benchmark.label_visibility"];
const FORBIDDEN_KEY_SEGMENTS: &[&str] = &[
    "resolved",
    "label",
    "ground_truth",
    "groundtruth",
    "target",
    "reward",
    "success_label",
    "failure_label",
    "outcome_label",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardSummary {
    pub traces: u64,
    pub spans: u64,
    pub inspected_facts: u64,
}

pub fn guard_fixture(path: &Path) -> Result<GuardSummary, Box<dyn Error>> {
    let mut summary = GuardSummary {
        traces: 0,
        spans: 0,
        inspected_facts: 0,
    };
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let trace: Trace = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid trace JSON on line {} of {}: {error}",
                line_index + 1,
                path.display()
            )
        })?;
        inspect_fact_map(
            &trace.metadata,
            &format!("trace {} metadata", trace.id),
            &mut summary,
        )?;
        for span in &trace.spans {
            inspect_fact_map(
                &span.attributes,
                &format!("trace {} span {} attributes", trace.id, span.id),
                &mut summary,
            )?;
        }
        summary.traces += 1;
        summary.spans += trace.spans.len() as u64;
    }
    if summary.traces == 0 {
        return Err(format!("fixture contains no traces: {}", path.display()).into());
    }
    Ok(summary)
}

pub fn guard_behavior_fixture(path: &Path) -> Result<GuardSummary, Box<dyn Error>> {
    let mut summary = GuardSummary {
        traces: 0,
        spans: 0,
        inspected_facts: 0,
    };
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let trace: AgentBehaviorTrace = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid behavior trace JSON on line {} of {}: {error}",
                line_index + 1,
                path.display()
            )
        })?;
        let document = serde_json::to_value(&trace)?;
        inspect_json_document(
            &document,
            &format!("behavior trace {}", trace.trace_id),
            &mut summary,
        )?;
        summary.traces += 1;
        summary.spans += trace.coverage.span_count;
    }
    if summary.traces == 0 {
        return Err(format!("fixture contains no behavior traces: {}", path.display()).into());
    }
    Ok(summary)
}

pub(crate) fn inspect_fact_map(
    facts: &std::collections::BTreeMap<String, Value>,
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    for (key, value) in facts {
        inspect_fact(key, value, context, summary)?;
    }
    Ok(())
}

pub(crate) fn inspect_json_document(
    value: &Value,
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    inspect_nested_value(value, context, summary)
}

pub(crate) fn inspect_fact(
    key: &str,
    value: &Value,
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    summary.inspected_facts += 1;
    if ALLOWED_DISCLOSURE_KEYS.contains(&key) {
        if value.as_str() != Some("withheld-sidecar") {
            return Err(format!(
                "{context} uses {key:?} without the required withheld-sidecar value"
            )
            .into());
        }
        return Ok(());
    }
    let normalized = key.to_ascii_lowercase().replace(['-', ' '], "_");
    if FORBIDDEN_KEY_SEGMENTS
        .iter()
        .any(|segment| normalized.split('.').any(|part| part == *segment))
    {
        return Err(format!(
            "held-out target key {key:?} leaked into {context}; labels must remain in the scoring sidecar"
        )
        .into());
    }
    inspect_nested_value(value, context, summary)
}

fn inspect_nested_value(
    value: &Value,
    context: &str,
    summary: &mut GuardSummary,
) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Array(values) => {
            for value in values {
                inspect_nested_value(value, context, summary)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                inspect_fact(key, value, context, summary)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use serde_json::json;
    use traces_to_evals::{Span, Trace};

    use super::*;

    fn write_trace(trace: &Trace) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.jsonl");
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(trace).unwrap()),
        )
        .unwrap();
        (directory, path)
    }

    #[test]
    fn accepts_explicit_withheld_disclosure() {
        let mut trace = Trace::new("trace-1").with_span(Span::new("span-1", "agent"));
        trace.metadata.insert(
            "benchmark.label_visibility".into(),
            json!("withheld-sidecar"),
        );
        let (_directory, path) = write_trace(&trace);

        assert_eq!(guard_fixture(&path).unwrap().traces, 1);
    }

    #[test]
    fn rejects_target_in_span_attributes() {
        let mut attributes = BTreeMap::new();
        attributes.insert("benchmark.resolved".into(), json!(true));
        let mut span = Span::new("span-1", "agent");
        span.attributes = attributes;
        let trace = Trace::new("trace-1").with_span(span);
        let (_directory, path) = write_trace(&trace);

        let error = guard_fixture(&path).unwrap_err().to_string();
        assert!(error.contains("held-out target key"));
    }

    #[test]
    fn rejects_targets_nested_inside_typed_attribute_values() {
        let mut trace = Trace::new("trace-1").with_span(Span::new("span-1", "agent"));
        trace.spans[0].attributes.insert(
            "request.metadata".into(),
            json!({"evaluation": {"target": false}}),
        );
        let (_directory, path) = write_trace(&trace);

        assert!(guard_fixture(&path).is_err());
    }
}

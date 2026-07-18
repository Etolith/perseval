use std::sync::Arc;

use rmcp::model::{JsonObject, Meta};
use rmcp::model::{TaskSupport, Tool, ToolAnnotations, ToolExecution};
use rmcp::schemars::{JsonSchema, schema_for};
use serde_json::{Value, json};

use crate::input::{
    GetEvalBatchJobInput, GetEvidenceTraceInput, GetFailureGroupInput, GetVerificationReportInput,
    InspectFindingInput, ListFailureGroupsInput, ListRunsInput, ListSessionsInput, PageInput,
};

pub(crate) fn read_tools(enabled: bool) -> Vec<Tool> {
    if !enabled {
        return Vec::new();
    }
    vec![
        descriptor::<PageInput>(
            "list_projects",
            "List projects",
            "List the projects in the open Perseval workspace using a bounded committed page.",
        ),
        descriptor::<ListSessionsInput>(
            "list_sessions",
            "List sessions",
            "List bounded agent sessions in an explicit project or all-projects scope.",
        ),
        descriptor::<ListRunsInput>(
            "list_runs",
            "List runs",
            "List bounded safe run summaries in an explicit project or all-projects scope.",
        ),
        descriptor::<ListFailureGroupsInput>(
            "list_failure_groups",
            "List failure groups",
            "List denominator-backed failure-group summaries without raw trace payloads.",
        ),
        descriptor::<GetFailureGroupInput>(
            "get_failure_group",
            "Get failure group",
            "Inspect one failure group with bounded representative occurrences and provenance.",
        ),
        descriptor::<InspectFindingInput>(
            "inspect_finding",
            "Inspect finding",
            "Inspect one immutable finding, its safe presentation, evidence references, and telemetry gaps.",
        ),
        descriptor::<GetEvidenceTraceInput>(
            "get_evidence_trace",
            "Get evidence trace",
            "Return a bounded safe span projection around cited evidence; payload bodies are never included.",
        ),
        descriptor::<GetEvalBatchJobInput>(
            "get_eval_batch_job",
            "Get eval batch job",
            "Read one durable eval-candidate generation job in a concrete project scope.",
        ),
        descriptor::<GetVerificationReportInput>(
            "get_verification_report",
            "Get verification report",
            "Read one durable remediation verification job or immutable report in a concrete project scope.",
        ),
    ]
}

fn descriptor<T: JsonSchema>(name: &'static str, title: &str, description: &'static str) -> Tool {
    let input_schema = typed_schema::<T>(&format!(
        "https://perseval.dev/schemas/mcp/v1/{name}.input.schema.json"
    ));
    let output_schema = output_schema(name);
    let annotations = ToolAnnotations::new()
        .read_only(true)
        .destructive(false)
        .idempotent(true)
        .open_world(false);
    let mut meta = Meta::new();
    meta.insert("dev.perseval/schemaVersion".into(), json!("v1"));
    meta.insert("dev.perseval/permissionClass".into(), json!("read"));
    meta.insert(
        "dev.perseval/dataClassification".into(),
        json!("safe_projection"),
    );
    Tool::new(name, description, input_schema)
        .with_title(title)
        .with_raw_output_schema(output_schema)
        .with_annotations(annotations)
        .with_execution(ToolExecution::new().with_task_support(TaskSupport::Forbidden))
        .with_meta(meta)
}

fn typed_schema<T: JsonSchema>(id: &str) -> Arc<JsonObject> {
    let mut value = serde_json::to_value(schema_for!(T)).expect("JSON Schema is serializable");
    inline_local_schema_refs(&mut value);
    let object = value
        .as_object_mut()
        .expect("MCP input schema root is an object");
    object.insert("$id".into(), Value::String(id.into()));
    Arc::new(object.clone())
}

fn inline_local_schema_refs(value: &mut Value) {
    let definitions = value
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    inline_schema_node(value, &definitions, &mut Vec::new());
    if let Some(object) = value.as_object_mut() {
        object.remove("$defs");
    }
}

fn inline_schema_node(
    value: &mut Value,
    definitions: &serde_json::Map<String, Value>,
    resolving: &mut Vec<String>,
) {
    let reference = value
        .as_object()
        .and_then(|object| object.get("$ref"))
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .map(str::to_owned);
    if let Some(name) = reference
        && !resolving.contains(&name)
        && let Some(definition) = definitions.get(&name)
    {
        resolving.push(name);
        let mut replacement = definition.clone();
        inline_schema_node(&mut replacement, definitions, resolving);
        resolving.pop();
        if let (Some(replacement), Some(original)) =
            (replacement.as_object_mut(), value.as_object())
        {
            replacement.extend(
                original
                    .iter()
                    .filter(|(key, _)| key.as_str() != "$ref")
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        *value = replacement;
        return;
    }
    match value {
        Value::Array(values) => {
            for value in values {
                inline_schema_node(value, definitions, resolving);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                inline_schema_node(value, definitions, resolving);
            }
        }
        _ => {}
    }
}

fn output_schema(name: &str) -> Arc<JsonObject> {
    let value = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://perseval.dev/schemas/mcp/v1/{name}.output.schema.json"),
        "type": "object",
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["schema_version", "ok", "request_id", "workspace_id", "commit_sequence", "data", "warnings"],
                "properties": {
                    "schema_version": {"const": format!("perseval.mcp.{name}.output.v1")},
                    "ok": {"const": true},
                    "request_id": {"type": "string"},
                    "workspace_id": {"type": "string"},
                    "commit_sequence": {"type": "string", "pattern": "^[0-9]+$"},
                    "scope_id": {"type": "string"},
                    "data": {"type": "object"},
                    "warnings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["code", "message"],
                            "properties": {
                                "code": {"type": "string"},
                                "message": {"type": "string"}
                            }
                        }
                    },
                    "next_cursor": {"type": "string"}
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["schema_version", "ok", "request_id", "error"],
                "properties": {
                    "schema_version": {"const": format!("perseval.mcp.{name}.output.v1")},
                    "ok": {"const": false},
                    "request_id": {"type": "string"},
                    "error": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["code", "message", "retryable"],
                        "properties": {
                            "code": {"type": "string"},
                            "message": {"type": "string"},
                            "retryable": {"type": "boolean"},
                            "retry_after_ms": {"type": "string", "pattern": "^[0-9]+$"},
                            "details": {"type": "object"}
                        }
                    }
                }
            }
        ]
    });
    Arc::new(
        value
            .as_object()
            .expect("output schema is an object")
            .clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_catalog_has_required_metadata_and_schema_ids() {
        let tools = read_tools(true);
        assert_eq!(tools.len(), 9);
        for tool in tools {
            assert!(tool.title.is_some());
            assert!(tool.output_schema.is_some());
            assert_eq!(tool.task_support(), TaskSupport::Forbidden);
            assert_eq!(
                tool.annotations.as_ref().unwrap().read_only_hint,
                Some(true)
            );
            assert_eq!(
                tool.input_schema.get("$id").and_then(Value::as_str),
                Some(
                    format!(
                        "https://perseval.dev/schemas/mcp/v1/{}.input.schema.json",
                        tool.name
                    )
                    .as_str()
                )
            );
        }
        assert!(read_tools(false).is_empty());
    }

    #[test]
    fn run_scope_schema_is_inline_and_discoverable() {
        let tool = read_tools(true)
            .into_iter()
            .find(|tool| tool.name == "list_runs")
            .expect("list_runs descriptor");
        let scope = tool
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get("scope"))
            .and_then(Value::as_object)
            .expect("inline scope schema");
        assert_eq!(scope.get("type"), Some(&json!("object")));
        let project = scope
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get("project"))
            .and_then(Value::as_object)
            .expect("inline project selector schema");
        assert!(project.get("oneOf").is_some());
    }
}

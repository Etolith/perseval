use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use duckdb::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use traces_to_evals::{Span, SpanKind, Trace};

use crate::fetch::{sha256_file, verify_source};
use crate::guard::guard_fixture;
use crate::manifest::{BenchmarkTier, SourceManifest};

const ROOT_START_NANO: u64 = 1_000_000_000;
const TURN_STRIDE_NANO: u64 = 10_000_000;
const SPAN_DURATION_NANO: u64 = 4_000_000;
const MAXIMUM_PAYLOAD_CHARACTERS: usize = 16_384;

#[derive(Debug, Clone)]
struct SourceRecord {
    instance_id: String,
    resolved: bool,
    model: String,
    trajectory_id: String,
    patch: Option<String>,
    messages: Vec<SourceMessage>,
    selection_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HeldOutLabel<'a> {
    trace_id: &'a str,
    instance_id: &'a str,
    trajectory_id: &'a str,
    resolved: bool,
    model: &'a str,
    group_key: String,
    split: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FixtureManifest {
    schema_version: &'static str,
    dataset: String,
    revision: String,
    source_artifact: String,
    source_sha256: String,
    tier: String,
    fixture_schema_version: String,
    selection_schema_version: String,
    selection: String,
    ground_truth_policy: &'static str,
    traces: u64,
    spans: u64,
    resolved: u64,
    unresolved: u64,
    fixture_sha256: String,
    labels_sha256: String,
    selected_trace_ids: Vec<String>,
    split_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixtureBuildReport {
    pub schema_version: &'static str,
    pub tier: String,
    pub fixture: PathBuf,
    pub labels: PathBuf,
    pub manifest: PathBuf,
    pub traces: u64,
    pub spans: u64,
    pub fixture_sha256: String,
    pub labels_sha256: String,
    pub manifest_sha256: String,
}

pub fn build_fixture(
    source_manifest_path: &Path,
    source: &Path,
    tier_name: &str,
    output_directory: &Path,
) -> Result<FixtureBuildReport, Box<dyn Error>> {
    let source_manifest = SourceManifest::load(source_manifest_path)?;
    verify_source(&source_manifest, source)?;
    let tier = source_manifest.tier(tier_name)?;
    let mut records = read_source(source)?;
    validate_source_counts(&source_manifest, &records)?;
    records.sort_by(|left, right| left.selection_hash.cmp(&right.selection_hash));
    let selected = select_records(tier, &records)?;

    fs::create_dir_all(output_directory)?;
    let base = format!("swesmith-{tier_name}");
    let fixture = output_directory.join(format!("{base}.jsonl"));
    let labels = output_directory.join(format!("{base}.labels.jsonl"));
    let manifest = output_directory.join(format!("{base}.manifest.json"));
    let fixture_temporary = temporary_path(&fixture);
    let labels_temporary = temporary_path(&labels);
    let manifest_temporary = temporary_path(&manifest);
    for path in [&fixture_temporary, &labels_temporary, &manifest_temporary] {
        let _ = fs::remove_file(path);
    }

    let mut fixture_writer = BufWriter::new(File::create(&fixture_temporary)?);
    let mut label_writer = BufWriter::new(File::create(&labels_temporary)?);
    let mut span_count = 0_u64;
    let mut selected_trace_ids = Vec::with_capacity(selected.len());
    let mut split_counts = BTreeMap::<String, u64>::new();
    for record in &selected {
        let trace = trace_from_record(&source_manifest, tier, record);
        span_count = span_count.saturating_add(trace.spans.len() as u64);
        selected_trace_ids.push(trace.id.clone());
        serde_json::to_writer(&mut fixture_writer, &trace)?;
        fixture_writer.write_all(b"\n")?;
        let split = split_for_record(tier, record);
        *split_counts.entry(split.into()).or_default() += 1;
        let label = HeldOutLabel {
            trace_id: &trace.id,
            instance_id: &record.instance_id,
            trajectory_id: &record.trajectory_id,
            resolved: record.resolved,
            model: &record.model,
            group_key: repository_task_group(&record.instance_id),
            split,
        };
        serde_json::to_writer(&mut label_writer, &label)?;
        label_writer.write_all(b"\n")?;
    }
    sync_writer(fixture_writer)?;
    sync_writer(label_writer)?;
    guard_fixture(&fixture_temporary)?;
    let fixture_sha256 = sha256_file(&fixture_temporary)?;
    let labels_sha256 = sha256_file(&labels_temporary)?;
    let resolved = selected.iter().filter(|record| record.resolved).count() as u64;
    let fixture_manifest = FixtureManifest {
        schema_version: "perseval.benchmark_fixture_manifest.v2",
        dataset: source_manifest.dataset.clone(),
        revision: source_manifest.revision.clone(),
        source_artifact: source_manifest.artifact.clone(),
        source_sha256: source_manifest.sha256.clone(),
        tier: tier.name.clone(),
        fixture_schema_version: source_manifest.fixture_schema_version.clone(),
        selection_schema_version: source_manifest.selection_schema_version.clone(),
        selection: tier.selection.clone(),
        ground_truth_policy: "labels sidecar never enters fixture, OTLP, or product workspace",
        traces: selected.len() as u64,
        spans: span_count,
        resolved,
        unresolved: selected.len() as u64 - resolved,
        fixture_sha256: fixture_sha256.clone(),
        labels_sha256: labels_sha256.clone(),
        selected_trace_ids,
        split_counts,
    };
    let mut manifest_writer = BufWriter::new(File::create(&manifest_temporary)?);
    serde_json::to_writer_pretty(&mut manifest_writer, &fixture_manifest)?;
    manifest_writer.write_all(b"\n")?;
    sync_writer(manifest_writer)?;
    let manifest_sha256 = sha256_file(&manifest_temporary)?;

    fs::rename(fixture_temporary, &fixture)?;
    fs::rename(labels_temporary, &labels)?;
    fs::rename(manifest_temporary, &manifest)?;
    Ok(FixtureBuildReport {
        schema_version: "perseval.benchmark_fixture_build_report.v2",
        tier: tier.name.clone(),
        fixture,
        labels,
        manifest,
        traces: selected.len() as u64,
        spans: span_count,
        fixture_sha256,
        labels_sha256,
        manifest_sha256,
    })
}

fn read_source(source: &Path) -> Result<Vec<SourceRecord>, Box<dyn Error>> {
    let source = source
        .to_str()
        .ok_or_else(|| format!("source path is not UTF-8: {}", source.display()))?;
    let connection = Connection::open_in_memory()?;
    let mut statement = connection.prepare(
        "SELECT instance_id, resolved, model, traj_id, patch, to_json(messages)
         FROM read_parquet(?1)",
    )?;
    statement
        .query_map([source], |row| {
            let trajectory_id: String = row.get(3)?;
            let messages_json: String = row.get(5)?;
            let messages = serde_json::from_str(&messages_json).map_err(|error| {
                duckdb::Error::FromSqlConversionFailure(
                    messages_json.len(),
                    duckdb::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(SourceRecord {
                instance_id: row.get(0)?,
                resolved: row.get(1)?,
                model: row.get(2)?,
                selection_hash: hex::encode(Sha256::digest(trajectory_id.as_bytes())),
                trajectory_id,
                patch: row.get(4)?,
                messages,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn validate_source_counts(
    manifest: &SourceManifest,
    records: &[SourceRecord],
) -> Result<(), Box<dyn Error>> {
    let resolved = records.iter().filter(|record| record.resolved).count() as u64;
    let unresolved = records.len() as u64 - resolved;
    if records.len() as u64 != manifest.rows
        || resolved != manifest.resolved_rows
        || unresolved != manifest.unresolved_rows
    {
        return Err(format!(
            "source counts differ from manifest: rows={} resolved={} unresolved={}",
            records.len(),
            resolved,
            unresolved
        )
        .into());
    }
    Ok(())
}

fn select_records<'a>(
    tier: &BenchmarkTier,
    records: &'a [SourceRecord],
) -> Result<Vec<&'a SourceRecord>, Box<dyn Error>> {
    let (Some(resolved_limit), Some(unresolved_limit)) = (tier.resolved, tier.unresolved) else {
        return Ok(records.iter().collect());
    };
    let mut resolved = 0_u64;
    let mut unresolved = 0_u64;
    let mut selected = Vec::with_capacity((resolved_limit + unresolved_limit) as usize);
    for record in records {
        let include = if record.resolved {
            if resolved == resolved_limit {
                false
            } else {
                resolved += 1;
                true
            }
        } else if unresolved == unresolved_limit {
            false
        } else {
            unresolved += 1;
            true
        };
        if include {
            selected.push(record);
        }
        if resolved == resolved_limit && unresolved == unresolved_limit {
            break;
        }
    }
    if resolved != resolved_limit || unresolved != unresolved_limit {
        return Err(format!(
            "tier {:?} requested {resolved_limit} resolved and {unresolved_limit} unresolved rows, but the source could not satisfy it",
            tier.name
        )
        .into());
    }
    selected.sort_by(|left, right| left.selection_hash.cmp(&right.selection_hash));
    Ok(selected)
}

fn trace_from_record(
    source: &SourceManifest,
    tier: &BenchmarkTier,
    record: &SourceRecord,
) -> Trace {
    let trace_id = format!("swesmith-{}", &record.selection_hash[..24]);
    let root_id = format!("{trace_id}-root");
    let input = record
        .messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| message.content.clone());
    let system_prompt = record
        .messages
        .iter()
        .find(|message| message.role == "system")
        .map(|message| message.content.clone());
    let mut root = Span::new(&root_id, "coding_agent.run").with_kind(SpanKind::Agent);
    root.trace_id = Some(trace_id.clone());
    root.input = input.clone();
    root.output = record
        .patch
        .as_deref()
        .filter(|patch| !patch.is_empty())
        .map(bounded_payload);
    root.started_at = Some(ROOT_START_NANO.to_string());
    root.ended_at = Some(
        ROOT_START_NANO
            .saturating_add(
                record
                    .messages
                    .len()
                    .saturating_mul(TURN_STRIDE_NANO as usize) as u64,
            )
            .to_string(),
    );
    root.attributes.insert(
        "agent.system_prompt".into(),
        system_prompt.map_or(Value::Null, Value::String),
    );
    root.attributes.insert(
        "benchmark.label_visibility".into(),
        json!("withheld-sidecar"),
    );
    root.attributes
        .insert("dataset.name".into(), json!(source.dataset));
    root.attributes
        .insert("dataset.revision".into(), json!(source.revision));
    root.attributes
        .insert("gen_ai.request.model".into(), json!(record.model));
    root.attributes
        .insert("source.instance_id".into(), json!(record.instance_id));
    root.attributes
        .insert("source.trajectory_id".into(), json!(record.trajectory_id));

    let mut trace = Trace::new(&trace_id).with_span(root);
    trace
        .metadata
        .insert("dataset_revision".into(), json!(source.revision));
    trace.metadata.insert(
        "fixture_kind".into(),
        json!(if tier.name == "balanced-100" {
            "balanced labeled benchmark; label withheld".to_owned()
        } else {
            format!("{} benchmark; label withheld", tier.name)
        }),
    );
    trace.metadata.insert("model".into(), json!(record.model));
    trace.metadata.insert(
        "source".into(),
        json!(format!("Hugging Face: {}", source.dataset)),
    );
    trace
        .metadata
        .insert("title".into(), json!(record.instance_id));

    let mut planner_index = 0_u64;
    let mut first_planner = true;
    for (message_index, message) in record.messages.iter().enumerate() {
        if message.role != "assistant" {
            continue;
        }
        planner_index += 1;
        let planner_id = format!("{trace_id}-planner-{planner_index:03}");
        let planner_start = ROOT_START_NANO + planner_index * TURN_STRIDE_NANO;
        let mut planner = Span::new(&planner_id, format!("planner.step.{planner_index}"))
            .with_kind(SpanKind::Llm);
        planner.trace_id = Some(trace_id.clone());
        planner.parent_id = Some(root_id.clone());
        planner.input = first_planner.then(|| input.clone()).flatten();
        first_planner = false;
        planner.output = Some(planner_output(&message.content));
        planner.started_at = Some(planner_start.to_string());
        planner.ended_at = Some((planner_start + SPAN_DURATION_NANO).to_string());
        planner
            .attributes
            .insert("agent.role".into(), json!("planner"));
        planner
            .attributes
            .insert("gen_ai.operation.name".into(), json!("chat"));
        planner
            .attributes
            .insert("source.message_index".into(), json!(message_index as u64));
        trace.spans.push(planner);

        let Some(call) = parse_tool_call(&message.content) else {
            continue;
        };
        let observation = record
            .messages
            .get(message_index + 1)
            .filter(|message| message.role == "user")
            .map(|message| message.content.clone());
        let verifier = call.name == "submit";
        let tool_id = format!(
            "{trace_id}-{}-{planner_index:03}-01",
            if verifier { "verifier" } else { "terminal" }
        );
        let mut tool = Span::new(
            tool_id,
            format!(
                "{}.{}",
                if verifier { "verifier" } else { "terminal" },
                call.name
            ),
        )
        .with_kind(SpanKind::Tool);
        tool.trace_id = Some(trace_id.clone());
        tool.parent_id = Some(planner_id);
        tool.input = call.parameters_json().as_deref().map(bounded_payload);
        tool.output = observation.as_deref().map(bounded_payload);
        tool.error = observation
            .as_deref()
            .filter(|output| observation_indicates_failure(output))
            .map(|_| "tool observation indicates failure".into());
        tool.started_at = Some((planner_start + 5_000_000).to_string());
        tool.ended_at = Some((planner_start + 9_000_000).to_string());
        tool.attributes.insert(
            "agent.role".into(),
            json!(if verifier { "verifier" } else { "terminal" }),
        );
        tool.attributes.insert("tool.name".into(), json!(call.name));
        tool.attributes.insert(
            "source.message_index".into(),
            json!((message_index + 1) as u64),
        );
        trace.spans.push(tool);
    }
    trace
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedToolCall {
    name: String,
    parameters: Vec<(String, String)>,
}

impl ParsedToolCall {
    fn parameters_json(&self) -> Option<String> {
        (!self.parameters.is_empty()).then(|| {
            let encoded = self
                .parameters
                .iter()
                .map(|(name, value)| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(name).expect("parameter name serializes"),
                        serde_json::to_string(value).expect("parameter value serializes")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{encoded}}}")
        })
    }
}

fn parse_tool_call(content: &str) -> Option<ParsedToolCall> {
    let function_start = content.find("<function=")? + "<function=".len();
    let function_end = content[function_start..].find('>')? + function_start;
    let name = content[function_start..function_end].trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let block_end = content[function_end + 1..]
        .find("</function>")
        .map(|offset| function_end + 1 + offset)
        .unwrap_or(content.len());
    let block = &content[function_end + 1..block_end];
    let mut parameters = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = block[cursor..].find("<parameter=") {
        let start = cursor + relative_start + "<parameter=".len();
        let name_end = block[start..].find('>')? + start;
        let value_start = name_end + 1;
        let value_end = block[value_start..].find("</parameter>")? + value_start;
        parameters.push((
            block[start..name_end].trim().to_owned(),
            block[value_start..value_end].trim().to_owned(),
        ));
        cursor = value_end + "</parameter>".len();
    }
    Some(ParsedToolCall { name, parameters })
}

fn planner_output(content: &str) -> String {
    let Some(function_start) = content.find("<function=") else {
        return content.to_owned();
    };
    let reasoning = content[..function_start].trim();
    if reasoning.is_empty() {
        content.to_owned()
    } else {
        reasoning.to_owned()
    }
}

fn observation_indicates_failure(output: &str) -> bool {
    let lowercase = output.to_ascii_lowercase();
    lowercase.contains("error:")
        || lowercase.contains("traceback (most recent call last)")
        || lowercase.contains("no such file or directory")
        || lowercase.contains("❌ test failed")
        || lowercase.contains("✗ union type test failed")
        || lowercase.contains("✗ variable arguments test failed")
}

fn bounded_payload(value: &str) -> String {
    if value.chars().count() <= MAXIMUM_PAYLOAD_CHARACTERS {
        return value.to_owned();
    }
    let mut bounded = value
        .chars()
        .take(MAXIMUM_PAYLOAD_CHARACTERS)
        .collect::<String>();
    bounded.push_str("\n…[truncated]");
    bounded
}

fn repository_task_group(instance_id: &str) -> String {
    instance_id
        .split('.')
        .next()
        .unwrap_or(instance_id)
        .to_owned()
}

fn split_for_record(tier: &BenchmarkTier, record: &SourceRecord) -> &'static str {
    if tier.name != "full" {
        return "qualification";
    }
    let group = repository_task_group(&record.instance_id);
    let digest = Sha256::digest(group.as_bytes());
    match u16::from_be_bytes([digest[0], digest[1]]) % 10 {
        0 => "test",
        1 => "validation",
        _ => "train",
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.partial-{}", std::process::id()))
}

fn sync_writer(mut writer: BufWriter<File>) -> Result<(), Box<dyn Error>> {
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn parses_tool_call_parameters_in_source_order() {
        let parsed = parse_tool_call(
            "reason\n<function=str_replace_editor>\n<parameter=command>create</parameter>\n<parameter=path>/tmp/file</parameter>\n<parameter=file_text>hello\nworld</parameter>\n</function>",
        )
        .unwrap();

        assert_eq!(parsed.name, "str_replace_editor");
        assert_eq!(
            parsed.parameters_json().as_deref(),
            Some(
                "{\"command\": \"create\", \"path\": \"/tmp/file\", \"file_text\": \"hello\\nworld\"}"
            )
        );
    }

    #[test]
    fn planner_output_keeps_reasoning_but_preserves_call_only_messages() {
        assert_eq!(
            planner_output("reason\n<function=bash>\n</function>"),
            "reason"
        );
        assert_eq!(
            planner_output("<function=submit>\n</function>"),
            "<function=submit>\n</function>"
        );
    }

    #[test]
    fn full_split_is_stable_for_a_repository_group() {
        let record = SourceRecord {
            instance_id: "owner__repo.commit.task__id".into(),
            resolved: true,
            model: "model".into(),
            trajectory_id: "trajectory".into(),
            patch: None,
            messages: Vec::new(),
            selection_hash: "0".repeat(64),
        };
        let tier = BenchmarkTier {
            name: "full".into(),
            purpose: "test".into(),
            resolved: None,
            unresolved: None,
            selection: "all".into(),
        };

        assert_eq!(split_for_record(&tier, &record), "test");
    }

    #[test]
    fn selected_trace_ids_are_stable_hashes() {
        let source = SourceManifest {
            schema_version: crate::manifest::SOURCE_MANIFEST_SCHEMA_VERSION.into(),
            dataset: "dataset".into(),
            revision: "revision".into(),
            artifact: "artifact".into(),
            url: "https://example.test".into(),
            sha256: "0".repeat(64),
            rows: 1,
            resolved_rows: 1,
            unresolved_rows: 0,
            fixture_schema_version: "fixture".into(),
            selection_schema_version: "selection".into(),
            tiers: Vec::new(),
        };
        let tier = BenchmarkTier {
            name: "ci".into(),
            purpose: "test".into(),
            resolved: Some(1),
            unresolved: Some(0),
            selection: "hash".into(),
        };
        let trajectory_id = "encode__starlette.db5063c2.func_pm_remove_loop__d5nfkox0.lg7bwtbd";
        let record = SourceRecord {
            instance_id: "instance".into(),
            resolved: true,
            model: "model".into(),
            trajectory_id: trajectory_id.into(),
            patch: None,
            messages: Vec::new(),
            selection_hash: hex::encode(Sha256::digest(trajectory_id.as_bytes())),
        };

        assert_eq!(
            trace_from_record(&source, &tier, &record).id,
            "swesmith-003079c48b2e4f19a2aba236"
        );
    }

    #[test]
    fn selected_records_do_not_duplicate_inputs() {
        let records = (0..4)
            .map(|index| SourceRecord {
                instance_id: format!("instance-{index}"),
                resolved: index % 2 == 0,
                model: "model".into(),
                trajectory_id: format!("trajectory-{index}"),
                patch: None,
                messages: Vec::new(),
                selection_hash: format!("{index:064}"),
            })
            .collect::<Vec<_>>();
        let tier = BenchmarkTier {
            name: "ci".into(),
            purpose: "test".into(),
            resolved: Some(1),
            unresolved: Some(1),
            selection: "hash".into(),
        };

        let selected = select_records(&tier, &records).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected
                .iter()
                .map(|record| record.trajectory_id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
    }
}

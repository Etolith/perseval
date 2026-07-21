use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use traces_to_evals::{
    BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION,
    COMPACT_TASK_COMPLETION_PROJECTION_SCHEMA_VERSION, CompactTaskCompletionProjectionStatsV1,
    CompactTaskCompletionProjectionV1, CompactTaskCompletionTokenBudgetV1,
    CompactTaskCompletionVariantV1, EvaluationEvidenceCatalogV1, EvaluationEvidenceKindV1,
    EvaluationEvidenceLocationV1, EvaluationEvidenceRecordV1, TaskCompletionEvidenceLaneV1,
    TaskCompletionGoalBundleV1, TaskCompletionRecoveryChainV1, TaskCompletionTraceFactV1,
    TraceFactActorV1, TraceFactKindV1, TraceFactStatusV1, canonical_content_id,
};

use crate::task_completion::{SuiteRecord, SuiteSpan, load_selected_suite};

const PROJECTOR_VERSION: &str = "perseval.compact-task-completion-projector.v2";
const MAX_INPUT_TOKENS: u32 = 6_144;
const RUBRIC: &str = "Judge whether the active user request was completed from the supplied trace facts. Choose A only when the requested outcome and verification are supported by evidence. Choose B when work is incomplete, failed, abandoned, contradicted, or unsupported. A final assistant claim is not proof. Errors followed by related recovery evidence must be considered together. Return exactly one label: A = completed, B = incomplete.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectionReport {
    schema_version: String,
    decision_schema_version: String,
    projector_version: String,
    split: String,
    variant: CompactTaskCompletionVariantV1,
    tokenizer_id: String,
    selected_traces: u64,
    projected_traces: u64,
    over_budget_traces: u64,
    mandatory_facts_omitted: u64,
    maximum_projected_tokens: u32,
    output: String,
    held_out_labels_in_projection: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    fact: TaskCompletionTraceFactV1,
    record: EvaluationEvidenceRecordV1,
    relevance: u64,
    family: String,
}

#[async_trait]
trait TokenCounter: Send + Sync {
    fn tokenizer_id(&self) -> &str;
    async fn count_tokens(&self, text: &str) -> Result<u32, Box<dyn Error>>;
}

#[derive(Clone)]
struct LlamaTokenCounter {
    client: Client,
    endpoint: String,
    tokenizer_id: String,
}

impl LlamaTokenCounter {
    fn from_environment() -> Result<Self, Box<dyn Error>> {
        let base_url = std::env::var("PERSEVAL_CHAT_BASE_URL")
            .map_err(|_| "PERSEVAL_CHAT_BASE_URL is required for exact tokenization")?;
        let base_url = base_url.trim().trim_end_matches('/');
        if base_url.is_empty() {
            return Err("PERSEVAL_CHAT_BASE_URL must not be empty".into());
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()?,
            endpoint: format!("{base_url}/tokenize"),
            tokenizer_id: std::env::var("PERSEVAL_TOKENIZER_ID")
                .unwrap_or_else(|_| "llama.cpp:/tokenize".into()),
        })
    }
}

#[derive(Deserialize)]
struct TokenizeResponse {
    tokens: Vec<Value>,
}

#[async_trait]
impl TokenCounter for LlamaTokenCounter {
    fn tokenizer_id(&self) -> &str {
        &self.tokenizer_id
    }

    async fn count_tokens(&self, text: &str) -> Result<u32, Box<dyn Error>> {
        let response = self
            .client
            .post(&self.endpoint)
            .json(&json!({"content": text, "add_special": true}))
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes)
                .chars()
                .take(500)
                .collect::<String>();
            return Err(format!("tokenizer returned HTTP {status}: {body}").into());
        }
        let response: TokenizeResponse = serde_json::from_slice(&bytes)?;
        u32::try_from(response.tokens.len()).map_err(Into::into)
    }
}

pub async fn project_suite(
    suite: &Path,
    labels: &Path,
    split: &str,
    output: &Path,
    variant: &str,
    limit: Option<usize>,
) -> Result<ProjectionReport, Box<dyn Error>> {
    if !suite.is_file() || !labels.is_file() {
        return Err("trace suite and label sidecar must exist".into());
    }
    if limit == Some(0) {
        return Err("limit must be greater than zero".into());
    }
    let variant = parse_variant(variant)?;
    let records = load_selected_suite(suite, labels, split, limit)?;
    if records.is_empty() {
        return Err(format!("no suite records selected for split {split:?}").into());
    }
    let tokenizer = LlamaTokenCounter::from_environment()?;
    let file = File::create(output)?;
    let mut writer = BufWriter::new(file);
    let mut projected = 0_u64;
    let mut over_budget = 0_u64;
    let mut mandatory_omitted = 0_u64;
    let mut maximum_tokens = 0_u32;
    for record in &records {
        match project_record(record, variant, &tokenizer).await {
            Ok(projection) => {
                maximum_tokens = maximum_tokens.max(projection.token_budget.projected_tokens);
                mandatory_omitted = mandatory_omitted
                    .saturating_add(u64::from(projection.stats.mandatory_facts_omitted));
                serde_json::to_writer(&mut writer, &projection)?;
                writer.write_all(b"\n")?;
                projected += 1;
            }
            Err(error) if error.to_string().contains("exceeds the 6144-token budget") => {
                over_budget += 1;
            }
            Err(error) => return Err(error),
        }
    }
    writer.flush()?;
    Ok(ProjectionReport {
        schema_version: COMPACT_TASK_COMPLETION_PROJECTION_SCHEMA_VERSION.into(),
        decision_schema_version: BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION.into(),
        projector_version: PROJECTOR_VERSION.into(),
        split: split.into(),
        variant,
        tokenizer_id: tokenizer.tokenizer_id().into(),
        selected_traces: records.len() as u64,
        projected_traces: projected,
        over_budget_traces: over_budget,
        mandatory_facts_omitted: mandatory_omitted,
        maximum_projected_tokens: maximum_tokens,
        output: output.display().to_string(),
        held_out_labels_in_projection: false,
    })
}

fn parse_variant(value: &str) -> Result<CompactTaskCompletionVariantV1, Box<dyn Error>> {
    match value {
        "goal-final" => Ok(CompactTaskCompletionVariantV1::GoalAndFinalResponse),
        "mandatory" => Ok(CompactTaskCompletionVariantV1::MandatoryEvidence),
        "mandatory-recovery" => Ok(CompactTaskCompletionVariantV1::MandatoryWithRecovery),
        "complete" => Ok(CompactTaskCompletionVariantV1::Complete),
        _ => Err(format!(
            "unknown compact projection variant {value:?}; expected goal-final, mandatory, mandatory-recovery, or complete"
        )
        .into()),
    }
}

async fn project_record<C: TokenCounter>(
    record: &SuiteRecord,
    variant: CompactTaskCompletionVariantV1,
    tokenizer: &C,
) -> Result<CompactTaskCompletionProjectionV1, Box<dyn Error>> {
    let target_revision = format!("{}:schema-{}", record.suite_version, record.schema_version);
    let binding_id = canonical_content_id(
        "perseval.compact-task-completion-binding.v1",
        &json!({"target_key": record.sample_id, "target_revision": target_revision}),
    )?;
    let request = bounded_text(
        record
            .root
            .input
            .as_deref()
            .unwrap_or("Task intent unavailable."),
        2_400,
    );
    let goal_words = lexical_words(&request);
    let mut candidates = normalize_facts(record, &target_revision, &goal_words)?;
    let original_tokens = tokenizer
        .count_tokens(&serde_json::to_string(record)?)
        .await?;
    let mandatory_total = candidates
        .iter()
        .filter(|candidate| candidate.fact.mandatory)
        .count();
    let all_count = candidates.len();
    let all_chains = recovery_chains(&candidates);
    let chain_evidence = all_chains
        .iter()
        .flat_map(|chain| chain.evidence_ids.iter().cloned())
        .collect::<BTreeSet<_>>();

    let mut selected = Vec::new();
    for candidate in candidates.drain(..) {
        let include = match variant {
            CompactTaskCompletionVariantV1::GoalAndFinalResponse => {
                candidate.fact.kind == TraceFactKindV1::UserRequest
                    || candidate.fact.lane == TaskCompletionEvidenceLaneV1::FinalResponse
            }
            CompactTaskCompletionVariantV1::MandatoryEvidence => candidate.fact.mandatory,
            CompactTaskCompletionVariantV1::MandatoryWithRecovery => {
                candidate.fact.mandatory || chain_evidence.contains(&candidate.fact.evidence_id)
            }
            CompactTaskCompletionVariantV1::Complete => true,
        };
        if include {
            selected.push(candidate);
        }
    }
    if variant == CompactTaskCompletionVariantV1::Complete {
        selected.sort_by_key(|candidate| {
            (
                Reverse(candidate.fact.mandatory),
                Reverse(candidate.relevance),
                candidate.fact.sequence,
            )
        });
        deduplicate_optional_facts(&mut selected);
    }
    selected.sort_by_key(|candidate| candidate.fact.sequence);

    let mut projection = assemble_projection(
        record,
        target_revision,
        binding_id,
        request,
        variant,
        all_count,
        mandatory_total,
        original_tokens,
        selected,
        all_chains,
        tokenizer,
    )
    .await?;
    while projection.token_budget.projected_tokens > MAX_INPUT_TOKENS {
        let Some(index) = projection.facts.iter().rposition(|fact| !fact.mandatory) else {
            return Err(format!(
                "trace {} mandatory evidence exceeds the 6144-token budget",
                record.sample_id
            )
            .into());
        };
        projection.facts.remove(index);
        projection.recovery_chains.retain(|chain| {
            chain
                .evidence_ids
                .iter()
                .all(|id| projection.facts.iter().any(|fact| &fact.evidence_id == id))
        });
        projection = retokenize_projection(
            projection,
            tokenizer,
            all_count,
            mandatory_total,
            original_tokens,
        )
        .await?;
    }
    projection.seal().map_err(Into::into)
}

fn normalize_facts(
    record: &SuiteRecord,
    target_revision: &str,
    goal_words: &BTreeSet<String>,
) -> Result<Vec<Candidate>, Box<dyn Error>> {
    let mut output = Vec::new();
    if let Some(input) = record
        .root
        .input
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        push_segment_fact(
            &mut output,
            record,
            target_revision,
            "root",
            input,
            true,
            TraceFactActorV1::User,
            TraceFactKindV1::UserRequest,
            TaskCompletionEvidenceLaneV1::Mandatory,
            0,
            goal_words,
        )?;
    }
    if let Some(final_response) = record
        .root
        .output
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .filter(|value| !is_duplicated_tool_trajectory(value, &record.spans))
    {
        push_segment_fact(
            &mut output,
            record,
            target_revision,
            "root",
            final_response,
            false,
            TraceFactActorV1::Assistant,
            TraceFactKindV1::AssistantMessage,
            TaskCompletionEvidenceLaneV1::FinalResponse,
            u32::try_from(record.spans.len()).unwrap_or(u32::MAX),
            goal_words,
        )?;
    }
    let last_index = record.spans.len().saturating_sub(1);
    for (index, span) in record.spans.iter().enumerate() {
        let sequence = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let span_id = format!("span-{sequence:04}");
        let status = fact_status(span.status.as_deref());
        let is_tool = span
            .kind
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("tool"))
            || span.name.starts_with("execute_tool ");
        let searchable = format!(
            "{} {} {}",
            span.name,
            span.input.as_deref().unwrap_or_default(),
            span.output.as_deref().unwrap_or_default()
        );
        let kind = if is_tool && is_verification(&searchable) {
            TraceFactKindV1::Verification
        } else if is_tool && is_mutation(&searchable) {
            TraceFactKindV1::ArtifactMutation
        } else if is_tool && is_external_action(&searchable) {
            TraceFactKindV1::ExternalAction
        } else if is_tool {
            TraceFactKindV1::ToolResult
        } else if span.kind.as_deref() == Some("agent") {
            TraceFactKindV1::ChildAgentResult
        } else if span.kind.as_deref() == Some("llm") {
            TraceFactKindV1::AssistantMessage
        } else {
            TraceFactKindV1::ToolResult
        };
        let mandatory = status == TraceFactStatusV1::Failed
            || matches!(
                kind,
                TraceFactKindV1::Verification
                    | TraceFactKindV1::ArtifactMutation
                    | TraceFactKindV1::ExternalAction
                    | TraceFactKindV1::ChildAgentResult
            )
            || span.output.is_none()
            || index == last_index;
        let actor = if is_tool {
            TraceFactActorV1::Tool
        } else if kind == TraceFactKindV1::ChildAgentResult {
            TraceFactActorV1::ChildAgent
        } else {
            TraceFactActorV1::Assistant
        };
        let summary = span_summary(span);
        let evidence_key = canonical_content_id(
            "perseval.compact-task-completion-evidence.v1",
            &json!({
                "target_key": record.sample_id,
                "target_revision": target_revision,
                "span_id": span_id,
                "kind": kind,
                "summary": summary,
            }),
        )?;
        let evidence_id = format!("E{:04}", output.len() + 1);
        let relevance = relevance_score(&summary, goal_words, sequence, kind);
        let family = tool_family(&span.name, span.input.as_deref());
        output.push(Candidate {
            fact: TaskCompletionTraceFactV1 {
                evidence_id,
                evidence_key: evidence_key.clone(),
                sequence,
                actor,
                kind,
                status,
                lane: if mandatory {
                    TaskCompletionEvidenceLaneV1::Mandatory
                } else {
                    TaskCompletionEvidenceLaneV1::GoalRelevant
                },
                mandatory,
                span_id: Some(span_id.clone()),
                parent_span_id: Some("root".into()),
                tool_name: is_tool.then(|| span.name.clone()),
                summary,
                structured_facts: structured_facts(span),
                token_count: 1,
            },
            record: evidence_record(
                &record.sample_id,
                target_revision,
                &span_id,
                EvaluationEvidenceKindV1::Span,
                EvaluationEvidenceLocationV1::Span {
                    span_id: span_id.clone(),
                },
            ),
            relevance,
            family,
        });
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn push_segment_fact(
    output: &mut Vec<Candidate>,
    record: &SuiteRecord,
    target_revision: &str,
    span_id: &str,
    content: &str,
    input: bool,
    actor: TraceFactActorV1,
    kind: TraceFactKindV1,
    lane: TaskCompletionEvidenceLaneV1,
    sequence: u32,
    goal_words: &BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    let summary = bounded_text(content, 2_400);
    let evidence_key = canonical_content_id(
        "perseval.compact-task-completion-evidence.v1",
        &json!({
            "target_key": record.sample_id,
            "target_revision": target_revision,
            "span_id": span_id,
            "field": if input { "input" } else { "output" },
            "content": summary,
        }),
    )?;
    let end_byte = u32::try_from(content.len()).unwrap_or(u32::MAX).max(1);
    output.push(Candidate {
        fact: TaskCompletionTraceFactV1 {
            evidence_id: format!("E{:04}", output.len() + 1),
            evidence_key: evidence_key.clone(),
            sequence,
            actor,
            kind,
            status: TraceFactStatusV1::Succeeded,
            lane,
            mandatory: true,
            span_id: Some(span_id.into()),
            parent_span_id: None,
            tool_name: None,
            summary: summary.clone(),
            structured_facts: BTreeMap::new(),
            token_count: 1,
        },
        record: evidence_record(
            &record.sample_id,
            target_revision,
            span_id,
            if input {
                EvaluationEvidenceKindV1::InputSegment
            } else {
                EvaluationEvidenceKindV1::OutputSegment
            },
            EvaluationEvidenceLocationV1::Segment {
                span_id: span_id.into(),
                start_byte: 0,
                end_byte,
            },
        ),
        relevance: relevance_score(&summary, goal_words, sequence, kind),
        family: "root".into(),
    });
    Ok(())
}

fn evidence_record(
    target_key: &str,
    target_revision: &str,
    _span_id: &str,
    evidence_kind: EvaluationEvidenceKindV1,
    location: EvaluationEvidenceLocationV1,
) -> EvaluationEvidenceRecordV1 {
    EvaluationEvidenceRecordV1 {
        target_key: target_key.into(),
        target_revision: target_revision.into(),
        projection_hash: format!("sha256:{}", "0".repeat(64)),
        evidence_kind,
        location,
        applicable_criterion_ids: BTreeSet::new(),
    }
}

fn recovery_chains(candidates: &[Candidate]) -> Vec<TaskCompletionRecoveryChainV1> {
    let mut chains = Vec::new();
    for (index, failure) in candidates.iter().enumerate() {
        if failure.fact.status != TraceFactStatusV1::Failed {
            continue;
        }
        let Some(recovery) = candidates[index + 1..].iter().find(|candidate| {
            candidate.family == failure.family
                && candidate.fact.status == TraceFactStatusV1::Succeeded
        }) else {
            continue;
        };
        chains.push(TaskCompletionRecoveryChainV1 {
            chain_id: format!("recovery-{:04}", chains.len() + 1),
            evidence_ids: vec![
                failure.fact.evidence_id.clone(),
                recovery.fact.evidence_id.clone(),
            ],
            token_count: 1,
        });
    }
    chains
}

#[allow(clippy::too_many_arguments)]
async fn assemble_projection<C: TokenCounter>(
    record: &SuiteRecord,
    target_revision: String,
    binding_id: String,
    request: String,
    variant: CompactTaskCompletionVariantV1,
    all_count: usize,
    mandatory_total: usize,
    original_tokens: u32,
    selected: Vec<Candidate>,
    all_chains: Vec<TaskCompletionRecoveryChainV1>,
    tokenizer: &C,
) -> Result<CompactTaskCompletionProjectionV1, Box<dyn Error>> {
    let agent_context = string_list_attribute(&record.root.attributes, "perseval.agent.context");
    let selected_ids = selected
        .iter()
        .map(|candidate| candidate.fact.evidence_id.as_str())
        .collect::<BTreeSet<_>>();
    let recovery_chains = all_chains
        .into_iter()
        .filter(|chain| {
            chain
                .evidence_ids
                .iter()
                .all(|id| selected_ids.contains(id.as_str()))
        })
        .collect::<Vec<_>>();
    let facts = selected
        .iter()
        .map(|candidate| candidate.fact.clone())
        .collect::<Vec<_>>();
    let entries = selected
        .into_iter()
        .map(|candidate| (candidate.fact.evidence_key, candidate.record))
        .collect();
    let projection = CompactTaskCompletionProjectionV1 {
        schema_version: COMPACT_TASK_COMPLETION_PROJECTION_SCHEMA_VERSION.into(),
        projector_version: PROJECTOR_VERSION.into(),
        variant,
        target_key: record.sample_id.clone(),
        target_revision: target_revision.clone(),
        trace_context_binding_id: binding_id,
        context_release_id: None,
        context_projection_release_id: None,
        projection_hash: format!("sha256:{}", "0".repeat(64)),
        goal: TaskCompletionGoalBundleV1 {
            primary_request: request,
            amendments: Vec::new(),
            success_criteria: vec!["Fulfill the active user request.".into()],
            requested_side_effects: Vec::new(),
            requested_verification: Vec::new(),
            constraints: Vec::new(),
            agent_context: if agent_context.is_empty() {
                vec!["Agent operating in the task environment represented by this trace.".into()]
            } else {
                agent_context
            },
            superseded_requirements: Vec::new(),
            token_count: 1,
        },
        facts,
        recovery_chains,
        token_budget: empty_budget(tokenizer.tokenizer_id()),
        stats: CompactTaskCompletionProjectionStatsV1 {
            included_facts: 0,
            omitted_facts: 0,
            mandatory_facts: u32::try_from(mandatory_total)?,
            mandatory_facts_omitted: 0,
        },
        evidence_catalog: EvaluationEvidenceCatalogV1 {
            target_key: record.sample_id.clone(),
            target_revision,
            projection_hash: format!("sha256:{}", "0".repeat(64)),
            entries,
        },
    };
    retokenize_projection(
        projection,
        tokenizer,
        all_count,
        mandatory_total,
        original_tokens,
    )
    .await
}

async fn retokenize_projection<C: TokenCounter>(
    mut projection: CompactTaskCompletionProjectionV1,
    tokenizer: &C,
    all_count: usize,
    mandatory_total: usize,
    original_tokens: u32,
) -> Result<CompactTaskCompletionProjectionV1, Box<dyn Error>> {
    for fact in &mut projection.facts {
        fact.token_count = tokenizer.count_tokens(&render_fact(fact)).await?;
    }
    for chain in &mut projection.recovery_chains {
        chain.token_count = tokenizer.count_tokens(&render_chain(chain)).await?;
    }
    let sections = render_sections(&projection, all_count);
    let counts = cumulative_section_counts(tokenizer, &sections).await?;
    projection.goal.token_count = counts[1];
    projection.token_budget = CompactTaskCompletionTokenBudgetV1 {
        tokenizer_id: tokenizer.tokenizer_id().into(),
        max_input_tokens: MAX_INPUT_TOKENS,
        original_tokens,
        projected_tokens: counts.iter().sum(),
        rubric_tokens: counts[0],
        goal_tokens: counts[1],
        final_response_tokens: counts[2],
        mandatory_tokens: counts[3],
        recovery_tokens: counts[4],
        goal_relevant_tokens: counts[5],
        metadata_tokens: counts[6],
    };
    let included_mandatory = projection
        .facts
        .iter()
        .filter(|fact| fact.mandatory)
        .count();
    projection.stats = CompactTaskCompletionProjectionStatsV1 {
        included_facts: u32::try_from(projection.facts.len())?,
        omitted_facts: u32::try_from(all_count.saturating_sub(projection.facts.len()))?,
        mandatory_facts: u32::try_from(mandatory_total)?,
        mandatory_facts_omitted: u32::try_from(mandatory_total.saturating_sub(included_mandatory))?,
    };
    Ok(projection)
}

fn empty_budget(tokenizer_id: &str) -> CompactTaskCompletionTokenBudgetV1 {
    CompactTaskCompletionTokenBudgetV1 {
        tokenizer_id: tokenizer_id.into(),
        max_input_tokens: MAX_INPUT_TOKENS,
        original_tokens: 0,
        projected_tokens: 0,
        rubric_tokens: 0,
        goal_tokens: 0,
        final_response_tokens: 0,
        mandatory_tokens: 0,
        recovery_tokens: 0,
        goal_relevant_tokens: 0,
        metadata_tokens: 0,
    }
}

fn render_sections(
    projection: &CompactTaskCompletionProjectionV1,
    all_count: usize,
) -> [String; 7] {
    let render_lane = |lane| {
        projection
            .facts
            .iter()
            .filter(|fact| fact.lane == lane)
            .map(render_fact)
            .collect::<Vec<_>>()
            .join("\n")
    };
    [
        format!("<rubric>\n{RUBRIC}\n</rubric>\n"),
        format!(
            "<goal>\nPrimary request: {}\nSuccess criteria:\n{}\nAgent context:\n{}\n</goal>\n",
            projection.goal.primary_request,
            render_list(&projection.goal.success_criteria),
            render_list(&projection.goal.agent_context),
        ),
        format!(
            "<final_response>\n{}\n</final_response>\n",
            render_lane(TaskCompletionEvidenceLaneV1::FinalResponse)
        ),
        format!(
            "<mandatory_evidence>\n{}\n</mandatory_evidence>\n",
            render_lane(TaskCompletionEvidenceLaneV1::Mandatory)
        ),
        format!(
            "<failure_recovery>\n{}\n</failure_recovery>\n",
            projection
                .recovery_chains
                .iter()
                .map(render_chain)
                .collect::<Vec<_>>()
                .join("\n")
        ),
        format!(
            "<goal_relevant>\n{}\n</goal_relevant>\n",
            render_lane(TaskCompletionEvidenceLaneV1::GoalRelevant)
        ),
        format!(
            "<projection_metadata>variant={:?}; included_facts={}; original_facts={}; omitted_facts={}</projection_metadata>\nDecision:",
            projection.variant,
            projection.facts.len(),
            all_count,
            all_count.saturating_sub(projection.facts.len())
        ),
    ]
}

fn render_list(values: &[String]) -> String {
    if values.is_empty() {
        return "- Unspecified.".into();
    }
    values
        .iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn string_list_attribute(attributes: &BTreeMap<String, Value>, key: &str) -> Vec<String> {
    match attributes.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => vec![value.trim().into()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(Into::into)
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn render_projection_prompt(projection: &CompactTaskCompletionProjectionV1) -> String {
    let all_count = projection
        .stats
        .included_facts
        .saturating_add(projection.stats.omitted_facts) as usize;
    render_sections(projection, all_count).concat()
}

pub(super) fn render_projection_evidence(projection: &CompactTaskCompletionProjectionV1) -> String {
    let all_count = projection
        .stats
        .included_facts
        .saturating_add(projection.stats.omitted_facts) as usize;
    let sections = render_sections(projection, all_count);
    sections[1..]
        .join("")
        .trim_end_matches("Decision:")
        .trim_end()
        .to_string()
}

async fn cumulative_section_counts<C: TokenCounter>(
    tokenizer: &C,
    sections: &[String; 7],
) -> Result<[u32; 7], Box<dyn Error>> {
    let mut prefix = String::new();
    let mut previous = 0_u32;
    let mut output = [0_u32; 7];
    for (index, section) in sections.iter().enumerate() {
        prefix.push_str(section);
        let count = tokenizer.count_tokens(&prefix).await?;
        output[index] = count.saturating_sub(previous);
        previous = count;
    }
    Ok(output)
}

fn render_fact(fact: &TaskCompletionTraceFactV1) -> String {
    format!(
        "[{}] actor={:?} kind={:?} status={:?} tool={} :: {}",
        fact.evidence_id,
        fact.actor,
        fact.kind,
        fact.status,
        fact.tool_name.as_deref().unwrap_or("none"),
        fact.summary
    )
}

fn render_chain(chain: &TaskCompletionRecoveryChainV1) -> String {
    format!("{}: {}", chain.chain_id, chain.evidence_ids.join(" -> "))
}

fn is_duplicated_tool_trajectory(value: &str, spans: &[SuiteSpan]) -> bool {
    !spans.is_empty()
        && value.trim_start().starts_with('[')
        && value.contains("\"tool\"")
        && value.contains("\"arguments\"")
}

fn deduplicate_optional_facts(candidates: &mut Vec<Candidate>) {
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| {
        candidate.fact.mandatory || seen.insert(candidate.fact.summary.to_ascii_lowercase())
    });
}

fn relevance_score(
    text: &str,
    goal_words: &BTreeSet<String>,
    sequence: u32,
    kind: TraceFactKindV1,
) -> u64 {
    let overlap = lexical_words(text).intersection(goal_words).count() as u64;
    let structural = u64::from(matches!(
        kind,
        TraceFactKindV1::Verification
            | TraceFactKindV1::ArtifactMutation
            | TraceFactKindV1::ExternalAction
    ));
    overlap
        .saturating_mul(100)
        .saturating_add(structural.saturating_mul(25))
        .saturating_add(u64::from(sequence))
}

fn lexical_words(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| word.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn fact_status(value: Option<&str>) -> TraceFactStatusV1 {
    match value.unwrap_or_default().to_ascii_lowercase().as_str() {
        "ok" | "success" | "succeeded" => TraceFactStatusV1::Succeeded,
        "error" | "failed" | "failure" => TraceFactStatusV1::Failed,
        "running" => TraceFactStatusV1::Running,
        "cancelled" | "canceled" => TraceFactStatusV1::Cancelled,
        _ => TraceFactStatusV1::Unknown,
    }
}

fn span_summary(span: &SuiteSpan) -> String {
    let input = bounded_text(span.input.as_deref().unwrap_or("none"), 700);
    let output = bounded_text(span.output.as_deref().unwrap_or("none"), 900);
    format!(
        "{}; status={}; input={input}; output={output}",
        span.name,
        span.status.as_deref().unwrap_or("unknown")
    )
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return value.into();
    }
    let head = max_chars.saturating_mul(2) / 3;
    let tail = max_chars.saturating_sub(head);
    let prefix = value.chars().take(head).collect::<String>();
    let suffix = value
        .chars()
        .rev()
        .take(tail)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{prefix}\n[content omitted; original trace retained]\n{suffix}")
}

fn tool_family(name: &str, input: Option<&str>) -> String {
    if let Some(path) = input.and_then(|value| {
        serde_json::from_str::<Value>(value)
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| extract_json_string(value, "path"))
    }) {
        return format!("artifact:{path}");
    }
    let command = input
        .unwrap_or_default()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    format!("{}:{command}", name.to_ascii_lowercase())
}

fn extract_json_string(value: &str, field: &str) -> Option<String> {
    let marker = format!("\"{field}\"");
    let remainder = value.split_once(&marker)?.1;
    let remainder = remainder.split_once(':')?.1.trim_start();
    let remainder = remainder.strip_prefix('"')?;
    let end = remainder.find('"')?;
    Some(remainder[..end].to_string())
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    let value = value.to_ascii_lowercase();
    needles.iter().any(|needle| value.contains(needle))
}

fn is_verification(value: &str) -> bool {
    contains_any(
        value,
        &[
            "cargo test",
            "pytest",
            "npm test",
            "pnpm test",
            "cargo clippy",
            "cargo check",
            "typecheck",
            " lint",
            " build",
        ],
    )
}

fn is_mutation(value: &str) -> bool {
    contains_any(
        value,
        &[
            "apply_patch",
            "write_file",
            "create_file",
            "text_editor:str_replace",
            "text_editor:create",
            "\"command\": \"str_replace\"",
            "\"command\": \"create\"",
            "git commit",
            "git push",
        ],
    )
}

fn is_external_action(value: &str) -> bool {
    contains_any(
        value,
        &["browser", "github", "pull request", "deploy", "send email"],
    )
}

fn structured_facts(span: &SuiteSpan) -> BTreeMap<String, Value> {
    let mut output = BTreeMap::new();
    if let Some(status) = &span.status {
        output.insert("status".into(), Value::String(status.clone()));
    }
    if let Some(duration_ms) = span.duration_ms {
        output.insert("duration_ms".into(), Value::from(duration_ms));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    struct WordCounter;

    #[async_trait]
    impl TokenCounter for WordCounter {
        fn tokenizer_id(&self) -> &str {
            "test-word-counter"
        }

        async fn count_tokens(&self, text: &str) -> Result<u32, Box<dyn Error>> {
            Ok(u32::try_from(text.split_whitespace().count())?)
        }
    }

    fn span(name: &str, status: &str, input: &str, output: &str) -> SuiteSpan {
        SuiteSpan {
            name: name.into(),
            kind: Some("tool".into()),
            status: Some(status.into()),
            duration_ms: Some(10),
            input: Some(input.into()),
            output: Some(output.into()),
            attributes: BTreeMap::new(),
        }
    }

    fn record() -> SuiteRecord {
        SuiteRecord {
            schema_version: 1,
            suite_version: "suite-v1".into(),
            sample_id: "trace-1".into(),
            root: SuiteSpan {
                name: "agent".into(),
                kind: Some("agent".into()),
                status: Some("ok".into()),
                duration_ms: None,
                input: Some("Fix authentication and run tests.".into()),
                output: Some("Authentication is fixed and tests pass.".into()),
                attributes: BTreeMap::new(),
            },
            spans: vec![
                span(
                    "execute_tool terminal",
                    "failed",
                    "cargo test -p auth",
                    "compile error",
                ),
                span(
                    "execute_tool apply_patch",
                    "ok",
                    "apply_patch auth.rs",
                    "done",
                ),
                span(
                    "execute_tool terminal",
                    "ok",
                    "cargo test -p auth",
                    "284 passed",
                ),
            ],
        }
    }

    #[tokio::test]
    async fn mandatory_projection_keeps_errors_mutations_and_verification() {
        let projection = project_record(
            &record(),
            CompactTaskCompletionVariantV1::MandatoryEvidence,
            &WordCounter,
        )
        .await
        .unwrap();
        assert_eq!(projection.stats.mandatory_facts_omitted, 0);
        assert!(projection.token_budget.projected_tokens <= MAX_INPUT_TOKENS);
        assert!(
            projection
                .facts
                .iter()
                .any(|fact| fact.status == TraceFactStatusV1::Failed)
        );
        projection.validate().unwrap();
    }

    #[tokio::test]
    async fn recovery_projection_preserves_failure_and_later_success_relationship() {
        let projection = project_record(
            &record(),
            CompactTaskCompletionVariantV1::MandatoryWithRecovery,
            &WordCounter,
        )
        .await
        .unwrap();
        assert_eq!(projection.recovery_chains.len(), 1);
        assert_eq!(projection.recovery_chains[0].evidence_ids.len(), 2);
    }

    #[tokio::test]
    async fn projection_uses_trace_specific_agent_context() {
        let mut input = record();
        input.root.attributes.insert(
            "perseval.agent.context".into(),
            json!([
                "Browser agent operating in WebArena.",
                "Success depends on web state."
            ]),
        );
        let projection = project_record(
            &input,
            CompactTaskCompletionVariantV1::MandatoryWithRecovery,
            &WordCounter,
        )
        .await
        .unwrap();
        assert_eq!(
            projection.goal.agent_context,
            vec![
                "Browser agent operating in WebArena.",
                "Success depends on web state."
            ]
        );
        let prompt = render_projection_prompt(&projection);
        assert!(prompt.contains("Browser agent operating in WebArena."));
        assert!(!prompt.contains("software-engineering repository agent"));
    }

    #[tokio::test]
    async fn goal_final_ablation_records_intentional_mandatory_omission() {
        let projection = project_record(
            &record(),
            CompactTaskCompletionVariantV1::GoalAndFinalResponse,
            &WordCounter,
        )
        .await
        .unwrap();
        assert!(projection.stats.mandatory_facts_omitted > 0);
        assert!(
            projection
                .facts
                .iter()
                .all(|fact| fact.kind == TraceFactKindV1::UserRequest
                    || fact.lane == TaskCompletionEvidenceLaneV1::FinalResponse)
        );
    }

    #[test]
    fn bounded_text_preserves_head_tail_and_discloses_omission() {
        let bounded = bounded_text(&"x".repeat(100), 12);
        assert!(bounded.starts_with("xxxxxxxx"));
        assert!(bounded.ends_with("xxxx"));
        assert!(bounded.contains("original trace retained"));
    }

    #[test]
    fn tool_family_recovers_artifact_from_truncated_json() {
        let input = r#"{"command":"create","path":"/app/main.go","file_text":"truncated"#;
        assert_eq!(
            tool_family("execute_tool text_editor:create", Some(input)),
            "artifact:/app/main.go"
        );
    }
}

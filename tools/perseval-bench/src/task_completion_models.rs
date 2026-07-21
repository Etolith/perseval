use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ort::{session::Session, value::Tensor};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use traces_to_evals::{
    BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION, BinaryTaskCompletionDecisionV1,
    BinaryTaskCompletionOutcomeV1, ChatClient, ChatRequest, CompactTaskCompletionProjectionV1,
    LearnedAbstentionReasonV1, OpenAiChatClient, ProviderExecutionFailureV1,
    ProviderResponseEnvelopeV1, ResponseSchema, TaskCompletionInferenceProvenanceV1,
    canonical_content_id,
};

use tokenizers::{EncodeInput, Tokenizer, TruncationParams, TruncationStrategy};

use crate::task_completion_compact::{render_projection_evidence, render_projection_prompt};

const RUN_SCHEMA_VERSION: &str = "perseval.task_completion_binary_run.v1";
const SCORE_SCHEMA_VERSION: &str = "perseval.task_completion_binary_score.v1";
const PROMPT_VERSION: &str = "perseval.binary-task-completion-ab-v1";
const CLOUD_PROMPT_VERSION: &str = "perseval.compact-task-completion-cloud-v1";
const NLI_PROMPT_VERSION: &str = "perseval.binary-task-completion-nli-v1";
const NLI_HYPOTHESIS: &str = "The agent completed every active requirement in the user's request, including required verification and external side effects.";
const NLI_MAX_TOKENS: usize = 2_048;
const COMPLETE_VERBALIZER: &str = " A";
const INCOMPLETE_VERBALIZER: &str = " B";
const F1_EXIT: f64 = 0.206;
const MCC_EXIT: f64 = 0.200;
const CLOUD_SYSTEM_PROMPT: &str = "Judge whether the active user request was completed from the supplied compact trace evidence. Completed requires evidence that every active requirement and required side effect was fulfilled. A final assistant claim is not proof. Consider an error together with any later recovery and verification. Choose incomplete when required work is missing, failed, abandoned, contradicted, or unsupported. Choose abstain only when the supplied projection genuinely lacks the evidence needed to decide. Cite at most three supplied evidence identifiers. Return probability_complete as your best probability that the task was completed; it must agree with the chosen label at threshold 0.5.";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelRunRecord {
    schema_version: String,
    target_key: String,
    #[serde(default)]
    mandatory_facts_omitted: u32,
    decision: BinaryTaskCompletionDecisionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nli_diagnostics: Option<NliDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_response: Option<ProviderResponseEnvelopeV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelRunReport {
    schema_version: String,
    model_id: String,
    model_hash: String,
    prompt_version: String,
    threshold: f64,
    selected_projections: u64,
    completed: u64,
    incomplete: u64,
    abstained: u64,
    summed_latency_ms: u64,
    output: String,
}

#[derive(Clone)]
struct SmolCompletionClient {
    client: Client,
    endpoint: String,
    model_id: String,
    model_hash: String,
    evaluator_release_id: String,
    threshold: f64,
}

#[derive(Clone)]
struct OpenAiCompletionClient {
    client: Arc<OpenAiChatClient>,
    model_id: String,
    evaluator_release_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CloudTaskCompletionLabel {
    Completed,
    Incomplete,
    Abstain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CloudTaskCompletionJudgment {
    label: CloudTaskCompletionLabel,
    probability_complete: f64,
    evidence_ids: Vec<String>,
    reason_code: String,
    explanation: String,
}

struct ModernBertNliClient {
    tokenizer: Tokenizer,
    session: Session,
    model_id: String,
    model_hash: String,
    tokenizer_hash: String,
    evaluator_release_id: String,
    threshold: f64,
    neutral_policy: NliNeutralPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NliLogits {
    entailment: f64,
    neutral: f64,
    contradiction: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NliDiagnostics {
    logits: NliLogits,
    probabilities: NliLogits,
    neutral_argmax: bool,
    input_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NliNeutralPolicy {
    Abstain,
    DiagnosticBinary,
}

impl NliNeutralPolicy {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "abstain" => Ok(Self::Abstain),
            "diagnostic-binary" => Ok(Self::DiagnosticBinary),
            _ => anyhow::bail!("neutral policy must be either abstain or diagnostic-binary"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Abstain => "abstain_if_argmax",
            Self::DiagnosticBinary => "diagnostic_binary_entailment_vs_contradiction",
        }
    }
}

impl SmolCompletionClient {
    fn from_environment(model_id: &str, model_hash: &str, threshold: f64) -> Result<Self> {
        validate_probability(threshold, "threshold")?;
        validate_sha256(model_hash, "model_hash")?;
        let base_url = std::env::var("PERSEVAL_CHAT_BASE_URL")
            .context("PERSEVAL_CHAT_BASE_URL is required for local inference")?;
        let base_url = base_url.trim().trim_end_matches('/');
        anyhow::ensure!(!base_url.is_empty(), "PERSEVAL_CHAT_BASE_URL is empty");
        let evaluator_release_id = canonical_content_id(
            "perseval.binary-task-completion-evaluator.v1",
            &json!({
                "model_id": model_id,
                "model_hash": model_hash,
                "prompt_version": PROMPT_VERSION,
                "verbalizers": [COMPLETE_VERBALIZER, INCOMPLETE_VERBALIZER],
                "decoding": {
                    "n_predict": 1,
                    "temperature": 0,
                    "top_k": 0,
                    "top_p": 1,
                    "min_p": 0,
                    "seed": 42,
                    "n_probs": 128,
                },
            }),
        )?;
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(600))
                .build()?,
            endpoint: format!("{base_url}/completion"),
            model_id: model_id.into(),
            model_hash: model_hash.into(),
            evaluator_release_id,
            threshold,
        })
    }

    async fn evaluate(
        &self,
        projection: CompactTaskCompletionProjectionV1,
    ) -> Result<ModelRunRecord> {
        projection.validate()?;
        let prompt = render_projection_prompt(&projection);
        let started = Instant::now();
        let inferred = self.infer(&prompt).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let inference = TaskCompletionInferenceProvenanceV1 {
            runtime: "llama.cpp-b9637".into(),
            model_id: self.model_id.clone(),
            model_hash: Some(self.model_hash.clone()),
            prompt_version: Some(PROMPT_VERSION.into()),
            tokenizer_id: projection.token_budget.tokenizer_id.clone(),
            input_tokens: projection.token_budget.projected_tokens,
            output_tokens: u32::from(inferred.is_ok()),
            latency_ms,
            cost_microusd: Some(0),
        };
        let (outcome, raw_logit_difference, probability_complete, abstention_reason, error) =
            match inferred {
                Ok(logits) => {
                    let probability = sigmoid(logits.complete - logits.incomplete);
                    let outcome = if probability >= self.threshold {
                        BinaryTaskCompletionOutcomeV1::Completed
                    } else {
                        BinaryTaskCompletionOutcomeV1::Incomplete
                    };
                    (
                        outcome,
                        Some(logits.complete - logits.incomplete),
                        Some(probability),
                        None,
                        None,
                    )
                }
                Err(error) => (
                    BinaryTaskCompletionOutcomeV1::Abstain,
                    None,
                    None,
                    Some(error.abstention_reason()),
                    Some(error.to_string()),
                ),
            };
        let decision = BinaryTaskCompletionDecisionV1 {
            schema_version: BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION.into(),
            evaluator_release_id: self.evaluator_release_id.clone(),
            target_key: projection.target_key.clone(),
            target_revision: projection.target_revision.clone(),
            trace_context_binding_id: projection.trace_context_binding_id.clone(),
            projection_hash: projection.projection_hash.clone(),
            outcome,
            raw_logit_difference,
            probability_complete,
            threshold: self.threshold,
            calibration_model_id: None,
            evidence_ids: Vec::new(),
            reason_code: None,
            explanation: None,
            abstention_reason,
            inference,
        };
        decision.validate_against(&projection)?;
        Ok(ModelRunRecord {
            schema_version: RUN_SCHEMA_VERSION.into(),
            target_key: projection.target_key,
            mandatory_facts_omitted: projection.stats.mandatory_facts_omitted,
            decision,
            error,
            nli_diagnostics: None,
            provider_response: None,
        })
    }

    async fn infer(&self, prompt: &str) -> std::result::Result<BinaryLogits, InferenceError> {
        let response = self
            .client
            .post(&self.endpoint)
            .json(&json!({
                "prompt": prompt,
                "n_predict": 1,
                "temperature": 0,
                "top_k": 0,
                "top_p": 1,
                "min_p": 0,
                "seed": 42,
                "n_probs": 128,
                "grammar": "root ::= \"A\" | \"B\"",
                "cache_prompt": true,
            }))
            .send()
            .await
            .map_err(|error| InferenceError::Unavailable(error.to_string()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| InferenceError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes)
                .chars()
                .take(500)
                .collect::<String>();
            return Err(InferenceError::Unavailable(format!(
                "llama.cpp returned HTTP {status}: {body}"
            )));
        }
        let response: CompletionResponse = serde_json::from_slice(&bytes)
            .map_err(|error| InferenceError::Invalid(error.to_string()))?;
        response.binary_logits()
    }
}

impl OpenAiCompletionClient {
    fn from_environment(model_id: &str) -> Result<Self> {
        anyhow::ensure!(
            std::env::var_os("OPENAI_API_KEY").is_some(),
            "OPENAI_API_KEY is required for cloud task-completion inference"
        );
        anyhow::ensure!(!model_id.trim().is_empty(), "model id must not be empty");
        let evaluator_release_id = canonical_content_id(
            "perseval.compact-task-completion-cloud-evaluator.v1",
            &json!({
                "model_id": model_id,
                "prompt_version": CLOUD_PROMPT_VERSION,
                "system_prompt": CLOUD_SYSTEM_PROMPT,
                "response_schema": cloud_response_schema().schema,
                "threshold": 0.5,
            }),
        )?;
        Ok(Self {
            client: Arc::new(OpenAiChatClient::from_env()),
            model_id: model_id.into(),
            evaluator_release_id,
        })
    }

    async fn evaluate(
        &self,
        projection: CompactTaskCompletionProjectionV1,
    ) -> Result<ModelRunRecord> {
        projection.validate()?;
        let request = ChatRequest {
            model: self.model_id.clone(),
            system_prompt: CLOUD_SYSTEM_PROMPT.into(),
            user_prompt: render_projection_evidence(&projection),
            response_schema: cloud_response_schema(),
            context_id: Some(projection.projection_hash.clone()),
        };
        let started = Instant::now();
        let mut response = None;
        for attempt in 0..3_u64 {
            match self
                .client
                .complete_json_enveloped::<CloudTaskCompletionJudgment>(request.clone())
                .await
            {
                Ok(envelope) => {
                    response = Some(Ok(envelope));
                    break;
                }
                Err(error) if attempt < 2 => {
                    response = Some(Err(error));
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
                }
                Err(error) => response = Some(Err(error)),
            }
        }
        let response = response.expect("cloud inference loop must execute");
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (judgment, provider_response, provider_error) = match response {
            Ok(envelope) => (
                Some(envelope.output),
                Some(envelope.provider_response),
                None,
            ),
            Err(error) => {
                let failure = error.downcast_ref::<ProviderExecutionFailureV1>();
                (
                    None,
                    failure.and_then(|failure| failure.provider_response.clone()),
                    Some(sanitize_cloud_error(&error.to_string())),
                )
            }
        };
        let usage = provider_response
            .as_ref()
            .and_then(|response| response.usage.as_ref());
        let input_tokens = usage
            .and_then(|usage| usage.input_tokens)
            .unwrap_or(projection.token_budget.projected_tokens.max(1));
        let output_tokens = usage.and_then(|usage| usage.output_tokens).unwrap_or(0);
        let inference = TaskCompletionInferenceProvenanceV1 {
            runtime: "openai-chat-completions".into(),
            model_id: self.model_id.clone(),
            model_hash: None,
            prompt_version: Some(CLOUD_PROMPT_VERSION.into()),
            tokenizer_id: "openai-provider-reported".into(),
            input_tokens,
            output_tokens,
            latency_ms: elapsed_ms,
            cost_microusd: None,
        };
        let (decision, judgment_error) = match judgment {
            Some(judgment) => {
                match self.decision_from_judgment(&projection, judgment, inference.clone()) {
                    Ok(decision) => (decision, None),
                    Err(error) => (
                        self.abstention_decision(
                            &projection,
                            inference,
                            LearnedAbstentionReasonV1::InvalidProviderOutput,
                            "invalid_provider_output",
                            "The provider judgment failed the compact task-completion contract.",
                        ),
                        Some(sanitize_cloud_error(&error.to_string())),
                    ),
                }
            }
            None => (
                self.abstention_decision(
                    &projection,
                    inference,
                    LearnedAbstentionReasonV1::ProviderUnavailable,
                    "provider_unavailable",
                    "Cloud judgment was unavailable; no completion decision was recorded.",
                ),
                None,
            ),
        };
        decision.validate_against(&projection)?;
        Ok(ModelRunRecord {
            schema_version: RUN_SCHEMA_VERSION.into(),
            target_key: projection.target_key,
            mandatory_facts_omitted: projection.stats.mandatory_facts_omitted,
            decision,
            error: provider_error.or(judgment_error),
            nli_diagnostics: None,
            provider_response,
        })
    }

    fn decision_from_judgment(
        &self,
        projection: &CompactTaskCompletionProjectionV1,
        judgment: CloudTaskCompletionJudgment,
        inference: TaskCompletionInferenceProvenanceV1,
    ) -> Result<BinaryTaskCompletionDecisionV1> {
        validate_probability(judgment.probability_complete, "probability_complete")?;
        anyhow::ensure!(
            !judgment.reason_code.trim().is_empty(),
            "cloud judgment reason_code must not be empty"
        );
        anyhow::ensure!(
            !judgment.explanation.trim().is_empty(),
            "cloud judgment explanation must not be empty"
        );
        anyhow::ensure!(
            judgment.evidence_ids.len() <= 3,
            "cloud judgment cited more than three evidence identifiers"
        );
        let known_ids = projection
            .facts
            .iter()
            .map(|fact| fact.evidence_id.as_str())
            .collect::<BTreeSet<_>>();
        let unique_ids = judgment
            .evidence_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            unique_ids.len() == judgment.evidence_ids.len(),
            "cloud judgment repeated an evidence identifier"
        );
        anyhow::ensure!(
            unique_ids.iter().all(|id| known_ids.contains(id)),
            "cloud judgment cited an unknown evidence identifier"
        );
        let (outcome, raw_logit_difference, probability_complete, abstention_reason) =
            match judgment.label {
                CloudTaskCompletionLabel::Completed => {
                    anyhow::ensure!(
                        judgment.probability_complete >= 0.5,
                        "completed cloud label conflicts with probability_complete"
                    );
                    anyhow::ensure!(
                        !judgment.evidence_ids.is_empty(),
                        "decisive cloud judgment requires cited evidence"
                    );
                    (
                        BinaryTaskCompletionOutcomeV1::Completed,
                        Some(logit(judgment.probability_complete)),
                        Some(judgment.probability_complete),
                        None,
                    )
                }
                CloudTaskCompletionLabel::Incomplete => {
                    anyhow::ensure!(
                        judgment.probability_complete < 0.5,
                        "incomplete cloud label conflicts with probability_complete"
                    );
                    anyhow::ensure!(
                        !judgment.evidence_ids.is_empty(),
                        "decisive cloud judgment requires cited evidence"
                    );
                    (
                        BinaryTaskCompletionOutcomeV1::Incomplete,
                        Some(logit(judgment.probability_complete)),
                        Some(judgment.probability_complete),
                        None,
                    )
                }
                CloudTaskCompletionLabel::Abstain => (
                    BinaryTaskCompletionOutcomeV1::Abstain,
                    None,
                    None,
                    Some(LearnedAbstentionReasonV1::EvidenceInsufficient),
                ),
            };
        Ok(BinaryTaskCompletionDecisionV1 {
            schema_version: BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION.into(),
            evaluator_release_id: self.evaluator_release_id.clone(),
            target_key: projection.target_key.clone(),
            target_revision: projection.target_revision.clone(),
            trace_context_binding_id: projection.trace_context_binding_id.clone(),
            projection_hash: projection.projection_hash.clone(),
            outcome,
            raw_logit_difference,
            probability_complete,
            threshold: 0.5,
            calibration_model_id: None,
            evidence_ids: judgment.evidence_ids,
            reason_code: Some(judgment.reason_code),
            explanation: Some(judgment.explanation),
            abstention_reason,
            inference,
        })
    }

    fn abstention_decision(
        &self,
        projection: &CompactTaskCompletionProjectionV1,
        inference: TaskCompletionInferenceProvenanceV1,
        abstention_reason: LearnedAbstentionReasonV1,
        reason_code: &str,
        explanation: &str,
    ) -> BinaryTaskCompletionDecisionV1 {
        BinaryTaskCompletionDecisionV1 {
            schema_version: BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION.into(),
            evaluator_release_id: self.evaluator_release_id.clone(),
            target_key: projection.target_key.clone(),
            target_revision: projection.target_revision.clone(),
            trace_context_binding_id: projection.trace_context_binding_id.clone(),
            projection_hash: projection.projection_hash.clone(),
            outcome: BinaryTaskCompletionOutcomeV1::Abstain,
            raw_logit_difference: None,
            probability_complete: None,
            threshold: 0.5,
            calibration_model_id: None,
            evidence_ids: Vec::new(),
            reason_code: Some(reason_code.into()),
            explanation: Some(explanation.into()),
            abstention_reason: Some(abstention_reason),
            inference,
        }
    }
}

fn cloud_response_schema() -> ResponseSchema {
    ResponseSchema {
        name: "compact_task_completion_judgment".into(),
        description: Some("Evidence-grounded binary task-completion judgment".into()),
        strict: true,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "label",
                "probability_complete",
                "evidence_ids",
                "reason_code",
                "explanation"
            ],
            "properties": {
                "label": {
                    "type": "string",
                    "enum": ["completed", "incomplete", "abstain"]
                },
                "probability_complete": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0
                },
                "evidence_ids": {
                    "type": "array",
                    "maxItems": 3,
                    "items": {"type": "string", "pattern": "^E[0-9]{4}$"}
                },
                "reason_code": {
                    "type": "string",
                    "enum": [
                        "task_completed",
                        "required_work_missing",
                        "verification_failed",
                        "completion_unsupported",
                        "unresolved_error",
                        "external_side_effect_missing",
                        "evidence_insufficient"
                    ]
                },
                "explanation": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 512
                }
            }
        }),
    }
}

fn sanitize_cloud_error(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            if word.starts_with("sk-") || word.contains("OPENAI_API_KEY=") {
                "[REDACTED]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

impl ModernBertNliClient {
    fn load(
        model_path: &Path,
        tokenizer_path: &Path,
        model_id: &str,
        model_hash: &str,
        tokenizer_hash: &str,
        threshold: f64,
        neutral_policy: NliNeutralPolicy,
    ) -> Result<Self> {
        validate_probability(threshold, "threshold")?;
        validate_sha256(model_hash, "model_hash")?;
        validate_sha256(tokenizer_hash, "tokenizer_hash")?;
        anyhow::ensure!(model_path.is_file(), "ONNX model does not exist");
        anyhow::ensure!(tokenizer_path.is_file(), "tokenizer does not exist");
        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|error| anyhow::anyhow!("failed to load tokenizer: {error}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: NLI_MAX_TOKENS,
                strategy: TruncationStrategy::LongestFirst,
                ..TruncationParams::default()
            }))
            .map_err(|error| anyhow::anyhow!("failed to configure tokenizer: {error}"))?;
        tokenizer.with_padding(None);
        let session = Session::builder()?
            .with_intra_threads(4)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .commit_from_file(model_path)?;
        let input_names = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            input_names == BTreeSet::from(["attention_mask".into(), "input_ids".into()]),
            "unsupported ModernBERT inputs: {input_names:?}"
        );
        let output_names = session
            .outputs()
            .iter()
            .map(|output| output.name().to_string())
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            output_names.contains("logits"),
            "ModernBERT model has no logits output: {output_names:?}"
        );
        let evaluator_release_id = canonical_content_id(
            "perseval.binary-task-completion-nli-evaluator.v1",
            &json!({
                "model_id": model_id,
                "model_hash": model_hash,
                "tokenizer_hash": tokenizer_hash,
                "prompt_version": NLI_PROMPT_VERSION,
                "hypothesis": NLI_HYPOTHESIS,
                "max_tokens": NLI_MAX_TOKENS,
                "labels": {
                    "0": "entailment",
                    "1": "neutral",
                    "2": "contradiction",
                },
                "neutral_policy": neutral_policy.as_str(),
            }),
        )?;
        Ok(Self {
            tokenizer,
            session,
            model_id: model_id.into(),
            model_hash: model_hash.into(),
            tokenizer_hash: tokenizer_hash.into(),
            evaluator_release_id,
            threshold,
            neutral_policy,
        })
    }

    fn evaluate(
        &mut self,
        projection: CompactTaskCompletionProjectionV1,
    ) -> Result<ModelRunRecord> {
        projection.validate()?;
        let premise = render_projection_evidence(&projection);
        let encoding = self
            .tokenizer
            .encode(
                EncodeInput::Dual(premise.into(), NLI_HYPOTHESIS.into()),
                true,
            )
            .map_err(|error| anyhow::anyhow!("failed to tokenize NLI input: {error}"))?;
        let input_truncated = !encoding.get_overflowing().is_empty();
        let input_tokens = u32::try_from(encoding.len())?;
        let input_ids = encoding
            .get_ids()
            .iter()
            .map(|value| i64::from(*value))
            .collect::<Vec<_>>();
        let attention_mask = encoding
            .get_attention_mask()
            .iter()
            .map(|value| i64::from(*value))
            .collect::<Vec<_>>();
        let sequence_length = input_ids.len();
        let input_ids = Tensor::from_array(([1, sequence_length], input_ids.into_boxed_slice()))?;
        let attention_mask =
            Tensor::from_array(([1, sequence_length], attention_mask.into_boxed_slice()))?;
        let started = Instant::now();
        let outputs = self.session.run(ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
        ])?;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (_, values) = outputs["logits"].try_extract_tensor::<f32>()?;
        anyhow::ensure!(
            values.len() == 3,
            "expected three NLI logits, got {}",
            values.len()
        );
        let logits = NliLogits {
            entailment: f64::from(values[0]),
            neutral: f64::from(values[1]),
            contradiction: f64::from(values[2]),
        };
        anyhow::ensure!(
            logits.entailment.is_finite()
                && logits.neutral.is_finite()
                && logits.contradiction.is_finite(),
            "NLI logits must be finite"
        );
        let neutral_is_argmax =
            logits.neutral >= logits.entailment && logits.neutral >= logits.contradiction;
        let probabilities = softmax_three(&logits);
        let (outcome, raw_logit_difference, probability_complete, abstention_reason) =
            if neutral_is_argmax && self.neutral_policy == NliNeutralPolicy::Abstain {
                (
                    BinaryTaskCompletionOutcomeV1::Abstain,
                    None,
                    None,
                    Some(LearnedAbstentionReasonV1::EvidenceInsufficient),
                )
            } else {
                let raw = logits.entailment - logits.contradiction;
                let probability = sigmoid(raw);
                let outcome = if probability >= self.threshold {
                    BinaryTaskCompletionOutcomeV1::Completed
                } else {
                    BinaryTaskCompletionOutcomeV1::Incomplete
                };
                (outcome, Some(raw), Some(probability), None)
            };
        let decision = BinaryTaskCompletionDecisionV1 {
            schema_version: BINARY_TASK_COMPLETION_DECISION_SCHEMA_VERSION.into(),
            evaluator_release_id: self.evaluator_release_id.clone(),
            target_key: projection.target_key.clone(),
            target_revision: projection.target_revision.clone(),
            trace_context_binding_id: projection.trace_context_binding_id.clone(),
            projection_hash: projection.projection_hash.clone(),
            outcome,
            raw_logit_difference,
            probability_complete,
            threshold: self.threshold,
            calibration_model_id: None,
            evidence_ids: Vec::new(),
            reason_code: None,
            explanation: None,
            abstention_reason,
            inference: TaskCompletionInferenceProvenanceV1 {
                runtime: "ort-2.0.0-rc.12/onnxruntime-1.24".into(),
                model_id: self.model_id.clone(),
                model_hash: Some(self.model_hash.clone()),
                prompt_version: Some(NLI_PROMPT_VERSION.into()),
                tokenizer_id: format!("tokenizer-json@{}", self.tokenizer_hash),
                input_tokens,
                output_tokens: 0,
                latency_ms,
                cost_microusd: Some(0),
            },
        };
        decision.validate_against(&projection)?;
        Ok(ModelRunRecord {
            schema_version: RUN_SCHEMA_VERSION.into(),
            target_key: projection.target_key,
            mandatory_facts_omitted: projection.stats.mandatory_facts_omitted,
            decision,
            error: None,
            nli_diagnostics: Some(NliDiagnostics {
                logits,
                probabilities,
                neutral_argmax: neutral_is_argmax,
                input_truncated,
            }),
            provider_response: None,
        })
    }
}

#[derive(Debug)]
enum InferenceError {
    Unavailable(String),
    Invalid(String),
}

impl InferenceError {
    fn abstention_reason(&self) -> LearnedAbstentionReasonV1 {
        match self {
            Self::Unavailable(_) => LearnedAbstentionReasonV1::ProviderUnavailable,
            Self::Invalid(_) => LearnedAbstentionReasonV1::InvalidProviderOutput,
        }
    }
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => {
                write!(formatter, "local inference unavailable: {message}")
            }
            Self::Invalid(message) => {
                write!(formatter, "invalid local inference output: {message}")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    completion_probabilities: Vec<CompletionProbability>,
}

#[derive(Debug, Deserialize)]
struct CompletionProbability {
    #[serde(default)]
    top_logprobs: Vec<TokenLogProbability>,
}

#[derive(Debug, Deserialize)]
struct TokenLogProbability {
    token: String,
    logprob: f64,
}

#[derive(Debug)]
struct BinaryLogits {
    complete: f64,
    incomplete: f64,
}

impl CompletionResponse {
    fn binary_logits(self) -> std::result::Result<BinaryLogits, InferenceError> {
        let probabilities = self
            .completion_probabilities
            .first()
            .ok_or_else(|| InferenceError::Invalid("missing completion probabilities".into()))?;
        let find = |token: &str| {
            probabilities
                .top_logprobs
                .iter()
                .find(|candidate| candidate.token == token)
                .map(|candidate| candidate.logprob)
        };
        let complete = find(COMPLETE_VERBALIZER).ok_or_else(|| {
            InferenceError::Invalid("complete verbalizer is absent from top logprobs".into())
        })?;
        let incomplete = find(INCOMPLETE_VERBALIZER).ok_or_else(|| {
            InferenceError::Invalid("incomplete verbalizer is absent from top logprobs".into())
        })?;
        if !complete.is_finite() || !incomplete.is_finite() {
            return Err(InferenceError::Invalid(
                "verbalizer log probabilities must be finite".into(),
            ));
        }
        Ok(BinaryLogits {
            complete,
            incomplete,
        })
    }
}

pub async fn run_smollm(
    projections: &Path,
    output: &Path,
    model_id: &str,
    model_hash: &str,
    threshold: f64,
    concurrency: usize,
    limit: Option<usize>,
) -> Result<ModelRunReport> {
    anyhow::ensure!(projections.is_file(), "projection file does not exist");
    anyhow::ensure!(
        (1..=8).contains(&concurrency),
        "concurrency must be between 1 and 8"
    );
    let client = SmolCompletionClient::from_environment(model_id, model_hash, threshold)?;
    let mut projections = load_projections(projections)?;
    projections.sort_by(|left, right| left.target_key.cmp(&right.target_key));
    if let Some(limit) = limit {
        anyhow::ensure!(limit > 0, "limit must be greater than zero");
        projections.truncate(limit);
    }
    // Continuous batching is substantially faster when prompts in a chunk have
    // similar lengths. The target key remains the deterministic tie breaker,
    // completed records are still sorted before they are persisted, and a
    // caller-supplied limit retains its original target-key selection semantics.
    projections.sort_by(|left, right| {
        left.token_budget
            .projected_tokens
            .cmp(&right.token_budget.projected_tokens)
            .then_with(|| left.target_key.cmp(&right.target_key))
    });
    anyhow::ensure!(!projections.is_empty(), "no projections selected");
    let selected_projections = projections.len() as u64;
    let existing = if output.is_file() {
        load_model_results(output)?
    } else {
        BTreeMap::new()
    };
    {
        let selected = projections
            .iter()
            .map(|projection| (projection.target_key.as_str(), projection))
            .collect::<BTreeMap<_, _>>();
        for result in existing.values() {
            let projection = selected.get(result.target_key.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "existing result {} is not in the selected projections",
                    result.target_key
                )
            })?;
            result.decision.validate_against(projection)?;
            anyhow::ensure!(
                result.mandatory_facts_omitted == projection.stats.mandatory_facts_omitted,
                "existing result {} has stale mandatory-evidence accounting",
                result.target_key
            );
            anyhow::ensure!(
                result.decision.inference.model_id == model_id
                    && result.decision.inference.model_hash.as_deref() == Some(model_hash)
                    && result.decision.inference.prompt_version.as_deref() == Some(PROMPT_VERSION)
                    && (result.decision.threshold - threshold).abs() < f64::EPSILON,
                "existing result {} was produced by a different evaluator configuration",
                result.target_key
            );
        }
    }
    projections.retain(|projection| !existing.contains_key(&projection.target_key));
    let mut writer = BufWriter::new(OpenOptions::new().create(true).append(true).open(output)?);
    let mut report = ModelRunReport {
        schema_version: RUN_SCHEMA_VERSION.into(),
        model_id: model_id.into(),
        model_hash: model_hash.into(),
        prompt_version: PROMPT_VERSION.into(),
        threshold,
        selected_projections,
        completed: 0,
        incomplete: 0,
        abstained: 0,
        summed_latency_ms: 0,
        output: output.display().to_string(),
    };
    for result in existing.values() {
        match result.decision.outcome {
            BinaryTaskCompletionOutcomeV1::Completed => report.completed += 1,
            BinaryTaskCompletionOutcomeV1::Incomplete => report.incomplete += 1,
            BinaryTaskCompletionOutcomeV1::Abstain => report.abstained += 1,
        }
        report.summed_latency_ms = report
            .summed_latency_ms
            .saturating_add(result.decision.inference.latency_ms);
    }
    for chunk in projections.chunks(concurrency) {
        let mut handles = Vec::with_capacity(chunk.len());
        for projection in chunk.iter().cloned() {
            let client = client.clone();
            handles.push(tokio::spawn(
                async move { client.evaluate(projection).await },
            ));
        }
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.await??);
        }
        results.sort_by(|left, right| left.target_key.cmp(&right.target_key));
        for result in results {
            match result.decision.outcome {
                BinaryTaskCompletionOutcomeV1::Completed => report.completed += 1,
                BinaryTaskCompletionOutcomeV1::Incomplete => report.incomplete += 1,
                BinaryTaskCompletionOutcomeV1::Abstain => report.abstained += 1,
            }
            report.summed_latency_ms = report
                .summed_latency_ms
                .saturating_add(result.decision.inference.latency_ms);
            serde_json::to_writer(&mut writer, &result)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
    }
    Ok(report)
}

pub async fn run_openai(
    projections: &Path,
    output: &Path,
    model_id: &str,
    concurrency: usize,
    limit: Option<usize>,
) -> Result<ModelRunReport> {
    anyhow::ensure!(projections.is_file(), "projection file does not exist");
    anyhow::ensure!(
        (1..=8).contains(&concurrency),
        "concurrency must be between 1 and 8"
    );
    let client = OpenAiCompletionClient::from_environment(model_id)?;
    let mut projections = load_projections(projections)?;
    projections.sort_by(|left, right| left.target_key.cmp(&right.target_key));
    if let Some(limit) = limit {
        anyhow::ensure!(limit > 0, "limit must be greater than zero");
        projections.truncate(limit);
    }
    anyhow::ensure!(!projections.is_empty(), "no projections selected");
    let selected_projections = projections.len() as u64;
    let existing = if output.is_file() {
        load_model_results(output)?
    } else {
        BTreeMap::new()
    };
    {
        let selected = projections
            .iter()
            .map(|projection| (projection.target_key.as_str(), projection))
            .collect::<BTreeMap<_, _>>();
        for result in existing.values() {
            let projection = selected.get(result.target_key.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "existing result {} is not in the selected projections",
                    result.target_key
                )
            })?;
            result.decision.validate_against(projection)?;
            anyhow::ensure!(
                result.mandatory_facts_omitted == projection.stats.mandatory_facts_omitted,
                "existing result {} has stale mandatory-evidence accounting",
                result.target_key
            );
            anyhow::ensure!(
                result.decision.inference.model_id == model_id
                    && result.decision.inference.model_hash.is_none()
                    && result.decision.inference.prompt_version.as_deref()
                        == Some(CLOUD_PROMPT_VERSION)
                    && (result.decision.threshold - 0.5).abs() < f64::EPSILON,
                "existing result {} was produced by a different evaluator configuration",
                result.target_key
            );
        }
    }
    projections.retain(|projection| !existing.contains_key(&projection.target_key));
    let mut writer = BufWriter::new(OpenOptions::new().create(true).append(true).open(output)?);
    let mut report = ModelRunReport {
        schema_version: RUN_SCHEMA_VERSION.into(),
        model_id: model_id.into(),
        model_hash: "provider-managed".into(),
        prompt_version: CLOUD_PROMPT_VERSION.into(),
        threshold: 0.5,
        selected_projections,
        completed: 0,
        incomplete: 0,
        abstained: 0,
        summed_latency_ms: 0,
        output: output.display().to_string(),
    };
    for result in existing.values() {
        update_report(&mut report, result);
    }
    for chunk in projections.chunks(concurrency) {
        let mut handles = Vec::with_capacity(chunk.len());
        for projection in chunk.iter().cloned() {
            let client = client.clone();
            handles.push(tokio::spawn(
                async move { client.evaluate(projection).await },
            ));
        }
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.await??);
        }
        results.sort_by(|left, right| left.target_key.cmp(&right.target_key));
        for result in results {
            update_report(&mut report, &result);
            serde_json::to_writer(&mut writer, &result)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
    }
    Ok(report)
}

fn update_report(report: &mut ModelRunReport, result: &ModelRunRecord) {
    match result.decision.outcome {
        BinaryTaskCompletionOutcomeV1::Completed => report.completed += 1,
        BinaryTaskCompletionOutcomeV1::Incomplete => report.incomplete += 1,
        BinaryTaskCompletionOutcomeV1::Abstain => report.abstained += 1,
    }
    report.summed_latency_ms = report
        .summed_latency_ms
        .saturating_add(result.decision.inference.latency_ms);
}

#[allow(clippy::too_many_arguments)]
pub fn run_modernbert_nli(
    projections: &Path,
    output: &Path,
    model_path: &Path,
    tokenizer_path: &Path,
    model_id: &str,
    model_hash: &str,
    tokenizer_hash: &str,
    neutral_policy: &str,
    threshold: f64,
    limit: Option<usize>,
) -> Result<ModelRunReport> {
    anyhow::ensure!(projections.is_file(), "projection file does not exist");
    let neutral_policy = NliNeutralPolicy::parse(neutral_policy)?;
    let mut client = ModernBertNliClient::load(
        model_path,
        tokenizer_path,
        model_id,
        model_hash,
        tokenizer_hash,
        threshold,
        neutral_policy,
    )?;
    let mut projections = load_projections(projections)?;
    projections.sort_by(|left, right| left.target_key.cmp(&right.target_key));
    if let Some(limit) = limit {
        anyhow::ensure!(limit > 0, "limit must be greater than zero");
        projections.truncate(limit);
    }
    anyhow::ensure!(!projections.is_empty(), "no projections selected");
    let mut writer = BufWriter::new(File::create(output)?);
    let mut report = ModelRunReport {
        schema_version: RUN_SCHEMA_VERSION.into(),
        model_id: model_id.into(),
        model_hash: model_hash.into(),
        prompt_version: NLI_PROMPT_VERSION.into(),
        threshold,
        selected_projections: projections.len() as u64,
        completed: 0,
        incomplete: 0,
        abstained: 0,
        summed_latency_ms: 0,
        output: output.display().to_string(),
    };
    for projection in projections {
        let result = client.evaluate(projection)?;
        match result.decision.outcome {
            BinaryTaskCompletionOutcomeV1::Completed => report.completed += 1,
            BinaryTaskCompletionOutcomeV1::Incomplete => report.incomplete += 1,
            BinaryTaskCompletionOutcomeV1::Abstain => report.abstained += 1,
        }
        report.summed_latency_ms = report
            .summed_latency_ms
            .saturating_add(result.decision.inference.latency_ms);
        serde_json::to_writer(&mut writer, &result)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(report)
}

fn load_projections(path: &Path) -> Result<Vec<CompactTaskCompletionProjectionV1>> {
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let projection: CompactTaskCompletionProjectionV1 = serde_json::from_str(&line)
            .with_context(|| format!("invalid projection on line {}", index + 1))?;
        projection.validate()?;
        anyhow::ensure!(
            seen.insert(projection.target_key.clone()),
            "duplicate projection target {}",
            projection.target_key
        );
        output.push(projection);
    }
    Ok(output)
}

#[derive(Debug, Deserialize)]
struct ResolutionLabel {
    trace_id: String,
    resolved: bool,
    #[serde(default)]
    split: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct Confusion {
    true_positive: u64,
    false_positive: u64,
    true_negative: u64,
    false_negative: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Metrics {
    confusion: Confusion,
    precision: Option<f64>,
    recall: Option<f64>,
    f1: f64,
    mcc: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BinaryScoreReport {
    schema_version: String,
    split: String,
    threshold: f64,
    threshold_source: String,
    labeled_results: u64,
    decisive_results: u64,
    abstained_results: u64,
    decision_coverage: f64,
    metrics: Metrics,
    auroc: Option<f64>,
    brier_score: f64,
    expected_calibration_error: f64,
    mandatory_facts_omitted: u64,
    mandatory_evidence_pass: bool,
    f1_must_exceed: f64,
    mcc_must_exceed: f64,
    f1_pass: bool,
    mcc_pass: bool,
    exit_pass: bool,
}

pub fn score(
    results: &Path,
    labels: &Path,
    split: &str,
    threshold: Option<f64>,
) -> Result<BinaryScoreReport> {
    let labels = load_resolution_labels(labels, split)?;
    let results = load_model_results(results)?;
    let configured_threshold = match threshold {
        Some(value) => {
            validate_probability(value, "threshold")?;
            value
        }
        None => one_threshold(results.values())?,
    };
    let mut probability_rows = Vec::new();
    let mut abstained = 0_u64;
    let mut labeled = 0_u64;
    let mut mandatory_facts_omitted = 0_u64;
    for (target_key, result) in results {
        let Some(label) = labels.get(&target_key) else {
            continue;
        };
        labeled += 1;
        mandatory_facts_omitted =
            mandatory_facts_omitted.saturating_add(u64::from(result.mandatory_facts_omitted));
        let Some(probability_complete) = result.decision.probability_complete else {
            abstained += 1;
            continue;
        };
        probability_rows.push((!label.resolved, 1.0 - probability_complete));
    }
    anyhow::ensure!(
        labeled > 0,
        "no labeled results overlap the requested split"
    );
    let actual_and_predicted = probability_rows
        .iter()
        .map(|(actual_failure, probability_failure)| {
            (
                *actual_failure,
                *probability_failure > 1.0 - configured_threshold,
            )
        })
        .collect::<Vec<_>>();
    let metrics = metrics(&actual_and_predicted);
    let (auroc, brier_score, expected_calibration_error) = probability_quality(&probability_rows);
    let f1_pass = metrics.f1 > F1_EXIT;
    let mcc_pass = metrics.mcc.is_some_and(|value| value > MCC_EXIT);
    let mandatory_evidence_pass = mandatory_facts_omitted == 0;
    Ok(BinaryScoreReport {
        schema_version: SCORE_SCHEMA_VERSION.into(),
        split: split.into(),
        threshold: configured_threshold,
        threshold_source: if threshold.is_some() {
            "explicit_frozen".into()
        } else {
            "decision_record".into()
        },
        labeled_results: labeled,
        decisive_results: actual_and_predicted.len() as u64,
        abstained_results: abstained,
        decision_coverage: actual_and_predicted.len() as f64 / labeled as f64,
        metrics,
        auroc,
        brier_score,
        expected_calibration_error,
        mandatory_facts_omitted,
        mandatory_evidence_pass,
        f1_must_exceed: F1_EXIT,
        mcc_must_exceed: MCC_EXIT,
        f1_pass,
        mcc_pass,
        exit_pass: f1_pass && mcc_pass && mandatory_evidence_pass,
    })
}

pub fn calibrate(results: &Path, labels: &Path, split: &str) -> Result<BinaryScoreReport> {
    let labels = load_resolution_labels(labels, split)?;
    let results = load_model_results(results)?;
    let mut rows = Vec::new();
    let mut abstained = 0_u64;
    let mut labeled = 0_u64;
    let mut mandatory_facts_omitted = 0_u64;
    for (target_key, result) in &results {
        let Some(label) = labels.get(target_key) else {
            continue;
        };
        labeled += 1;
        mandatory_facts_omitted =
            mandatory_facts_omitted.saturating_add(u64::from(result.mandatory_facts_omitted));
        match result.decision.probability_complete {
            Some(probability) => rows.push((!label.resolved, probability)),
            None => abstained += 1,
        }
    }
    anyhow::ensure!(
        rows.len() >= 2,
        "calibration requires two decisive labeled results"
    );
    let threshold = best_threshold(&rows);
    let predicted = rows
        .iter()
        .map(|(actual_failure, probability)| (*actual_failure, *probability < threshold))
        .collect::<Vec<_>>();
    let metrics = metrics(&predicted);
    let probability_rows = rows
        .iter()
        .map(|(actual_failure, probability_complete)| {
            (*actual_failure, 1.0 - *probability_complete)
        })
        .collect::<Vec<_>>();
    let (auroc, brier_score, expected_calibration_error) = probability_quality(&probability_rows);
    let f1_pass = metrics.f1 > F1_EXIT;
    let mcc_pass = metrics.mcc.is_some_and(|value| value > MCC_EXIT);
    let mandatory_evidence_pass = mandatory_facts_omitted == 0;
    Ok(BinaryScoreReport {
        schema_version: SCORE_SCHEMA_VERSION.into(),
        split: split.into(),
        threshold,
        threshold_source: "calibrated_on_requested_split".into(),
        labeled_results: labeled,
        decisive_results: rows.len() as u64,
        abstained_results: abstained,
        decision_coverage: rows.len() as f64 / labeled as f64,
        metrics,
        auroc,
        brier_score,
        expected_calibration_error,
        mandatory_facts_omitted,
        mandatory_evidence_pass,
        f1_must_exceed: F1_EXIT,
        mcc_must_exceed: MCC_EXIT,
        f1_pass,
        mcc_pass,
        exit_pass: f1_pass && mcc_pass && mandatory_evidence_pass,
    })
}

fn load_model_results(path: &Path) -> Result<BTreeMap<String, ModelRunRecord>> {
    let mut output = BTreeMap::new();
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let result: ModelRunRecord = serde_json::from_str(&line)
            .with_context(|| format!("invalid model result on line {}", index + 1))?;
        anyhow::ensure!(
            result.schema_version == RUN_SCHEMA_VERSION,
            "unsupported run result"
        );
        anyhow::ensure!(
            output.insert(result.target_key.clone(), result).is_none(),
            "duplicate model result on line {}",
            index + 1
        );
    }
    Ok(output)
}

fn load_resolution_labels(path: &Path, split: &str) -> Result<BTreeMap<String, ResolutionLabel>> {
    let mut output = BTreeMap::new();
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let label: ResolutionLabel = serde_json::from_str(&line)
            .with_context(|| format!("invalid resolution label on line {}", index + 1))?;
        if split != "all" && label.split.as_deref() != Some(split) {
            continue;
        }
        anyhow::ensure!(
            output.insert(label.trace_id.clone(), label).is_none(),
            "duplicate resolution label on line {}",
            index + 1
        );
    }
    Ok(output)
}

fn one_threshold<'a>(results: impl Iterator<Item = &'a ModelRunRecord>) -> Result<f64> {
    let values = results
        .map(|result| result.decision.threshold.to_bits())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(values.len() == 1, "result file mixes decision thresholds");
    Ok(f64::from_bits(
        *values.first().unwrap_or(&0.5_f64.to_bits()),
    ))
}

fn best_threshold(rows: &[(bool, f64)]) -> f64 {
    let mut probabilities = rows.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    probabilities.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    probabilities.dedup_by(|left, right| left.to_bits() == right.to_bits());
    let mut candidates = vec![0.0, 0.5, 1.0];
    candidates.extend(probabilities.iter().copied());
    candidates.extend(
        probabilities
            .windows(2)
            .map(|window| (window[0] + window[1]) / 2.0),
    );
    candidates
        .into_iter()
        .max_by(|left, right| {
            let left_metrics = metrics(
                &rows
                    .iter()
                    .map(|(actual, probability)| (*actual, *probability < *left))
                    .collect::<Vec<_>>(),
            );
            let right_metrics = metrics(
                &rows
                    .iter()
                    .map(|(actual, probability)| (*actual, *probability < *right))
                    .collect::<Vec<_>>(),
            );
            left_metrics
                .mcc
                .unwrap_or(-1.0)
                .partial_cmp(&right_metrics.mcc.unwrap_or(-1.0))
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    left_metrics
                        .f1
                        .partial_cmp(&right_metrics.f1)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| right.partial_cmp(left).unwrap_or(Ordering::Equal))
        })
        .unwrap_or(0.5)
}

fn metrics(rows: &[(bool, bool)]) -> Metrics {
    let mut confusion = Confusion {
        true_positive: 0,
        false_positive: 0,
        true_negative: 0,
        false_negative: 0,
    };
    for (actual, predicted) in rows {
        match (*actual, *predicted) {
            (true, true) => confusion.true_positive += 1,
            (false, true) => confusion.false_positive += 1,
            (false, false) => confusion.true_negative += 1,
            (true, false) => confusion.false_negative += 1,
        }
    }
    let precision = ratio(
        confusion.true_positive,
        confusion.true_positive + confusion.false_positive,
    );
    let recall = ratio(
        confusion.true_positive,
        confusion.true_positive + confusion.false_negative,
    );
    let f1 = match (precision, recall) {
        (Some(precision), Some(recall)) if precision + recall > 0.0 => {
            2.0 * precision * recall / (precision + recall)
        }
        _ => 0.0,
    };
    let tp = confusion.true_positive as f64;
    let fp = confusion.false_positive as f64;
    let tn = confusion.true_negative as f64;
    let fn_ = confusion.false_negative as f64;
    let denominator = ((tp + fp) * (tp + fn_) * (tn + fp) * (tn + fn_)).sqrt();
    let mcc = (denominator > 0.0).then(|| (tp * tn - fp * fn_) / denominator);
    Metrics {
        confusion,
        precision,
        recall,
        f1,
        mcc,
    }
}

fn probability_quality(rows: &[(bool, f64)]) -> (Option<f64>, f64, f64) {
    if rows.is_empty() {
        return (None, 0.0, 0.0);
    }
    let positives = rows.iter().filter(|(actual, _)| *actual).count();
    let negatives = rows.len().saturating_sub(positives);
    let auroc = if positives == 0 || negatives == 0 {
        None
    } else {
        let mut concordance = 0.0;
        for (_, positive_probability) in rows.iter().filter(|(actual, _)| *actual) {
            for (_, negative_probability) in rows.iter().filter(|(actual, _)| !*actual) {
                concordance += match positive_probability.partial_cmp(negative_probability) {
                    Some(Ordering::Greater) => 1.0,
                    Some(Ordering::Equal) => 0.5,
                    _ => 0.0,
                };
            }
        }
        Some(concordance / (positives * negatives) as f64)
    };
    let brier_score = rows
        .iter()
        .map(|(actual, probability)| {
            let target = if *actual { 1.0 } else { 0.0 };
            (probability - target).powi(2)
        })
        .sum::<f64>()
        / rows.len() as f64;
    let mut calibration_error = 0.0;
    for bin in 0..10 {
        let lower = bin as f64 / 10.0;
        let upper = (bin + 1) as f64 / 10.0;
        let members = rows
            .iter()
            .filter(|(_, probability)| {
                *probability >= lower
                    && if bin == 9 {
                        *probability <= upper
                    } else {
                        *probability < upper
                    }
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        let confidence = members
            .iter()
            .map(|(_, probability)| *probability)
            .sum::<f64>()
            / members.len() as f64;
        let accuracy =
            members.iter().filter(|(actual, _)| *actual).count() as f64 / members.len() as f64;
        calibration_error +=
            members.len() as f64 / rows.len() as f64 * (confidence - accuracy).abs();
    }
    (auroc, brier_score, calibration_error)
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

fn logit(probability: f64) -> f64 {
    let probability = probability.clamp(1e-12, 1.0 - 1e-12);
    (probability / (1.0 - probability)).ln()
}

fn softmax_three(logits: &NliLogits) -> NliLogits {
    let maximum = logits
        .entailment
        .max(logits.neutral)
        .max(logits.contradiction);
    let entailment = (logits.entailment - maximum).exp();
    let neutral = (logits.neutral - maximum).exp();
    let contradiction = (logits.contradiction - maximum).exp();
    let denominator = entailment + neutral + contradiction;
    NliLogits {
        entailment: entailment / denominator,
        neutral: neutral / denominator,
        contradiction: contradiction / denominator,
    }
}

fn validate_probability(value: f64, field: &str) -> Result<()> {
    anyhow::ensure!(
        value.is_finite() && (0.0..=1.0).contains(&value),
        "{field} must be within [0, 1]"
    );
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<()> {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{field} must be a sha256 content identity"
    );
    Ok(())
}

pub fn write_report(report: &BinaryScoreReport, path: &Path) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(report)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_token_ab_logits() {
        let response = CompletionResponse {
            completion_probabilities: vec![CompletionProbability {
                top_logprobs: vec![
                    TokenLogProbability {
                        token: " A".into(),
                        logprob: -0.2,
                    },
                    TokenLogProbability {
                        token: " B".into(),
                        logprob: -1.2,
                    },
                ],
            }],
        };
        let logits = response.binary_logits().unwrap();
        assert!((logits.complete - logits.incomplete - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn known_confusion_metrics_match() {
        let metrics = metrics(&[(true, true), (true, false), (false, true), (false, false)]);
        assert_eq!(metrics.confusion.true_positive, 1);
        assert_eq!(metrics.confusion.false_positive, 1);
        assert_eq!(metrics.confusion.true_negative, 1);
        assert_eq!(metrics.confusion.false_negative, 1);
        assert_eq!(metrics.f1, 0.5);
        assert_eq!(metrics.mcc, Some(0.0));
    }

    #[test]
    fn calibration_threshold_is_deterministic() {
        let rows = vec![(true, 0.1), (true, 0.3), (false, 0.7), (false, 0.9)];
        assert_eq!(best_threshold(&rows), 0.5);
    }

    #[test]
    fn sigmoid_is_stable_at_extremes() {
        assert!(sigmoid(1_000.0) > 0.999);
        assert!(sigmoid(-1_000.0) < 0.001);
    }

    #[test]
    fn probability_quality_rewards_correct_ranking_and_calibration() {
        let (auroc, brier, calibration_error) =
            probability_quality(&[(true, 0.9), (true, 0.8), (false, 0.2), (false, 0.1)]);
        assert_eq!(auroc, Some(1.0));
        assert!((brier - 0.025).abs() < 1e-12);
        assert!((calibration_error - 0.15).abs() < 1e-12);
    }

    #[test]
    fn cloud_schema_is_strict_and_label_complete() {
        let schema = cloud_response_schema();
        assert!(schema.strict);
        assert_eq!(schema.schema["additionalProperties"], false);
        assert_eq!(
            schema.schema["properties"]["label"]["enum"],
            json!(["completed", "incomplete", "abstain"])
        );
        assert_eq!(schema.schema["properties"]["evidence_ids"]["maxItems"], 3);
    }

    #[test]
    fn cloud_error_sanitization_and_logit_are_safe() {
        let sanitized = sanitize_cloud_error("request failed with sk-do-not-leak token");
        assert_eq!(sanitized, "request failed with [REDACTED] token");
        assert!(logit(1.0).is_finite());
        assert!(logit(0.0).is_finite());
        assert!((sigmoid(logit(0.73)) - 0.73).abs() < 1e-12);
    }
}

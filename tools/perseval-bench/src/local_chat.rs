use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use traces_to_evals::{
    ChatClient, ChatCompletionEnvelopeV1, ChatRequest, ProviderExecutionFailureV1,
    ProviderExecutionStageV1, ProviderResponseEnvelopeV1, ProviderTokenUsageV1,
    canonical_content_id,
};

/// Thin adapter for local servers implementing the OpenAI chat-completions shape.
///
/// This lives in the benchmark binary while the local inference runtime is still
/// being qualified. It deliberately preserves the production evaluator contract:
/// callers provide a typed response schema and receive the same provider envelope
/// used by hosted judges.
#[derive(Clone)]
pub struct LocalChatClient {
    client: Client,
    endpoint: String,
}

impl LocalChatClient {
    pub fn from_base_url(base_url: &str) -> Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/');
        anyhow::ensure!(
            !base_url.is_empty(),
            "local chat base URL must not be empty"
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .context("failed to build local chat HTTP client")?;
        Ok(Self {
            client,
            endpoint: format!("{base_url}/v1/chat/completions"),
        })
    }

    async fn execute(&self, request: ChatRequest) -> Result<RawCompletion> {
        let requested_model = request.model.clone();
        let context_id = request.context_id.clone();
        let mut response_schema = request.response_schema.schema.clone();
        apply_local_generation_bounds(&mut response_schema);
        let payload = json!({
            "model": request.model,
            "messages": [
                {
                    "role": "system",
                    "content": format!("{}\n\n/no_think", request.system_prompt)
                },
                {"role": "user", "content": request.user_prompt}
            ],
            "temperature": 0.0,
            "seed": 42,
            "max_tokens": 1024,
            "chat_template_kwargs": {"enable_thinking": false},
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": request.response_schema.name,
                    "strict": request.response_schema.strict,
                    "schema": response_schema
                }
            }
        });
        let request_hash = canonical_content_id("perseval.local-chat-request.v1", &payload)?;
        let started = Instant::now();
        let response = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&payload)?)
            .send()
            .await
            .map_err(|error| {
                transport_failure(
                    format_context(
                        format!("failed to call local chat server: {error}"),
                        &context_id,
                    ),
                    &requested_model,
                    &request_hash,
                    started.elapsed(),
                )
            })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            transport_failure(
                format_context(
                    format!("failed to read local chat response: {error}"),
                    &context_id,
                ),
                &requested_model,
                &request_hash,
                started.elapsed(),
            )
        })?;
        let latency_ms = elapsed_ms(started.elapsed());
        if !status.is_success() {
            return Err(transport_failure(
                format_context(http_error(status, &bytes), &context_id),
                &requested_model,
                &request_hash,
                started.elapsed(),
            ));
        }
        let response_value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            transport_failure(
                format_context(
                    format!("local chat returned invalid JSON: {error}"),
                    &context_id,
                ),
                &requested_model,
                &request_hash,
                started.elapsed(),
            )
        })?;
        let response_hash =
            canonical_content_id("perseval.local-chat-response.v1", &response_value)?;
        let response: ChatResponse = serde_json::from_value(response_value).map_err(|error| {
            transport_failure(
                format_context(
                    format!("local chat response shape is invalid: {error}"),
                    &context_id,
                ),
                &requested_model,
                &request_hash,
                started.elapsed(),
            )
        })?;
        let envelope = ProviderResponseEnvelopeV1 {
            provider: Some("llama.cpp".into()),
            requested_model,
            returned_model: response.model,
            response_id: response.id,
            finish_reason: response
                .choices
                .first()
                .and_then(|choice| choice.finish_reason.clone()),
            system_fingerprint: None,
            service_tier: None,
            usage: response.usage.map(Into::into),
            request_hash,
            response_hash,
            attempts: 1,
            latency_ms,
        };
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| {
                provider_failure(
                    ProviderExecutionStageV1::ResponseValidation,
                    format_context(
                        "local chat returned no assistant content".into(),
                        &context_id,
                    ),
                    envelope.clone(),
                )
            })?;
        Ok(RawCompletion { content, envelope })
    }
}

fn apply_local_generation_bounds(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("string") {
                object.entry("maxLength").or_insert(json!(512));
            }
            if object.get("type").and_then(Value::as_str) == Some("array") {
                object.entry("maxItems").or_insert(json!(64));
            }
            for value in object.values_mut() {
                apply_local_generation_bounds(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                apply_local_generation_bounds(value);
            }
        }
        _ => {}
    }
}

#[async_trait::async_trait]
impl ChatClient for LocalChatClient {
    async fn complete_json<T>(&self, request: ChatRequest) -> Result<T>
    where
        T: DeserializeOwned + Send,
    {
        let context_id = request.context_id.clone();
        let raw = self.execute(request).await?;
        serde_json::from_str(&raw.content).map_err(|error| {
            provider_failure(
                ProviderExecutionStageV1::OutputParsing,
                format_context(
                    format!("failed to parse local model output: {error}"),
                    &context_id,
                ),
                raw.envelope,
            )
        })
    }

    async fn complete_json_enveloped<T>(
        &self,
        request: ChatRequest,
    ) -> Result<ChatCompletionEnvelopeV1<T>>
    where
        T: DeserializeOwned + Serialize + Send,
    {
        let context_id = request.context_id.clone();
        let raw = self.execute(request).await?;
        let output = serde_json::from_str(&raw.content).map_err(|error| {
            provider_failure(
                ProviderExecutionStageV1::OutputParsing,
                format_context(
                    format!("failed to parse local model output: {error}"),
                    &context_id,
                ),
                raw.envelope.clone(),
            )
        })?;
        Ok(ChatCompletionEnvelopeV1::new(output, raw.envelope)?)
    }
}

struct RawCompletion {
    content: String,
    envelope: ProviderResponseEnvelopeV1,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
}

impl From<ChatUsage> for ProviderTokenUsageV1 {
    fn from(usage: ChatUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: None,
            reasoning_tokens: None,
        }
    }
}

fn transport_failure(
    message: String,
    requested_model: &str,
    request_hash: &str,
    elapsed: Duration,
) -> anyhow::Error {
    ProviderExecutionFailureV1 {
        stage: ProviderExecutionStageV1::Transport,
        message,
        requested_model: requested_model.into(),
        request_hash: request_hash.into(),
        attempts: 1,
        latency_ms: elapsed_ms(elapsed),
        provider_response: None,
    }
    .into()
}

fn provider_failure(
    stage: ProviderExecutionStageV1,
    message: String,
    envelope: ProviderResponseEnvelopeV1,
) -> anyhow::Error {
    ProviderExecutionFailureV1 {
        stage,
        message,
        requested_model: envelope.requested_model.clone(),
        request_hash: envelope.request_hash.clone(),
        attempts: envelope.attempts,
        latency_ms: envelope.latency_ms,
        provider_response: Some(envelope),
    }
    .into()
}

fn format_context(message: String, context_id: &Option<String>) -> String {
    match context_id {
        Some(context_id) => format!("{message} for {context_id}"),
        None => message,
    }
}

fn http_error(status: StatusCode, bytes: &[u8]) -> String {
    let body = String::from_utf8_lossy(bytes);
    let body = body.chars().take(1_000).collect::<String>();
    format!("local chat returned HTTP {status}: {body}")
}

fn elapsed_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_normalized_to_chat_completions_endpoint() {
        let client = LocalChatClient::from_base_url(" http://127.0.0.1:8080/ ").unwrap();
        assert_eq!(client.endpoint, "http://127.0.0.1:8080/v1/chat/completions");
        assert!(LocalChatClient::from_base_url("  ").is_err());
    }

    #[test]
    fn local_schema_bounds_are_recursive_and_preserve_tighter_limits() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "explanation": {"type": "string"},
                "evidence": {
                    "type": "array",
                    "maxItems": 3,
                    "items": {"type": "string", "maxLength": 64}
                }
            }
        });
        apply_local_generation_bounds(&mut schema);
        assert_eq!(schema["properties"]["explanation"]["maxLength"], 512);
        assert_eq!(schema["properties"]["evidence"]["maxItems"], 3);
        assert_eq!(schema["properties"]["evidence"]["items"]["maxLength"], 64);
    }

    #[test]
    fn http_errors_are_bounded() {
        let message = http_error(StatusCode::BAD_REQUEST, &vec![b'x'; 2_000]);
        assert!(message.starts_with("local chat returned HTTP 400 Bad Request: "));
        assert!(message.len() < 1_100);
    }
}

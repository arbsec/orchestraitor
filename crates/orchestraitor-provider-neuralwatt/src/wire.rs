//! `OpenAI` Chat Completions wire types for the Neuralwatt provider.
//!
//! These types model the JSON shapes exchanged with the Neuralwatt
//! OpenAI-compatible `/v1/chat/completions` and `/v1/models` endpoints.
//! They are internal to the crate boundary; callers use the project-owned
//! [`orchestraitor_provider_api::ModelRequest`] and
//! [`orchestraitor_provider_api::ModelEvent`] types.

use orchestraitor_provider_api::{MessageRole, ToolChoice};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire request body for `POST /v1/chat/completions`.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    /// Model id sent on the wire.
    pub model: String,
    /// Ordered input messages.
    pub messages: Vec<WireMessage>,
    /// Whether to stream the response via SSE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Tool definitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    /// Tool choice policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
}

/// Wire message in the `messages` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    /// Message role.
    pub role: String,
    /// Message text content.
    pub content: String,
}

/// Wire response for `POST /v1/chat/completions` (non-streaming).
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    /// Provider-assigned completion id.
    #[serde(default)]
    pub id: String,
    /// Object type (e.g. `chat.completion`).
    #[serde(default)]
    pub object: String,
    /// Model id echoed by the provider.
    #[serde(default)]
    pub model: String,
    /// Response choices.
    pub choices: Vec<Choice>,
    /// Token usage when reported.
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A single choice in a non-streaming response.
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    /// Choice index.
    #[serde(default)]
    pub index: u32,
    /// The message content.
    pub message: ResponseMessage,
    /// Finish reason (`stop`, `tool_calls`, `length`, etc.).
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Message object in a non-streaming response.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    /// Message role.
    #[serde(default)]
    pub role: String,
    /// Text content (may be empty when `tool_calls` is present).
    #[serde(default)]
    pub content: Option<String>,
    /// Tool calls requested by the model.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

/// A tool call in a response.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned tool call id.
    #[serde(default)]
    pub id: String,
    /// Tool type (always `function`).
    #[serde(default)]
    pub r#type: String,
    /// Function call details.
    pub function: ToolCallFunction,
}

/// Function call details within a tool call.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallFunction {
    /// Function name.
    pub name: String,
    /// Function arguments as a JSON string.
    #[serde(default)]
    pub arguments: String,
}

/// Token usage reported by the provider.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    /// Input (prompt) token count.
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Output (completion) token count.
    #[serde(default)]
    pub completion_tokens: u64,
    /// Total token count.
    #[serde(default)]
    pub total_tokens: u64,
}

/// Wire response for `GET /v1/models`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelsResponse {
    /// Object type (always `list`).
    #[serde(default)]
    pub object: String,
    /// Model entries.
    pub data: Vec<ModelEntry>,
}

/// A single model entry in the models list.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    /// Model id.
    pub id: String,
    /// Object type (always `model`).
    #[serde(default)]
    pub object: String,
    /// Owner of the model.
    #[serde(default)]
    pub owned_by: String,
}

/// A single SSE chunk in a streaming response.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    /// Provider-assigned completion id.
    #[serde(default)]
    pub id: String,
    /// Object type (e.g. `chat.completion.chunk`).
    #[serde(default)]
    pub object: String,
    /// Model id echoed by the provider.
    #[serde(default)]
    pub model: String,
    /// Chunk choices.
    #[serde(default)]
    pub choices: Vec<StreamChoice>,
    /// Token usage (typically only on the final chunk).
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A choice in a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    /// Choice index.
    #[serde(default)]
    pub index: u32,
    /// Incremental delta.
    pub delta: Delta,
    /// Finish reason (present on the final chunk).
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Incremental delta in a streaming chunk.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Delta {
    /// Role (present on the first chunk).
    #[serde(default)]
    pub role: Option<String>,
    /// Text content delta.
    #[serde(default)]
    pub content: Option<String>,
    /// Tool call deltas.
    #[serde(default)]
    pub tool_calls: Vec<StreamToolCall>,
}

/// A tool call delta in a streaming chunk.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamToolCall {
    /// Tool call index (for assembling fragments).
    #[serde(default)]
    pub index: u32,
    /// Tool call id (present on the first fragment).
    #[serde(default)]
    pub id: Option<String>,
    /// Tool type (always `function`).
    #[serde(default)]
    pub r#type: Option<String>,
    /// Function call details.
    #[serde(default)]
    pub function: Option<StreamToolCallFunction>,
}

/// Function call delta in a streaming chunk.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamToolCallFunction {
    /// Function name (present on the first fragment).
    #[serde(default)]
    pub name: Option<String>,
    /// Argument fragment (may be partial JSON).
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Converts a project-owned [`ModelRequest`] into a wire request body.
///
/// # Errors
///
/// Never fails in the current implementation; returns `Result` for forward
/// compatibility if validation is added later.
pub(crate) fn build_request_body(
    model_id: &str,
    request: &orchestraitor_provider_api::ModelRequest,
    stream: bool,
) -> ChatCompletionRequest {
    let messages: Vec<WireMessage> = request
        .messages
        .iter()
        .map(|msg| WireMessage {
            role: wire_role(msg.role).to_string(),
            content: msg.content.clone(),
        })
        .collect();

    let tools = request.extensions.get("tools").cloned();
    let tool_choice = request
        .extensions
        .get("tool_choice")
        .cloned()
        .or_else(|| request.tool_choice.as_ref().map(wire_tool_choice));

    ChatCompletionRequest {
        model: model_id.to_string(),
        messages,
        stream: stream.then_some(true),
        max_tokens: request.max_output_tokens,
        temperature: request.temperature,
        tools,
        tool_choice,
    }
}

/// Maps a [`MessageRole`] to the wire string.
pub(crate) fn wire_role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

/// Maps a [`ToolChoice`] to the wire JSON value.
fn wire_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::None => Value::String("none".to_string()),
        ToolChoice::Required => Value::String("required".to_string()),
        ToolChoice::Named(name) => serde_json::json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use orchestraitor_model::ProviderId;
    use orchestraitor_provider_api::{MessageRole, ModelMessage, ModelRequest};

    #[test]
    fn build_request_body_maps_fields() {
        let request = ModelRequest {
            provider_id: ProviderId::from_string("neuralwatt".to_string()),
            model_id: orchestraitor_model::ModelId::from_string("glm-5.2".to_string()),
            messages: vec![
                ModelMessage {
                    role: MessageRole::System,
                    content: "You are helpful.".to_string(),
                },
                ModelMessage {
                    role: MessageRole::User,
                    content: "Hello".to_string(),
                },
            ],
            max_output_tokens: Some(1024),
            temperature: Some(0.7),
            reasoning: None,
            structured_output: None,
            tool_choice: None,
            extensions: serde_json::Map::new(),
        };
        let body = build_request_body("glm-5.2", &request, true);
        assert_eq!(body.model, "glm-5.2");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].role, "user");
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.max_tokens, Some(1024));
    }

    #[test]
    fn build_request_body_non_streaming_omits_stream_field() {
        let request = ModelRequest {
            provider_id: ProviderId::from_string("neuralwatt".to_string()),
            model_id: orchestraitor_model::ModelId::from_string("glm-5.2".to_string()),
            messages: vec![],
            max_output_tokens: None,
            temperature: None,
            reasoning: None,
            structured_output: None,
            tool_choice: None,
            extensions: serde_json::Map::new(),
        };
        let body = build_request_body("glm-5.2", &request, false);
        assert!(body.stream.is_none());
    }

    #[test]
    fn parse_non_streaming_response_with_tool_calls() {
        let json = serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"NYC\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 20,
                "total_tokens": 70
            }
        });
        let response: ChatCompletionResponse = serde_json::from_value(json).unwrap();
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].message.tool_calls.len(), 1);
        assert_eq!(
            response.choices[0].message.tool_calls[0].function.name,
            "get_weather"
        );
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.completion_tokens, 20);
    }

    #[test]
    fn parse_stream_chunk_with_content_delta() {
        let json = serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "delta": { "content": "Hello" },
                "finish_reason": null
            }]
        });
        let chunk: StreamChunk = serde_json::from_value(json).unwrap();
        assert_eq!(chunk.choices.len(), 1);
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn parse_models_response() {
        let json = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "glm-5.2", "object": "model", "owned_by": "neuralwatt" }
            ]
        });
        let response: ModelsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].id, "glm-5.2");
    }
}

//! Response body rendering for compatible provider surfaces.

use chrono::Utc;
use orchestraitor_provider_api::{ModelEvent, TokenCount};
use serde_json::{Value, json};

use crate::{ProtocolSurface, TrustBoundaryReport};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompletionOutput {
    pub(crate) text: String,
    pub(crate) usage: TokenCount,
    pub(crate) tool_calls: Vec<Value>,
}

impl CompletionOutput {
    pub(crate) fn from_events(events: impl IntoIterator<Item = ModelEvent>) -> Self {
        let mut text = String::new();
        let mut usage = TokenCount {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            reasoning_tokens: 0,
        };
        let mut tool_calls = Vec::new();
        for event in events {
            match event {
                ModelEvent::Started | ModelEvent::Completed => {}
                ModelEvent::TextDelta { text: delta } => text.push_str(&delta),
                ModelEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                })),
                ModelEvent::Usage { token_count } => usage = token_count,
            }
        }
        Self {
            text,
            usage,
            tool_calls,
        }
    }
}

pub(crate) fn render_completion(
    surface: ProtocolSurface,
    model: &str,
    output: &CompletionOutput,
) -> Value {
    let report = TrustBoundaryReport::provider_proxy_only();
    match surface {
        ProtocolSurface::OpenAiChatCompletions => json!({
            "id": format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            "object": "chat.completion",
            "created": Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": output.text, "tool_calls": output.tool_calls },
                "finish_reason": "stop"
            }],
            "usage": usage_json(output.usage),
            "orchestraitor_trust_boundary": report
        }),
        ProtocolSurface::OpenAiResponses => json!({
            "id": format!("resp_{}", uuid::Uuid::new_v4()),
            "object": "response",
            "created_at": Utc::now().timestamp(),
            "model": model,
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": output.text }]
            }],
            "usage": usage_json(output.usage),
            "orchestraitor_trust_boundary": report
        }),
        ProtocolSurface::AnthropicMessages => json!({
            "id": format!("msg_{}", uuid::Uuid::new_v4()),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{ "type": "text", "text": output.text }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": output.usage.input_tokens,
                "output_tokens": output.usage.output_tokens
            },
            "orchestraitor_trust_boundary": report
        }),
    }
}

pub(crate) fn usage_json(usage: TokenCount) -> Value {
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens + usage.output_tokens + usage.cached_tokens + usage.reasoning_tokens,
        "prompt_tokens_details": { "cached_tokens": usage.cached_tokens },
        "completion_tokens_details": { "reasoning_tokens": usage.reasoning_tokens }
    })
}

//! Protocol routing and provider registry.

use std::collections::HashMap;
use std::sync::Arc;

use orchestraitor_model::{ModelId, ProviderId};
use orchestraitor_provider_api::{ModelRequest, ProviderProtocol, ProviderTransport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{ProxyError, ProxyResult};

/// Provider-compatible protocol surface selected by an HTTP endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolSurface {
    /// `OpenAI` Chat Completions `/v1/chat/completions`.
    OpenAiChatCompletions,
    /// `OpenAI` Responses `/v1/responses`.
    OpenAiResponses,
    /// Anthropic Messages `/v1/messages`.
    AnthropicMessages,
}

impl ProtocolSurface {
    /// Returns a storage label for this protocol surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    pub(crate) const fn provider_protocol(self) -> ProviderProtocol {
        match self {
            Self::OpenAiChatCompletions => ProviderProtocol::OpenAiChatCompletions,
            Self::OpenAiResponses => ProviderProtocol::OpenAiResponses,
            Self::AnthropicMessages => ProviderProtocol::AnthropicMessages,
        }
    }
}

/// Route from model/protocol to a concrete provider transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRoute {
    /// Provider id selected before auth resolution.
    pub provider_id: ProviderId,
    /// Model id selected before auth resolution.
    pub model_id: ModelId,
    /// Routing-decision label stored in the cost ledger.
    pub routing_decision: String,
}

/// Registry of provider transports and explicit model routes.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    transports: HashMap<ProviderId, Arc<dyn ProviderTransport>>,
    routes: HashMap<(ProtocolSurface, ModelId), ProviderRoute>,
}

impl ProviderRegistry {
    /// Creates an empty provider registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider transport.
    #[must_use]
    pub fn with_transport(mut self, transport: Arc<dyn ProviderTransport>) -> Self {
        self.transports
            .insert(transport.descriptor().id.clone(), transport);
        self
    }

    /// Registers an explicit model route for a protocol surface.
    #[must_use]
    pub fn with_route(mut self, surface: ProtocolSurface, route: ProviderRoute) -> Self {
        self.routes.insert((surface, route.model_id.clone()), route);
        self
    }

    /// Resolves the provider route and transport for a request body.
    ///
    /// # Errors
    /// Returns a route error when no exact model/surface route exists.
    pub fn resolve(&self, surface: ProtocolSurface, model: &str) -> ProxyResult<ResolvedProvider> {
        let model_id = ModelId::from_string(model.to_owned());
        let route =
            self.routes
                .get(&(surface, model_id))
                .ok_or_else(|| ProxyError::RouteMissing {
                    model: model.to_owned(),
                    surface: surface.as_str(),
                })?;
        let transport =
            self.transports
                .get(&route.provider_id)
                .ok_or_else(|| ProxyError::RouteMissing {
                    model: model.to_owned(),
                    surface: surface.as_str(),
                })?;
        Ok(ResolvedProvider {
            route: route.clone(),
            transport: Arc::clone(transport),
        })
    }

    /// Returns configured models for the OpenAI-compatible `/v1/models` surface.
    #[must_use]
    pub fn model_entries(&self) -> Vec<serde_json::Value> {
        self.routes
            .iter()
            .map(|((surface, model_id), route)| {
                serde_json::json!({
                    "id": model_id.as_str(),
                    "object": "model",
                    "owned_by": route.provider_id.as_str(),
                    "orchestraitor_protocol_surface": surface.as_str()
                })
            })
            .collect()
    }
}

/// Resolved route and transport.
#[derive(Clone)]
pub struct ResolvedProvider {
    /// Route metadata.
    pub route: ProviderRoute,
    /// Provider transport.
    pub transport: Arc<dyn ProviderTransport>,
}

pub(crate) fn model_from_body(body: &Value) -> ProxyResult<&str> {
    body.get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ProxyError::InvalidRequest {
            message: "missing string field `model`".to_owned(),
        })
}

pub(crate) fn model_request_from_body(
    surface: ProtocolSurface,
    body: &Value,
    route: &ProviderRoute,
) -> ProxyResult<ModelRequest> {
    let messages = match surface {
        ProtocolSurface::OpenAiChatCompletions | ProtocolSurface::AnthropicMessages => {
            messages_from_array(body.get("messages"))?
        }
        ProtocolSurface::OpenAiResponses => responses_input_messages(body.get("input"))?,
    };
    Ok(ModelRequest {
        provider_id: route.provider_id.clone(),
        model_id: route.model_id.clone(),
        messages,
        max_output_tokens: optional_u32(body, "max_tokens")
            .or_else(|| optional_u32(body, "max_output_tokens")),
        temperature: optional_f32(body, "temperature"),
        reasoning: None,
        structured_output: body.get("response_format").cloned(),
        tool_choice: None,
        extensions: extension_fields(body),
    })
}

fn messages_from_array(
    value: Option<&Value>,
) -> ProxyResult<Vec<orchestraitor_provider_api::ModelMessage>> {
    let Some(Value::Array(items)) = value else {
        return Err(ProxyError::InvalidRequest {
            message: "missing array field `messages`".to_owned(),
        });
    };
    items.iter().map(message_from_value).collect()
}

fn message_from_value(value: &Value) -> ProxyResult<orchestraitor_provider_api::ModelMessage> {
    let role =
        value
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| ProxyError::InvalidRequest {
                message: "message missing string field `role`".to_owned(),
            })?;
    let role = match role {
        "system" => orchestraitor_provider_api::MessageRole::System,
        "user" => orchestraitor_provider_api::MessageRole::User,
        "assistant" => orchestraitor_provider_api::MessageRole::Assistant,
        "tool" => orchestraitor_provider_api::MessageRole::Tool,
        _ => {
            return Err(ProxyError::InvalidRequest {
                message: "unsupported message role".to_owned(),
            });
        }
    };
    Ok(orchestraitor_provider_api::ModelMessage {
        role,
        content: content_text(value.get("content"))?,
    })
}

fn responses_input_messages(
    value: Option<&Value>,
) -> ProxyResult<Vec<orchestraitor_provider_api::ModelMessage>> {
    match value {
        Some(Value::String(input)) => Ok(vec![orchestraitor_provider_api::ModelMessage {
            role: orchestraitor_provider_api::MessageRole::User,
            content: input.clone(),
        }]),
        Some(Value::Array(_)) => messages_from_array(value),
        _ => Err(ProxyError::InvalidRequest {
            message: "missing `input` string or message array".to_owned(),
        }),
    }
}

fn content_text(value: Option<&Value>) -> ProxyResult<String> {
    match value {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => Ok(parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")),
        _ => Err(ProxyError::InvalidRequest {
            message: "message missing text content".to_owned(),
        }),
    }
}

fn optional_u32(body: &Value, key: &str) -> Option<u32> {
    body.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn optional_f32(body: &Value, key: &str) -> Option<f32> {
    body.get(key)
        .and_then(Value::as_f64)
        .and_then(|value| value.to_string().parse::<f32>().ok())
}

fn extension_fields(body: &Value) -> Map<String, Value> {
    let mut extensions = Map::new();
    for key in ["tools", "tool_choice", "stream", "metadata"] {
        if let Some(value) = body.get(key) {
            extensions.insert(key.to_owned(), value.clone());
        }
    }
    extensions
}

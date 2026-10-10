//! Provider transport trait and provider-neutral model types.

use async_trait::async_trait;
use orchestraitor_model::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};

use crate::capabilities::{DiscoveredModel, ProviderCapabilities};
use crate::error::ProviderTransportError;

/// Convenience result type for provider transport operations.
pub type ProviderResult<T> = Result<T, ProviderTransportError>;

/// Boxed model event stream returned by provider transports.
pub type ModelEventStream = Box<dyn Iterator<Item = ProviderResult<ModelEvent>> + Send>;

/// Project-owned transport abstraction for model providers.
#[async_trait]
pub trait ProviderTransport: Send + Sync {
    /// Returns the static provider descriptor for this transport.
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Lists provider-visible models without inferring protocol from model names.
    ///
    /// # Errors
    ///
    /// Returns a provider error when discovery fails or the provider rejects the request.
    async fn list_models(&self) -> ProviderResult<Vec<DiscoveredModel>>;

    /// Streams events for one fully-bound model request.
    ///
    /// # Errors
    ///
    /// Returns a provider error when the request cannot be submitted.
    async fn stream(&self, request: ModelRequest) -> ProviderResult<ModelEventStream>;

    /// Counts tokens for a request when the provider exposes that capability.
    ///
    /// # Errors
    ///
    /// Returns a provider error when token counting was attempted and failed.
    async fn count_tokens(&self, request: TokenCountRequest) -> ProviderResult<Option<TokenCount>>;

    /// Reports current provider health.
    ///
    /// # Errors
    ///
    /// Returns a provider error when the health probe itself fails.
    async fn health(&self) -> ProviderResult<ProviderHealth>;
}

/// Fully-bound model request sent to a provider transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Explicit provider id selected before auth resolution.
    pub provider_id: ProviderId,
    /// Explicit model id selected before auth resolution.
    pub model_id: ModelId,
    /// Ordered input messages.
    pub messages: Vec<ModelMessage>,
    /// Optional maximum output token budget.
    pub max_output_tokens: Option<u32>,
    /// Optional sampling temperature.
    pub temperature: Option<f32>,
    /// Optional provider reasoning controls.
    pub reasoning: Option<ReasoningConfig>,
    /// Optional structured-output schema as provider-neutral JSON.
    pub structured_output: Option<serde_json::Value>,
    /// Optional tool choice policy.
    pub tool_choice: Option<ToolChoice>,
    /// Provider-specific extension fields preserved behind the project boundary.
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

/// Message within a model request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMessage {
    /// Message role.
    pub role: MessageRole,
    /// Text content for the message.
    pub content: String,
}

/// Role attached to a model message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageRole {
    /// System instruction message.
    System,
    /// User-authored input message.
    User,
    /// Assistant-authored output message.
    Assistant,
    /// Tool result message.
    Tool,
}

/// Reasoning controls preserved for capable providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningConfig {
    /// Requested reasoning effort level.
    pub effort: ReasoningEffort,
    /// Optional thinking token budget.
    pub budget_tokens: Option<u32>,
}

/// Provider-neutral reasoning effort levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffort {
    /// Minimal reasoning effort.
    Low,
    /// Balanced reasoning effort.
    Medium,
    /// Maximum reasoning effort.
    High,
}

/// Tool-choice policy requested for a model call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolChoice {
    /// Provider chooses whether to call tools.
    Auto,
    /// Provider must not call tools.
    None,
    /// Provider must call a tool.
    Required,
    /// Provider must call the named tool.
    Named(String),
}

/// Token-count request sent to capable provider transports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenCountRequest {
    /// Provider id selected before token counting.
    pub provider_id: ProviderId,
    /// Model id selected before token counting.
    pub model_id: ModelId,
    /// Messages to count.
    pub messages: Vec<ModelMessage>,
}

/// Event emitted by a provider stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum ModelEvent {
    /// Stream began.
    Started,
    /// Text delta from the model.
    TextDelta {
        /// Incremental UTF-8 text.
        text: String,
    },
    /// Tool call requested by the model.
    ToolCall {
        /// Provider-assigned tool call id.
        id: String,
        /// Tool name.
        name: String,
        /// Tool arguments as JSON.
        arguments: serde_json::Value,
    },
    /// Token usage update.
    Usage {
        /// Current token accounting.
        token_count: TokenCount,
    },
    /// Stream completed successfully.
    Completed,
}

/// Token usage returned by providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCount {
    /// Input token count.
    pub input_tokens: u64,
    /// Output token count.
    pub output_tokens: u64,
    /// Tokens read from prompt cache.
    pub cached_tokens: u64,
    /// Reasoning or thinking token count.
    pub reasoning_tokens: u64,
}

/// Provider health result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderHealth {
    /// Health status.
    pub status: ProviderHealthStatus,
    /// Optional non-secret diagnostic message.
    pub message: Option<String>,
}

/// Provider health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderHealthStatus {
    /// Provider is accepting requests.
    Healthy,
    /// Provider is reachable but degraded.
    Degraded,
    /// Provider is unavailable.
    Unhealthy,
}

/// Static provider descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    /// Stable provider id.
    pub id: ProviderId,
    /// Human-readable display name.
    pub display_name: String,
    /// Explicit provider protocol family.
    pub protocol: ProviderProtocol,
    /// Provider capabilities represented without silently inferring support.
    pub capabilities: ProviderCapabilities,
}

/// Explicit provider protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderProtocol {
    /// `OpenAI` Responses API.
    OpenAiResponses,
    /// `OpenAI` Chat Completions compatible API.
    OpenAiChatCompletions,
    /// Anthropic Messages API or compatible endpoint.
    AnthropicMessages,
    /// Google Gemini native API.
    GeminiNative,
    /// Google Vertex AI endpoint.
    VertexAi,
    /// Local OpenAI-compatible endpoint.
    LocalOpenAiCompatible,
    /// Custom provider plugin.
    CustomPlugin,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubTransport {
        descriptor: ProviderDescriptor,
    }

    #[async_trait]
    impl ProviderTransport for StubTransport {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        async fn list_models(&self) -> ProviderResult<Vec<DiscoveredModel>> {
            Ok(Vec::new())
        }

        async fn stream(&self, _request: ModelRequest) -> ProviderResult<ModelEventStream> {
            Ok(Box::new(
                vec![Ok(ModelEvent::Started), Ok(ModelEvent::Completed)].into_iter(),
            ))
        }

        async fn count_tokens(
            &self,
            _request: TokenCountRequest,
        ) -> ProviderResult<Option<TokenCount>> {
            Ok(Some(TokenCount {
                input_tokens: 1,
                output_tokens: 2,
                cached_tokens: 0,
                reasoning_tokens: 0,
            }))
        }

        async fn health(&self) -> ProviderResult<ProviderHealth> {
            Ok(ProviderHealth {
                status: ProviderHealthStatus::Healthy,
                message: None,
            })
        }
    }

    #[tokio::test]
    async fn provider_transport_trait_object_accepts_stub_impl() -> ProviderResult<()> {
        let descriptor = ProviderDescriptor {
            id: ProviderId::from_string("stub".to_string()),
            display_name: "Stub".to_string(),
            protocol: ProviderProtocol::CustomPlugin,
            capabilities: ProviderCapabilities::default(),
        };
        let transport = StubTransport { descriptor };
        let transport_object: &dyn ProviderTransport = &transport;

        assert_eq!(transport_object.descriptor().display_name, "Stub");
        assert_eq!(transport_object.list_models().await?.len(), 0);
        Ok(())
    }
}

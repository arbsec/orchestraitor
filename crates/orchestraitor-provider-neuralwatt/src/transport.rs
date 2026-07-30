//! [`ProviderTransport`] implementation for the Neuralwatt GLM-5.2 BYOK adapter.
//!
//! The transport sends `OpenAI` Chat Completions-compatible requests to the
//! Neuralwatt API endpoint and converts responses into [`ModelEvent`] streams.
//! Streaming responses are consumed via [`reqwest::Response::bytes_stream`]
//! and parsed as Server-Sent Events.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use orchestraitor_model::{ModelId, ProviderId};
use orchestraitor_provider_api::{
    ModelRequest, TokenCountRequest,
    capabilities::{CapabilitySupport, DiscoveredModel, ModelMetadataSource, ProviderCapabilities},
    transport::{
        ModelEvent, ModelEventStream, ProviderDescriptor, ProviderHealth, ProviderHealthStatus,
        ProviderProtocol, ProviderResult, ProviderTransport, TokenCount,
    },
};
use secrecy::ExposeSecret;
use tracing::{debug, warn};

use crate::config::NeuralwattConfig;
use crate::cost::{CompletionCostInput, CostAttribution, CostSink};
use crate::error::NeuralwattError;
use crate::stream::{SseParser, response_to_events};
use crate::wire::{ChatCompletionResponse, ModelsResponse, build_request_body};

/// Provider id for Neuralwatt.
const PROVIDER_ID: &str = "neuralwatt";

/// Display name for the Neuralwatt provider.
const PROVIDER_DISPLAY_NAME: &str = "Neuralwatt GLM-5.2";

/// HTTP connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP total request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(2);

/// Maximum redirect hops.
const MAX_REDIRECTS: usize = 5;

/// Neuralwatt provider transport implementing [`ProviderTransport`].
pub struct NeuralwattTransport {
    descriptor: ProviderDescriptor,
    http: reqwest::Client,
    config: NeuralwattConfig,
    api_key: secrecy::SecretString,
    cost_sink: Option<Arc<dyn CostSink>>,
    cost_attribution: CostAttribution,
}

impl NeuralwattTransport {
    /// Creates a transport from configuration, resolving the API key from
    /// the configured auth URI.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError`] when the HTTP client cannot be built or
    /// the API key cannot be resolved.
    pub fn from_config(config: NeuralwattConfig) -> Result<Self, NeuralwattError> {
        let api_key = config.resolve_api_key()?;
        Self::with_key(config, api_key)
    }

    /// Creates a transport with an explicit API key, bypassing auth resolution.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError`] when the HTTP client cannot be built.
    pub fn with_key(
        config: NeuralwattConfig,
        api_key: secrecy::SecretString,
    ) -> Result<Self, NeuralwattError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()
            .map_err(|source| NeuralwattError::HttpRequest { source })?;
        let descriptor = ProviderDescriptor {
            id: ProviderId::from_string(PROVIDER_ID.to_string()),
            display_name: PROVIDER_DISPLAY_NAME.to_string(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            capabilities: ProviderCapabilities {
                tool_choice: CapabilitySupport::Supported,
                structured_outputs: CapabilitySupport::Supported,
                ..ProviderCapabilities::default()
            },
        };
        Ok(Self {
            descriptor,
            http,
            config,
            api_key,
            cost_sink: None,
            cost_attribution: CostAttribution::local_defaults(),
        })
    }

    /// Attaches a cost sink for per-call cost entry emission (spec §9.19.4).
    #[must_use]
    pub fn with_cost_sink(mut self, sink: Arc<dyn CostSink>) -> Self {
        self.cost_sink = Some(sink);
        self
    }

    /// Overrides the cost attribution context.
    #[must_use]
    pub fn with_cost_attribution(mut self, attribution: CostAttribution) -> Self {
        self.cost_attribution = attribution;
        self
    }

    /// Returns the configured base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.config.base_url()
    }

    /// Returns the provider descriptor.
    #[must_use]
    pub fn descriptor_ref(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    /// Sends a non-streaming chat completion request and returns the parsed response.
    async fn send_chat_completion(
        &self,
        request: &ModelRequest,
    ) -> Result<ChatCompletionResponse, NeuralwattError> {
        let body = build_request_body(request.model_id.as_str(), request, false);
        let url = format!("{}/chat/completions", self.config.base_url());
        debug!(url = %url, model = %request.model_id.as_str(), "sending non-streaming chat completion");
        let response = self
            .http
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .json(&body)
            .send()
            .await
            .map_err(|source| NeuralwattError::HttpRequest { source })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = response.text().await.unwrap_or_default();
            let excerpt = body_excerpt.chars().take(500).collect::<String>();
            return Err(NeuralwattError::ProviderStatus {
                status: status.as_u16(),
                body_excerpt: excerpt,
            });
        }
        let body_text = response
            .text()
            .await
            .map_err(|source| NeuralwattError::HttpRequest { source })?;
        serde_json::from_str::<ChatCompletionResponse>(&body_text)
            .map_err(|source| NeuralwattError::ResponseParse { source })
    }

    /// Sends a streaming chat completion request and returns the raw response.
    async fn send_streaming_completion(
        &self,
        request: &ModelRequest,
    ) -> Result<reqwest::Response, NeuralwattError> {
        let body = build_request_body(request.model_id.as_str(), request, true);
        let url = format!("{}/chat/completions", self.config.base_url());
        debug!(url = %url, model = %request.model_id.as_str(), "sending streaming chat completion");
        let response = self
            .http
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .json(&body)
            .send()
            .await
            .map_err(|source| NeuralwattError::HttpRequest { source })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = response.text().await.unwrap_or_default();
            let excerpt = body_excerpt.chars().take(500).collect::<String>();
            return Err(NeuralwattError::ProviderStatus {
                status: status.as_u16(),
                body_excerpt: excerpt,
            });
        }
        Ok(response)
    }

    /// Fetches the models list endpoint.
    async fn fetch_models(&self) -> Result<ModelsResponse, NeuralwattError> {
        let url = format!("{}/models", self.config.base_url());
        debug!(url = %url, "fetching models list");
        let response = self
            .http
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .send()
            .await
            .map_err(|source| NeuralwattError::HttpRequest { source })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = response.text().await.unwrap_or_default();
            let excerpt = body_excerpt.chars().take(500).collect::<String>();
            return Err(NeuralwattError::ProviderStatus {
                status: status.as_u16(),
                body_excerpt: excerpt,
            });
        }
        let body_text = response
            .text()
            .await
            .map_err(|source| NeuralwattError::HttpRequest { source })?;
        serde_json::from_str::<ModelsResponse>(&body_text)
            .map_err(|source| NeuralwattError::ResponseParse { source })
    }

    /// Records a cost entry for a completed call if a sink is attached.
    fn record_cost(
        &self,
        request_id: &str,
        model_id: &str,
        usage: TokenCount,
        started_at: chrono::DateTime<chrono::Utc>,
        completed_at: chrono::DateTime<chrono::Utc>,
    ) {
        if let Some(sink) = &self.cost_sink {
            let input = CompletionCostInput {
                request_id,
                usage,
                routing_decision: "openai-chat-completions:neuralwatt",
            };
            let provider = self.descriptor.id.as_str();
            let attribution = &self.cost_attribution;
            let entry = build_cost_entry(
                attribution,
                &input,
                model_id,
                provider,
                started_at,
                completed_at,
            );
            if let Err(e) = sink.record(&entry) {
                warn!(error = %e, "failed to record cost entry");
            }
        }
    }
}

#[async_trait]
impl ProviderTransport for NeuralwattTransport {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn list_models(&self) -> ProviderResult<Vec<DiscoveredModel>> {
        let response = self.fetch_models().await.map_err(|_| {
            orchestraitor_provider_api::ProviderTransportError::RequestFailed {
                provider_id: self.descriptor.id.clone(),
            }
        })?;
        let models = response
            .data
            .into_iter()
            .map(|entry| {
                let id = entry.id;
                DiscoveredModel {
                    provider_id: self.descriptor.id.clone(),
                    model_id: ModelId::from_string(id.clone()),
                    wire_model_id: Some(id.clone()),
                    display_name: Some(id),
                    context_window: None,
                    max_output_tokens: None,
                    capabilities: ProviderCapabilities::default(),
                    metadata_source: ModelMetadataSource::ProviderEndpoint,
                }
            })
            .collect();
        Ok(models)
    }

    async fn stream(&self, request: ModelRequest) -> ProviderResult<ModelEventStream> {
        let request_id = format!("neuralwatt-{}", uuid::Uuid::new_v4());
        let started_at = chrono::Utc::now();
        let model_id = request.model_id.as_str();

        // Check if streaming is requested via extensions.
        let want_stream = request
            .extensions
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);

        if want_stream {
            let response = self
                .send_streaming_completion(&request)
                .await
                .map_err(
                    |_| orchestraitor_provider_api::ProviderTransportError::RequestFailed {
                        provider_id: self.descriptor.id.clone(),
                    },
                )?;

            // TODO(spec §9.19.4): true incremental streaming — events are buffered
            // into a Vec before returning. For MVP this is acceptable; converting
            // this to a channel-based bridge (e.g. `futures::channel::mpsc`) would
            // let callers observe tokens as they arrive without buffering the full
            // completion. Defer until cost attribution semantics are finalised.
            let mut parser = SseParser::new();
            let mut all_events = vec![ModelEvent::Started];
            let mut stream = response.bytes_stream();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result.map_err(|_| {
                    orchestraitor_provider_api::ProviderTransportError::RequestFailed {
                        provider_id: self.descriptor.id.clone(),
                    }
                })?;
                let events = parser.feed(&chunk).map_err(|_| {
                    orchestraitor_provider_api::ProviderTransportError::InvalidEvent
                })?;
                all_events.extend(events);
            }
            // Flush any remaining buffered data.
            let remaining = parser
                .finish()
                .map_err(|_| orchestraitor_provider_api::ProviderTransportError::InvalidEvent)?;
            all_events.extend(remaining);

            // Extract usage for cost recording.
            let completed_at = chrono::Utc::now();
            for event in &all_events {
                if let ModelEvent::Usage { token_count } = event {
                    self.record_cost(
                        &request_id,
                        model_id,
                        *token_count,
                        started_at,
                        completed_at,
                    );
                }
            }

            Ok(Box::new(all_events.into_iter().map(Ok)))
        } else {
            let response = self.send_chat_completion(&request).await.map_err(|_| {
                orchestraitor_provider_api::ProviderTransportError::RequestFailed {
                    provider_id: self.descriptor.id.clone(),
                }
            })?;

            // Extract usage for cost recording before consuming.
            let usage = response.usage.as_ref().map(|u| TokenCount {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cached_tokens: 0,
                reasoning_tokens: 0,
            });
            let completed_at = chrono::Utc::now();
            if let Some(usage) = usage {
                self.record_cost(&request_id, model_id, usage, started_at, completed_at);
            }

            let events = response_to_events(&response);
            Ok(Box::new(events.into_iter().map(Ok)))
        }
    }

    async fn count_tokens(
        &self,
        _request: TokenCountRequest,
    ) -> ProviderResult<Option<TokenCount>> {
        // Neuralwatt does not expose a token-counting endpoint.
        Ok(None)
    }

    async fn health(&self) -> ProviderResult<ProviderHealth> {
        match self.fetch_models().await {
            Ok(_) => Ok(ProviderHealth {
                status: ProviderHealthStatus::Healthy,
                message: None,
            }),
            Err(e) => {
                warn!(error = %e, "neuralwatt health check failed");
                Ok(ProviderHealth {
                    status: ProviderHealthStatus::Unhealthy,
                    message: Some("neuralwatt endpoint unreachable".to_string()),
                })
            }
        }
    }
}

/// Builds a cost entry from attribution and completion input.
fn build_cost_entry(
    attribution: &CostAttribution,
    input: &CompletionCostInput<'_>,
    model: &str,
    provider: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    completed_at: chrono::DateTime<chrono::Utc>,
) -> orchestraitor_cost_ledger::CostEntry {
    let wall_ms = completed_at
        .signed_duration_since(started_at)
        .num_milliseconds()
        .max(0);
    orchestraitor_cost_ledger::CostEntry {
        model: ModelId::from_string(model.to_owned()),
        provider: ProviderId::from_string(provider.to_owned()),
        agent_domain_id: attribution.agent_domain_id.clone(),
        role: attribution.role.clone(),
        project: attribution.project.clone(),
        session: attribution.session.clone(),
        repository: attribution.repository.clone(),
        input_tokens: input.usage.input_tokens,
        output_tokens: input.usage.output_tokens,
        reasoning_tokens: input.usage.reasoning_tokens,
        cache_read_tokens: input.usage.cached_tokens,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: input.request_id.to_owned(),
        parent_request_id: None,
        started_at,
        completed_at,
        wall_ms: wall_ms.try_into().unwrap_or(0),
        monetary_cost_measured: None,
        monetary_cost_estimated: None,
        monetary_cost_basis: orchestraitor_cost_ledger::MonetaryCostBasis::UtilizationOnly,
        subscription_attribution_id: None,
        routing_decision: input.routing_decision.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn default_base_url_is_neuralwatt_com() {
        let config = NeuralwattConfig::new();
        let transport = NeuralwattTransport::with_key(
            config,
            secrecy::SecretString::from("test-key".to_string()),
        )
        .unwrap();
        assert!(transport.base_url().contains("neuralwatt.com"));
        assert!(!transport.base_url().contains("bigmodel.cn"));
    }

    #[test]
    fn descriptor_uses_openai_chat_completions_protocol() {
        let config = NeuralwattConfig::new();
        let transport = NeuralwattTransport::with_key(
            config,
            secrecy::SecretString::from("test-key".to_string()),
        )
        .unwrap();
        assert_eq!(
            transport.descriptor().protocol,
            ProviderProtocol::OpenAiChatCompletions
        );
        assert_eq!(transport.descriptor().id.as_str(), "neuralwatt");
    }
}

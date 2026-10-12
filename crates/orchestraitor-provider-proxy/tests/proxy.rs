//! Integration tests for the provider-compatible proxy.

#![allow(clippy::unwrap_used)]

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use orchestraitor_cost_ledger::CostLedger;
use orchestraitor_model::{AgentId, ModelId, ProviderId};
use orchestraitor_provider_api::{
    CapabilitySupport, DiscoveredModel, ModelEvent, ModelEventStream, ModelMetadataSource,
    ModelRequest, ProviderCapabilities, ProviderDescriptor, ProviderHealth, ProviderHealthStatus,
    ProviderProtocol, ProviderResult, ProviderTransport, TokenCount, TokenCountRequest,
};
use orchestraitor_provider_proxy::{
    ChildEnvironment, CostAttribution, LocalTokenIssuer, ProtocolSurface, ProviderRegistry,
    ProviderRoute, ProxyHttpService, TRUST_BOUNDARY_HEADER,
};
use tower::Service;

#[tokio::test]
async fn proxy_returns_200_with_mock_provider() {
    // Given: a proxy with one short-lived local token and one mock OpenAI-compatible provider.
    let ledger = Arc::new(Mutex::new(CostLedger::open_in_memory().unwrap()));
    let service = proxy_service(Arc::clone(&ledger));
    let token = service.issue_local_token().unwrap();
    let request = completion_request(token.as_str(), "/v1/chat/completions");

    // When: a harness calls the OpenAI Chat Completions surface.
    let mut service = service;
    let response = service.call(request).await.unwrap();

    // Then: the proxy returns a compatible success response with explicit trust boundary.
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(TRUST_BOUNDARY_HEADER).unwrap(),
        "provider-proxy-only; no filesystem-or-shell-containment"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["choices"][0]["message"]["content"], "hello from mock");
}

#[tokio::test]
async fn responses_and_anthropic_surfaces_return_200_with_mock_provider() {
    // Given: a proxy with OpenAI Responses and Anthropic Messages routes.
    let ledger = Arc::new(Mutex::new(CostLedger::open_in_memory().unwrap()));
    let service = proxy_service(Arc::clone(&ledger));
    let responses_token = service.issue_local_token().unwrap();
    let anthropic_token = service.issue_local_token().unwrap();

    // When: both compatibility surfaces are called.
    let mut service = service;
    let responses = service
        .call(responses_request(responses_token.as_str()))
        .await
        .unwrap();
    let anthropic = service
        .call(completion_request(anthropic_token.as_str(), "/v1/messages"))
        .await
        .unwrap();

    // Then: both are accepted and routed through the mock provider.
    assert_eq!(responses.status(), StatusCode::OK);
    assert_eq!(anthropic.status(), StatusCode::OK);
}

#[tokio::test]
async fn models_surface_lists_configured_routes() {
    // Given: a proxy with configured model routes.
    let ledger = Arc::new(Mutex::new(CostLedger::open_in_memory().unwrap()));
    let service = proxy_service(ledger);
    let token = service.issue_local_token().unwrap();

    // When: a harness calls `/v1/models`.
    let mut service = service;
    let response = service.call(models_request(token.as_str())).await.unwrap();

    // Then: the model list contains explicit route metadata and trust-boundary reporting.
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["object"], "list");
    assert_eq!(json["data"].as_array().unwrap().len(), 3);
    assert_eq!(
        json["orchestraitor_trust_boundary"]["status"],
        "provider-proxy-only"
    );
}

#[test]
fn upstream_token_never_enters_child_environment() {
    // Given: an upstream token name/value that must remain proxy-private.
    let child_environment = ChildEnvironment::provider_proxy_child();
    let mut command = Command::new("/usr/bin/env");
    child_environment.apply_to_command(&mut command);

    // When: a child process is launched with the proxy child environment.
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Then: upstream credential variables are absent from the child environment.
    assert!(!child_environment.contains_key("UPSTREAM_API_KEY"));
    assert!(!stdout.contains("UPSTREAM_API_KEY"));
    assert!(!stdout.contains("sk-upstream-secret"));
}

#[tokio::test]
async fn cost_entries_has_one_row_per_completion() {
    // Given: a proxy with a shared in-memory cost ledger.
    let ledger = Arc::new(Mutex::new(CostLedger::open_in_memory().unwrap()));
    let service = proxy_service(Arc::clone(&ledger));
    let first = service.issue_local_token().unwrap();
    let second = service.issue_local_token().unwrap();

    // When: two completions pass through the proxy.
    let mut service = service;
    let first_response = service
        .call(completion_request(first.as_str(), "/v1/chat/completions"))
        .await
        .unwrap();
    let second_response = service
        .call(completion_request(second.as_str(), "/v1/chat/completions"))
        .await
        .unwrap();

    // Then: the cost ledger has exactly one cost-entry request per completion.
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(second_response.status(), StatusCode::OK);
    let ledger = ledger.lock().unwrap();
    let rollup = ledger
        .api_spend()
        .domain_rollup(&AgentId::from_string("provider-proxy".to_owned()))
        .unwrap()
        .unwrap();
    assert_eq!(rollup.request_count, 2);
}

fn proxy_service(ledger: Arc<Mutex<CostLedger>>) -> ProxyHttpService {
    let model_id = ModelId::from_string("mock-model".to_owned());
    let chat_provider = ProviderId::from_string("mock-chat".to_owned());
    let responses_provider = ProviderId::from_string("mock-responses".to_owned());
    let anthropic_provider = ProviderId::from_string("mock-anthropic".to_owned());
    let registry = ProviderRegistry::new()
        .with_transport(Arc::new(MockProvider::new(
            chat_provider.clone(),
            ProviderProtocol::OpenAiChatCompletions,
        )))
        .with_transport(Arc::new(MockProvider::new(
            responses_provider.clone(),
            ProviderProtocol::OpenAiResponses,
        )))
        .with_transport(Arc::new(MockProvider::new(
            anthropic_provider.clone(),
            ProviderProtocol::AnthropicMessages,
        )))
        .with_route(
            ProtocolSurface::OpenAiChatCompletions,
            ProviderRoute {
                provider_id: chat_provider,
                model_id: model_id.clone(),
                routing_decision: "test-route".to_owned(),
            },
        )
        .with_route(
            ProtocolSurface::OpenAiResponses,
            ProviderRoute {
                provider_id: responses_provider,
                model_id: model_id.clone(),
                routing_decision: "test-route".to_owned(),
            },
        )
        .with_route(
            ProtocolSurface::AnthropicMessages,
            ProviderRoute {
                provider_id: anthropic_provider,
                model_id,
                routing_decision: "test-route".to_owned(),
            },
        );
    ProxyHttpService::new(
        registry,
        LocalTokenIssuer::new(Duration::from_mins(1)),
        ledger,
        CostAttribution::local_proxy_defaults(),
    )
}

fn completion_request(token: &str, path: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(hyper::header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Full::new(Bytes::from_static(
            br#"{"model":"mock-model","messages":[{"role":"user","content":"hi"}]}"#,
        )))
        .unwrap()
}

fn responses_request(token: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/responses")
        .header(hyper::header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Full::new(Bytes::from_static(
            br#"{"model":"mock-model","input":"hi"}"#,
        )))
        .unwrap()
}

fn models_request(token: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/models")
        .header(hyper::header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

struct MockProvider {
    descriptor: ProviderDescriptor,
}

impl MockProvider {
    fn new(provider_id: ProviderId, protocol: ProviderProtocol) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: provider_id,
                display_name: "Mock".to_owned(),
                protocol,
                capabilities: ProviderCapabilities {
                    tool_choice: CapabilitySupport::Supported,
                    ..ProviderCapabilities::default()
                },
            },
        }
    }
}

#[async_trait]
impl ProviderTransport for MockProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn list_models(&self) -> ProviderResult<Vec<DiscoveredModel>> {
        Ok(vec![DiscoveredModel {
            provider_id: self.descriptor.id.clone(),
            model_id: ModelId::from_string("mock-model".to_owned()),
            wire_model_id: None,
            display_name: Some("Mock model".to_owned()),
            context_window: Some(128_000),
            max_output_tokens: Some(4096),
            capabilities: self.descriptor.capabilities,
            metadata_source: ModelMetadataSource::ExplicitConfig,
        }])
    }

    async fn stream(&self, _request: ModelRequest) -> ProviderResult<ModelEventStream> {
        Ok(Box::new(
            vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::TextDelta {
                    text: "hello from mock".to_owned(),
                }),
                Ok(ModelEvent::Usage {
                    token_count: TokenCount {
                        input_tokens: 3,
                        output_tokens: 4,
                        cached_tokens: 0,
                        reasoning_tokens: 1,
                    },
                }),
                Ok(ModelEvent::Completed),
            ]
            .into_iter(),
        ))
    }

    async fn count_tokens(
        &self,
        _request: TokenCountRequest,
    ) -> ProviderResult<Option<TokenCount>> {
        Ok(None)
    }

    async fn health(&self) -> ProviderResult<ProviderHealth> {
        Ok(ProviderHealth {
            status: ProviderHealthStatus::Healthy,
            message: None,
        })
    }
}

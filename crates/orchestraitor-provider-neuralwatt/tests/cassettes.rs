//! Wire-level cassette tests for the Neuralwatt provider adapter.
//!
//! These tests replay recorded HTTP responses from the Neuralwatt
//! `OpenAI` Chat Completions-compatible API. No live provider is contacted.
//! The mock HTTP server is built on raw TCP to avoid adding test
//! dependencies.

#![allow(clippy::unwrap_used)]

mod mock_server;

use std::sync::Arc;

use orchestraitor_model::{ModelId, ProviderId};
use orchestraitor_provider_api::{
    MessageRole, ModelMessage, ModelRequest,
    transport::{ModelEvent, ProviderProtocol, ProviderTransport},
};
use orchestraitor_provider_neuralwatt::{
    NeuralwattTransport,
    config::{DEFAULT_NEURALWATT_BASE_URL, NeuralwattConfig},
    cost::InMemoryCostSink,
};
use secrecy::SecretString;

use mock_server::MockServer;

const TEST_API_KEY: &str = "test-neuralwatt-key";

fn make_transport(base_url: String, cost_sink: Arc<InMemoryCostSink>) -> NeuralwattTransport {
    NeuralwattTransport::with_key(
        NeuralwattConfig::with_endpoint(base_url, "secret://env/NEURALWATT_API_KEY".to_string())
            .unwrap(),
        SecretString::from(TEST_API_KEY.to_string()),
    )
    .unwrap()
    .with_cost_sink(cost_sink)
}

fn make_chat_request(stream: bool) -> ModelRequest {
    let mut extensions = serde_json::Map::new();
    extensions.insert("stream".to_string(), serde_json::json!(stream));
    ModelRequest {
        provider_id: ProviderId::from_string("neuralwatt".to_string()),
        model_id: ModelId::from_string("glm-5.2".to_string()),
        messages: vec![ModelMessage {
            role: MessageRole::User,
            content: "Hello, who are you?".to_string(),
        }],
        max_output_tokens: Some(1024),
        temperature: Some(0.7),
        reasoning: None,
        structured_output: None,
        tool_choice: None,
        extensions,
    }
}

fn make_tool_call_request() -> ModelRequest {
    let mut extensions = serde_json::Map::new();
    extensions.insert(
        "tools".to_string(),
        serde_json::json!([{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": { "type": "string" },
                        "unit": { "type": "string" }
                    },
                    "required": ["location"]
                }
            }
        }]),
    );
    extensions.insert("stream".to_string(), serde_json::json!(false));
    ModelRequest {
        provider_id: ProviderId::from_string("neuralwatt".to_string()),
        model_id: ModelId::from_string("glm-5.2".to_string()),
        messages: vec![ModelMessage {
            role: MessageRole::User,
            content: "What's the weather in San Francisco?".to_string(),
        }],
        max_output_tokens: Some(1024),
        temperature: None,
        reasoning: None,
        structured_output: None,
        tool_choice: None,
        extensions,
    }
}

#[tokio::test]
async fn cassette_models_endpoint_returns_expected_list() {
    let server = MockServer::start(
        include_str!("cassettes/models.json"),
        "application/json",
        false,
    );
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), cost_sink);

    let models = transport.list_models().await.unwrap();

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_id.as_str(), "glm-5.2");
    assert_eq!(models[0].wire_model_id.as_deref(), Some("glm-5.2"));
    assert_eq!(models[1].model_id.as_str(), "glm-5.2-flash");
}

#[tokio::test]
async fn cassette_non_streaming_chat_returns_expected_response() {
    let body = include_str!("cassettes/chat_non_stream.json");
    let server = MockServer::start(body, "application/json", false);
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), cost_sink);

    let request = make_chat_request(false);
    let stream = transport.stream(request).await.unwrap();
    let events: Vec<_> = stream.filter_map(Result::ok).collect();

    assert!(events.iter().any(|e| matches!(e, ModelEvent::Started)));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("GLM-5.2"));
    assert!(text.contains("Neuralwatt"));
    assert!(events.iter().any(|e| matches!(e, ModelEvent::Completed)));
}

#[tokio::test]
async fn cassette_non_streaming_chat_records_cost_entry() {
    let body = include_str!("cassettes/chat_non_stream.json");
    let server = MockServer::start(body, "application/json", false);
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), Arc::clone(&cost_sink));

    let request = make_chat_request(false);
    let stream = transport.stream(request).await.unwrap();
    let _events: Vec<_> = stream.filter_map(Result::ok).collect();

    assert_eq!(
        cost_sink.len(),
        1,
        "exactly one cost entry should be recorded"
    );
    let entry = &cost_sink.entries()[0];
    assert_eq!(entry.input_tokens, 12);
    assert_eq!(entry.output_tokens, 18);
    assert_eq!(entry.model.as_str(), "neuralwatt");
    assert_eq!(entry.provider.as_str(), "neuralwatt");
}

#[tokio::test]
async fn cassette_streaming_chat_consumes_all_chunks() {
    let body = include_str!("cassettes/chat_stream.sse");
    let server = MockServer::start(body, "text/event-stream", false);
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), cost_sink);

    let request = make_chat_request(true);
    let stream = transport.stream(request).await.unwrap();
    let events: Vec<_> = stream.filter_map(Result::ok).collect();

    assert!(events.iter().any(|e| matches!(e, ModelEvent::Started)));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello from Neuralwatt GLM-5.2!");
    assert!(events.iter().any(|e| matches!(e, ModelEvent::Completed)));
    let usage_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::Usage { token_count } => Some(*token_count),
            _ => None,
        })
        .collect();
    assert_eq!(usage_events.len(), 1);
    assert_eq!(usage_events[0].input_tokens, 8);
    assert_eq!(usage_events[0].output_tokens, 7);
}

#[tokio::test]
async fn cassette_streaming_chat_records_cost_entry() {
    let body = include_str!("cassettes/chat_stream.sse");
    let server = MockServer::start(body, "text/event-stream", false);
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), Arc::clone(&cost_sink));

    let request = make_chat_request(true);
    let stream = transport.stream(request).await.unwrap();
    let _events: Vec<_> = stream.filter_map(Result::ok).collect();

    assert_eq!(
        cost_sink.len(),
        1,
        "streaming call should record a cost entry"
    );
    let entry = &cost_sink.entries()[0];
    assert_eq!(entry.input_tokens, 8);
    assert_eq!(entry.output_tokens, 7);
}

#[tokio::test]
async fn cassette_tool_calls_returned_in_non_streaming_response() {
    let body = include_str!("cassettes/chat_tool_calls.json");
    let server = MockServer::start(body, "application/json", false);
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), cost_sink);

    let request = make_tool_call_request();
    let stream = transport.stream(request).await.unwrap();
    let events: Vec<_> = stream.filter_map(Result::ok).collect();

    let tool_calls: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        })
        .collect();

    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].0, "call_neuralwatt_001");
    assert_eq!(tool_calls[0].1, "get_weather");
    assert_eq!(tool_calls[0].2["location"].as_str(), Some("San Francisco"));
    assert_eq!(tool_calls[0].2["unit"].as_str(), Some("celsius"));
}

#[tokio::test]
async fn default_base_url_is_neuralwatt_com_never_bigmodel_cn() {
    assert!(DEFAULT_NEURALWATT_BASE_URL.contains("neuralwatt.com"));
    assert!(!DEFAULT_NEURALWATT_BASE_URL.contains("bigmodel.cn"));

    let config = NeuralwattConfig::new();
    assert!(config.base_url().contains("neuralwatt.com"));
    assert!(!config.base_url().contains("bigmodel.cn"));

    let transport =
        NeuralwattTransport::with_key(config, SecretString::from(TEST_API_KEY.to_string()))
            .unwrap();
    assert!(transport.base_url().contains("neuralwatt.com"));
    assert!(!transport.base_url().contains("bigmodel.cn"));
}

#[tokio::test]
async fn transport_descriptor_uses_openai_chat_completions_protocol() {
    let config = NeuralwattConfig::new();
    let transport =
        NeuralwattTransport::with_key(config, SecretString::from(TEST_API_KEY.to_string()))
            .unwrap();
    let descriptor = transport.descriptor();
    assert_eq!(descriptor.protocol, ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(descriptor.id.as_str(), "neuralwatt");
}

#[tokio::test]
async fn health_check_returns_healthy_when_models_endpoint_responds() {
    let body = include_str!("cassettes/models.json");
    let server = MockServer::start(body, "application/json", false);
    let cost_sink = Arc::new(InMemoryCostSink::new());
    let transport = make_transport(server.url(), cost_sink);

    let health = transport.health().await.unwrap();
    assert_eq!(
        health.status,
        orchestraitor_provider_api::transport::ProviderHealthStatus::Healthy
    );
}

//! Wire-level cassette tests for the Clef Flash decision provider
//! (`NeuralwattDecisionProvider`, spec `30-model-routing.md` §9.45).
//!
//! Shapes replayed here were captured live from
//! `POST https://api.neuralwatt.com/v1/systemone` with model `clef-flash`
//! (2026-10-07). No live provider is contacted: the mock HTTP server is
//! built on raw TCP like the transport cassettes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
// Test-only allowances mirror `tests/cassettes.rs`: a failed expectation must
// fail the test loudly, and `unwrap_err` needs `Debug` on the provider (which
// deliberately does not implement it — the API key lives behind `secrecy`).
#![allow(clippy::panic)]

mod mock_server;

use orchestraitor_provider_api::{DecisionProvider, DecisionProviderError};
use orchestraitor_provider_neuralwatt::{
    DEFAULT_DECISION_MODEL, NEURALWATT_DECISION_PROVIDER_ID, NeuralwattDecisionProvider,
    config::NeuralwattConfig,
};
use secrecy::SecretString;

use mock_server::MockServer;

const TEST_API_KEY: &str = "test-neuralwatt-key";

fn make_provider(url: String) -> NeuralwattDecisionProvider {
    let config =
        NeuralwattConfig::with_endpoint(url, "secret://env/NEURALWATT_API_KEY".to_string())
            .unwrap();
    NeuralwattDecisionProvider::with_key(config, SecretString::from(TEST_API_KEY.to_string()))
        .unwrap()
}

/// Live response shape: a `choice` answer over ready-task ids.
const CHOICE_BODY: &str = r#"{"id":"systemone-1","object":"systemone","created":0,"model":"clef-flash","answers":{"pick":{"type":"choice","choice":"board--42","confidence":0.9875,"probabilities":{"board--42":0.9875,"board--9":0.007}}},"truncated":false,"usage":{"input_tokens":138,"output_tokens":0}}"#;

/// Live response shape: a `noul` probability answer.
const NOUL_BODY: &str = r#"{"id":"systemone-2","object":"systemone","created":0,"model":"clef-flash","answers":{"split":{"type":"noul","noul":0.8439}},"truncated":false,"usage":{"input_tokens":162,"output_tokens":0}}"#;

/// Live response shape: a low `noul` probability (no split).
const NOUL_LOW_BODY: &str = r#"{"id":"systemone-3","object":"systemone","created":0,"model":"clef-flash","answers":{"split":{"type":"noul","noul":0.12}},"truncated":false,"usage":{"input_tokens":150,"output_tokens":0}}"#;

#[tokio::test]
async fn task_selection_parses_choice_answer_into_typed_selection() {
    let server = MockServer::start(CHOICE_BODY, "application/json", false);
    let provider = make_provider(server.url());

    let selection = provider
        .propose_task_selection(&["board--42".to_string(), "board--9".to_string()])
        .await
        .unwrap();

    assert_eq!(selection.task_id, "board--42");
    assert!((selection.confidence - 0.9875).abs() < 1e-9);
    assert_eq!(provider.id().as_str(), NEURALWATT_DECISION_PROVIDER_ID);
    assert_eq!(provider.decision_model(), DEFAULT_DECISION_MODEL);
}

#[tokio::test]
async fn task_selection_rejects_ids_outside_the_ready_queue() {
    // The mock always answers `board--77`, which is not in the queue.
    let server = MockServer::start(CHOICE_BODY, "application/json", false);
    let provider = make_provider(server.url());

    let error = provider
        .propose_task_selection(&["board--9".to_string()])
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DecisionProviderError::Unavailable { reason, .. }
            if reason.contains("not in the ready queue")),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn task_selection_with_empty_queue_is_unavailable_without_a_request() {
    let server = MockServer::start(CHOICE_BODY, "application/json", false);
    let provider = make_provider(server.url());

    let error = provider.propose_task_selection(&[]).await.unwrap_err();
    assert!(
        matches!(&error, DecisionProviderError::Unavailable { reason, .. }
        if reason.contains("empty ready queue"))
    );
}

#[tokio::test]
async fn transport_failure_maps_to_unavailable_not_a_loop_error() {
    // Port 1 is a closed port: the connection refuses, exercising the
    // transport-failure path end to end.
    let provider = make_provider("http://127.0.0.1:1/v1".to_string());
    let error = provider
        .propose_task_selection(&["board--42".to_string()])
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DecisionProviderError::Unavailable { provider_id, reason }
            if provider_id.as_str() == "neuralwatt-clef-flash"
                && reason.contains("HTTP request failed")),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn malformed_response_maps_to_unavailable() {
    // A chat-completion body is not a systemone body: answers are missing.
    let server = MockServer::start(
        r#"{"id":"x","object":"chat.completion","choices":[]}"#,
        "application/json",
        false,
    );
    let provider = make_provider(server.url());
    let error = provider
        .propose_task_selection(&["board--42".to_string()])
        .await
        .unwrap_err();
    assert!(matches!(error, DecisionProviderError::Unavailable { .. }));
}

#[tokio::test]
async fn role_resolution_parses_choice_over_candidates() {
    let body = r#"{"id":"systemone-4","object":"systemone","created":0,"model":"clef-flash","answers":{"pick":{"type":"choice","choice":"neuralwatt/glm-5.3-flash","confidence":0.91,"probabilities":{"neuralwatt/glm-5.3-flash":0.91,"neuralwatt/glm-5.3":0.09}}},"truncated":false,"usage":{"input_tokens":120,"output_tokens":0}}"#;
    let server = MockServer::start(body, "application/json", false);
    let provider = make_provider(server.url());

    let proposal = provider.propose_role_resolution("implement").await.unwrap();
    assert_eq!(proposal.role, "implement");
    assert_eq!(proposal.provider.as_str(), "neuralwatt");
    assert_eq!(proposal.model, "glm-5.3-flash");
    assert!((proposal.confidence - 0.91).abs() < 1e-9);
    assert_eq!(proposal.alternatives.len(), 1);
    assert_eq!(proposal.alternatives[0].model, "glm-5.3");
}

#[tokio::test]
async fn role_resolution_rejects_candidates_outside_the_set() {
    let body = r#"{"id":"systemone-5","object":"systemone","created":0,"model":"clef-flash","answers":{"pick":{"type":"choice","choice":"neuralwatt/glm-9.9","confidence":0.99,"probabilities":{}}},"truncated":false,"usage":{"input_tokens":120,"output_tokens":0}}"#;
    let server = MockServer::start(body, "application/json", false);
    let provider = make_provider(server.url());
    let error = provider.propose_role_resolution("plan").await.unwrap_err();
    assert!(
        matches!(&error, DecisionProviderError::Unavailable { reason, .. }
            if reason.contains("not in the candidate set")),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn task_split_high_probability_proposes_a_split_shape() {
    // First call (noul 0.8439) crosses the 0.5 gate, second call picks the
    // `pair` shape — the mock replays by question name, so serve a body
    // containing both answers and let each request read its own key.
    let body = r#"{"id":"systemone-6","object":"systemone","created":0,"model":"clef-flash","answers":{"split":{"type":"noul","noul":0.8439},"shape":{"type":"choice","choice":"pair","confidence":0.77,"probabilities":{"pair":0.77,"triple":0.23}}},"truncated":false,"usage":{"input_tokens":190,"output_tokens":0}}"#;
    let server = MockServer::start(body, "application/json", false);
    let provider = make_provider(server.url());

    let task = orchestraitor_provider_api::TaskSummary {
        task_id: "board--42".to_string(),
        title: "write the file".to_string(),
        description: "inert".to_string(),
    };
    let proposal = provider.propose_task_split(&task).await.unwrap();
    assert!(proposal.split);
    assert_eq!(proposal.subtasks.len(), 2);
    assert!((proposal.confidence - 0.8439).abs() < 1e-9);
}

#[tokio::test]
async fn task_split_low_probability_declines_without_subtasks() {
    let server = MockServer::start(NOUL_LOW_BODY, "application/json", false);
    let provider = make_provider(server.url());

    let task = orchestraitor_provider_api::TaskSummary {
        task_id: "board--9".to_string(),
        title: "update docs".to_string(),
        description: "inert".to_string(),
    };
    let proposal = provider.propose_task_split(&task).await.unwrap();
    assert!(!proposal.split);
    assert!(proposal.subtasks.is_empty());
    assert!((proposal.confidence - 0.88).abs() < 1e-9);
}

#[tokio::test]
async fn tool_selection_parses_choice_over_tool_names() {
    let body = r#"{"id":"systemone-7","object":"systemone","created":0,"model":"clef-flash","answers":{"pick":{"type":"choice","choice":"board.query","confidence":0.66,"probabilities":{"board.query":0.66}}},"truncated":false,"usage":{"input_tokens":110,"output_tokens":0}}"#;
    let server = MockServer::start(body, "application/json", false);
    let provider = make_provider(server.url());

    let query = orchestraitor_provider_api::ToolQueryContext {
        query: "find the current board state".to_string(),
        available_tools: vec![
            orchestraitor_provider_api::ToolDescriptor {
                name: "board.query".to_string(),
                description: "query the project board".to_string(),
            },
            orchestraitor_provider_api::ToolDescriptor {
                name: "fs.write".to_string(),
                description: "write a file".to_string(),
            },
        ],
    };
    let selection = provider.propose_tool_selection(&query).await.unwrap();
    assert_eq!(selection.selected_tools, vec!["board.query".to_string()]);
    assert!((selection.confidence - 0.66).abs() < 1e-9);
}

#[tokio::test]
async fn tool_selection_rejects_unknown_tool_names() {
    let body = r#"{"id":"systemone-8","object":"systemone","created":0,"model":"clef-flash","answers":{"pick":{"type":"choice","choice":"net.send","confidence":0.9,"probabilities":{}}},"truncated":false,"usage":{"input_tokens":110,"output_tokens":0}}"#;
    let server = MockServer::start(body, "application/json", false);
    let provider = make_provider(server.url());

    let query = orchestraitor_provider_api::ToolQueryContext {
        query: "q".to_string(),
        available_tools: vec![orchestraitor_provider_api::ToolDescriptor {
            name: "board.query".to_string(),
            description: "query the board".to_string(),
        }],
    };
    let Err(error) = provider.propose_tool_selection(&query).await else {
        panic!("selection of an unknown tool must be a typed error")
    };
    assert!(
        matches!(&error, DecisionProviderError::Unavailable { reason, .. }
            if reason.contains("not in the query context")),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn split_request_without_shape_answer_is_unavailable() {
    // The noul gate passes (0.8439) but the shape choice is missing from
    // `answers`: the missing-answer path must be a typed unavailability.
    let server = MockServer::start(NOUL_BODY, "application/json", false);
    let provider = make_provider(server.url());
    let task = orchestraitor_provider_api::TaskSummary {
        task_id: "board--1".to_string(),
        title: "t".to_string(),
        description: "d".to_string(),
    };
    // First request (split noul) reads `split`; the mock always answers the
    // same body, so the second (shape) request finds no `shape` answer.
    let error = provider.propose_task_split(&task).await.unwrap_err();
    assert!(
        matches!(&error, DecisionProviderError::Unavailable { reason, .. }
            if reason.contains("shape")),
        "unexpected error: {error}"
    );
}

#[test]
fn base_url_parse_failure_fails_at_construction() {
    // `NeuralwattConfig::with_endpoint` rejects hostless URLs before the
    // provider constructor runs, so configuration-time failure is the
    // documented surface.
    let error = NeuralwattConfig::with_endpoint(
        "not a url".to_string(),
        "secret://env/NEURALWATT_API_KEY".to_string(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("invalid base URL"));
}

#[tokio::test]
async fn no_auth_endpoint_sends_no_authorization_header() {
    // A self-hosted decision engine (spec §10.3 local endpoint): the mock
    // records whether an Authorization header arrived. A `None` read means
    // the exchange never completed (test fails on the assertion below).
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    let seen_auth: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let seen_auth_clone = Arc::clone(&seen_auth);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let read = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            if let Ok(mut seen) = seen_auth_clone.lock() {
                *seen = Some(request.contains("Authorization:"));
            }
            let body = r#"{"id":"x","object":"systemone","created":0,"model":"clef-flash","answers":{"pick":{"type":"choice","choice":"board--42","confidence":0.9,"probabilities":{}}},"truncated":false,"usage":{"input_tokens":1,"output_tokens":0}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ignore = stream.write_all(response.as_bytes());
            let _ignore = stream.flush();
        }
    });

    let config = NeuralwattConfig::with_endpoint(
        format!("http://{addr}/v1"),
        // The `routing.api_key = "none"` mapping lands on this sentinel.
        "secret://none".to_string(),
    )
    .unwrap();
    let provider =
        orchestraitor_provider_neuralwatt::NeuralwattDecisionProvider::with_decision_model(
            config,
            secrecy::SecretString::from(String::new()),
            "clef-flash".to_string(),
        )
        .unwrap();
    let selection = provider
        .propose_task_selection(&["board--42".to_string()])
        .await
        .unwrap();
    assert_eq!(selection.task_id, "board--42");
    let _ignore = handle.join();
    let seen = seen_auth.lock().map(|guard| *guard).ok();
    assert_eq!(
        seen.flatten(),
        Some(false),
        "a no-auth endpoint must not receive an Authorization header"
    );
}

#[tokio::test]
async fn endpoint_override_reaches_the_wire() {
    // `routing.base_url` override: the request lands on the overridden
    // endpoint, not the default Neuralwatt API.
    let server = MockServer::start(CHOICE_BODY, "application/json", false);
    let default_config = NeuralwattConfig::new();
    let config =
        NeuralwattConfig::with_endpoint(server.url(), default_config.auth_uri().to_string())
            .unwrap();
    let provider = orchestraitor_provider_neuralwatt::NeuralwattDecisionProvider::with_endpoint(
        config,
        None,
        Some("secret://env/NEURALWATT_API_KEY"),
        "clef-flash".to_string(),
    )
    .unwrap();
    let selection = provider
        .propose_task_selection(&["board--42".to_string()])
        .await
        .unwrap();
    assert_eq!(selection.task_id, "board--42");
}

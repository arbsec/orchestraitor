//! Wire-level cassette tests for the System One decision provider
//! (`SystemOneDecisionProvider`, spec `30-model-routing.md` §9.45).
//!
//! Shapes replayed here were captured live from a System One endpoint
//! (`POST /v1/systemone`, model `clef-flash`, 2026-10-07). No live provider
//! is contacted: the mock HTTP server is built on raw TCP, so the fixtures
//! are endpoint-agnostic — any System One-compatible deployment serves the
//! same shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
// Test-only allowances mirror `tests/cassettes.rs`: a failed expectation must
// fail the test loudly, and `unwrap_err` needs `Debug` on the provider (which
// deliberately does not implement it — the API key lives behind `secrecy`).
#![allow(clippy::panic)]

mod mock_server;

use orchestraitor_provider_api::{
    DEFAULT_DECISION_MODEL, DecisionProvider, DecisionProviderError,
    SYSTEMONE_DECISION_PROVIDER_ID, SystemOneDecisionProvider, SystemOneEndpointConfig,
};
use secrecy::SecretString;

use mock_server::MockServer;

const TEST_API_KEY: &str = "test-endpoint-key";

fn make_provider(url: String) -> SystemOneDecisionProvider {
    SystemOneDecisionProvider::new(SystemOneEndpointConfig {
        base_url: url,
        model: DEFAULT_DECISION_MODEL.to_string(),
        api_key: Some(SecretString::from(TEST_API_KEY.to_string())),
    })
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
    assert_eq!(provider.id().as_str(), SYSTEMONE_DECISION_PROVIDER_ID);
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
            if provider_id.as_str() == "systemone"
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
    // A malformed `routing.base_url` fails at provider construction — the
    // configuration-time surface, before any campaign pass can start.
    let Err(error) = SystemOneDecisionProvider::new(SystemOneEndpointConfig {
        base_url: "not a url".to_string(),
        model: DEFAULT_DECISION_MODEL.to_string(),
        api_key: None,
    }) else {
        panic!("a malformed base URL must fail at construction")
    };
    assert!(error.to_string().contains("invalid systemone base URL"));
}

#[test]
fn resolve_endpoint_maps_none_and_secret_uris() {
    // `routing.api_key` mapping: absent or `none` -> no auth; a `secret://`
    // URI resolves through the standard chain.
    let none = orchestraitor_provider_api::resolve_endpoint(
        "http://127.0.0.1:1/v1".to_string(),
        None,
        Some("none"),
    )
    .unwrap();
    assert!(none.api_key.is_none());
    assert_eq!(none.model, DEFAULT_DECISION_MODEL);

    let absent = orchestraitor_provider_api::resolve_endpoint(
        "http://127.0.0.1:1/v1".to_string(),
        Some("custom-model".to_string()),
        None,
    )
    .unwrap();
    assert!(absent.api_key.is_none());
    assert_eq!(absent.model, "custom-model");

    let err = orchestraitor_provider_api::resolve_endpoint(
        "http://127.0.0.1:1/v1".to_string(),
        None,
        Some("secret://env/SYSTEMONE_TEST_UNSET_VAR_42"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("auth resolution failed"), "{err}");
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

    let provider = SystemOneDecisionProvider::new(SystemOneEndpointConfig {
        base_url: format!("http://{addr}/v1"),
        model: DEFAULT_DECISION_MODEL.to_string(),
        // The `routing.api_key` absent/`none` mapping: no auth at all.
        api_key: None,
    })
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
async fn project_endpoint_reaches_the_wire() {
    // The project-scoped `routing.base_url`: the request lands on whatever
    // endpoint the project config names — here, the localhost mock standing
    // in for any System One-compatible deployment.
    let server = MockServer::start(CHOICE_BODY, "application/json", false);
    let provider = make_provider(server.url());
    let selection = provider
        .propose_task_selection(&["board--42".to_string()])
        .await
        .unwrap();
    assert_eq!(selection.task_id, "board--42");
    assert_eq!(provider.endpoint_host(), "127.0.0.1");
}

#[tokio::test]
async fn trailing_slash_base_url_normalizes_before_path_join() {
    // A configured `routing.base_url` ending in `/` must not build
    // `<base>//systemone` (a 404 on real servers, silently degrading every
    // consultation to the heuristic fallback; the path-ignoring mock hides
    // it). The mock here rejects any path but `POST /v1/systemone`.
    let server = MockServer::start(CHOICE_BODY, "application/json", false);
    let provider = make_provider(format!("{}/", server.url()));
    let selection = provider
        .propose_task_selection(&["board--42".to_string()])
        .await
        .unwrap();
    assert_eq!(selection.task_id, "board--42");
}

#[test]
fn credential_over_cleartext_http_fails_closed() {
    // A bearer credential is never sent over plain `http` off loopback
    // (CWE-319): construction fails with the typed cleartext error.
    let Err(error) = SystemOneDecisionProvider::new(SystemOneEndpointConfig {
        base_url: "http://mekbook.tail1e276.ts.net:8080/v1".to_string(),
        model: DEFAULT_DECISION_MODEL.to_string(),
        api_key: Some(SecretString::from(TEST_API_KEY.to_string())),
    }) else {
        panic!("a credential over cleartext http must fail at construction")
    };
    assert!(
        error.to_string().contains("cleartext"),
        "unexpected error: {error}"
    );
}

#[test]
fn no_credential_over_cleartext_http_is_accepted() {
    // The documented self-hosted deployment: a no-auth tailnet endpoint
    // over plain `http` sends no credential, so it is accepted.
    let provider = SystemOneDecisionProvider::new(SystemOneEndpointConfig {
        base_url: "http://mekbook.tail1e276.ts.net:8080/v1".to_string(),
        model: DEFAULT_DECISION_MODEL.to_string(),
        api_key: None,
    })
    .unwrap();
    assert_eq!(provider.endpoint_host(), "mekbook.tail1e276.ts.net");
}

#[test]
fn credential_over_cleartext_http_to_loopback_is_accepted() {
    // Loopback is the one plain-`http` case where a credential stays
    // local: no network observer exists between this process and the
    // engine. `127.0.0.0/8` and `::1` both count.
    for (base_url, expected_host) in [
        ("http://127.0.0.1:8080/v1", "127.0.0.1"),
        ("http://[::1]:8080/v1", "[::1]"),
        ("http://localhost:8080/v1", "localhost"),
    ] {
        let provider = SystemOneDecisionProvider::new(SystemOneEndpointConfig {
            base_url: base_url.to_string(),
            model: DEFAULT_DECISION_MODEL.to_string(),
            api_key: Some(SecretString::from(TEST_API_KEY.to_string())),
        })
        .unwrap_or_else(|error| panic!("{base_url} must be accepted: {error}"));
        assert_eq!(provider.endpoint_host(), expected_host);
    }
}

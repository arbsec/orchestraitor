//! Fixture-driven integration tests for the board client against a scripted
//! in-process GraphQL server. No live network: every test serves recorded
//! response shapes over `127.0.0.1` (spec §21.3).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_trait::async_trait;
use orchestraitor_board::cache::FieldNodeIds;
use orchestraitor_board::{
    BoardAuth, BoardClient, BoardError, BoardProjectConfig, NodeIdCacheFile, ProjectNodeIds,
};
use secrecy::SecretString;

const RESOLVE_PROJECT: &str = include_str!("fixtures/resolve-project.json");
const RESOLVE_FIELDS: &str = include_str!("fixtures/resolve-fields.json");
const ITEMS: &str = include_str!("fixtures/items.json");
const FIND_ITEM: &str = include_str!("fixtures/find-item.json");

const TOKEN: &str = "fixture-token-value";

/// Injected auth returning the fixture token.
struct StaticAuth;

#[async_trait]
impl BoardAuth for StaticAuth {
    async fn bearer_token(&self) -> Result<SecretString, BoardError> {
        Ok(SecretString::from(TOKEN.to_string()))
    }
}

struct Request {
    authorization: Option<String>,
    body: String,
}

enum RuleResponse {
    Json(&'static str),
    Status(u16),
}

struct Rule {
    needle: &'static str,
    response: RuleResponse,
}

struct ScriptServer {
    endpoint: String,
    requests: Arc<Mutex<Vec<Request>>>,
    shutdown: Arc<Mutex<bool>>,
    join: Option<JoinHandle<()>>,
}

impl ScriptServer {
    fn start(rules: Vec<Rule>) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}/graphql", listener.local_addr()?);
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_shutdown = Arc::clone(&shutdown);
        let join = thread::spawn(move || {
            loop {
                if let Ok(guard) = thread_shutdown.lock()
                    && *guard
                {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        serve_connection(stream, &rules, &thread_requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        Ok(Self {
            endpoint,
            requests,
            shutdown,
            join: Some(join),
        })
    }

    fn recorded(&self) -> Vec<(Option<String>, String)> {
        match self.requests.lock() {
            Ok(guard) => guard
                .iter()
                .map(|request| (request.authorization.clone(), request.body.clone()))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Drop for ScriptServer {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.shutdown.lock() {
            *guard = true;
        }
        if let Some(join) = self.join.take() {
            let _ignore = join.join();
        }
    }
}

fn serve_connection(
    mut stream: std::net::TcpStream,
    rules: &[Rule],
    requests: &Arc<Mutex<Vec<Request>>>,
) {
    let _ignore = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut header_bytes = Vec::new();
    let mut body = Vec::new();
    {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(clone) => clone,
            Err(_) => return,
        });
        let mut content_length = 0_usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let trimmed = line.trim_end();
            if let Some(value) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
            header_bytes.extend_from_slice(line.as_bytes());
            if trimmed.is_empty() {
                break;
            }
        }
        body.resize(content_length, 0);
        if reader.read_exact(&mut body).is_err() {
            return;
        }
    }
    let headers = String::from_utf8_lossy(&header_bytes).to_lowercase();
    let authorization = headers
        .lines()
        .find_map(|line| line.strip_prefix("authorization:").map(str::trim))
        .map(ToOwned::to_owned);
    let body_text = String::from_utf8_lossy(&body).into_owned();
    if let Ok(mut guard) = requests.lock() {
        guard.push(Request {
            authorization,
            body: body_text.clone(),
        });
    }
    let response = match rules.iter().find(|rule| body_text.contains(rule.needle)) {
        Some(Rule {
            response: RuleResponse::Json(payload),
            ..
        }) => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            payload.len(),
            payload
        ),
        Some(Rule {
            response: RuleResponse::Status(status),
            ..
        }) => format!("HTTP/1.1 {status} Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
        None => {
            "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_string()
        }
    };
    let _ignore = stream.write_all(response.as_bytes());
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(source: std::io::Error) -> BoardError {
    BoardError::CacheIo {
        path: std::path::PathBuf::from("<test-io>"),
        source,
    }
}

fn config_fixture() -> BoardProjectConfig {
    BoardProjectConfig {
        organization: "arbsec".to_string(),
        project_number: 1,
        repos: vec!["arbsec/orchestraitor".to_string()],
        leaf_types: vec!["Task".to_string(), "Bug".to_string()],
        target_field: "Target".to_string(),
        target_value: "MVP".to_string(),
        ready_field: "Status".to_string(),
        ready_value: "Ready".to_string(),
        token_uri: None,
    }
}

fn full_ready_rules() -> Vec<Rule> {
    vec![
        Rule {
            needle: "projectV2(number",
            response: RuleResponse::Json(RESOLVE_PROJECT),
        },
        Rule {
            needle: "fields(first",
            response: RuleResponse::Json(RESOLVE_FIELDS),
        },
        Rule {
            needle: "items(first",
            response: RuleResponse::Json(ITEMS),
        },
    ]
}

fn client_on(
    server: &ScriptServer,
    cache_path: Option<std::path::PathBuf>,
) -> Result<BoardClient, BoardError> {
    let auth: Arc<dyn BoardAuth> = Arc::new(StaticAuth);
    Ok(BoardClient::with_endpoint(server.endpoint.clone(), auth)?.with_cache_path(cache_path))
}

#[tokio::test]
async fn ready_queue_fixture_filters_exact_eligibility() -> Result<(), BoardError> {
    let server = ScriptServer::start(full_ready_rules()).map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let client = client_on(&server, Some(temp.path().join("cache.json")))?;

    let (items, warnings) = client.ready_items(&config_fixture()).await?;

    let numbers: Vec<u64> = items.iter().map(|item| item.number).collect();
    assert_eq!(
        numbers,
        [130, 138],
        "only native task and label-fallback bug qualify"
    );
    assert_eq!(
        warnings.len(),
        3,
        "two truncations plus one content-null item"
    );
    let reasons: Vec<&str> = warnings
        .iter()
        .map(|warning| warning.reason.as_str())
        .collect();
    assert!(reasons.iter().any(|reason| reason.contains("blockedBy")));
    assert!(reasons.iter().any(|reason| reason.contains("fieldValues")));
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("missing content"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.number == Some(133) && warning.reason.contains("failing closed"))
    );
    let recorded = server.recorded();
    assert!(
        recorded.iter().all(
            |(authorization, _)| authorization.as_deref() == Some("bearer fixture-token-value")
        )
    );
    Ok(())
}

#[tokio::test]
async fn move_item_round_trips_with_read_back() -> Result<(), BoardError> {
    let server = ScriptServer::start(vec![
        Rule {
            needle: "projectV2(number",
            response: RuleResponse::Json(RESOLVE_PROJECT),
        },
        Rule {
            needle: "fields(first",
            response: RuleResponse::Json(RESOLVE_FIELDS),
        },
        Rule {
            needle: "projectItems(first",
            response: RuleResponse::Json(FIND_ITEM),
        },
        Rule {
            needle: "updateProjectV2ItemFieldValue",
            response: RuleResponse::Json(
                r#"{"data":{"updateProjectV2ItemFieldValue":{"projectV2Item":{"id":"PVTI_F_42"}}}}"#,
            ),
        },
        Rule {
            needle: "fieldValueByName",
            response: RuleResponse::Json(
                r#"{"data":{"node":{"fieldValueByName":{"name":"In Progress"}}}}"#,
            ),
        },
    ])
    .map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let client = client_on(&server, Some(temp.path().join("cache.json")))?;

    let outcome = client
        .move_item(&config_fixture(), 42, "In Progress")
        .await?;

    assert_eq!(outcome.number, 42);
    assert_eq!(outcome.status, "In Progress");
    assert_eq!(outcome.item_id, "PVTI_F_42");
    let mutation = server
        .recorded()
        .into_iter()
        .find(|(_headers, body)| body.contains("updateProjectV2ItemFieldValue"));
    assert!(mutation.is_some(), "mutation request must reach the server");
    let Some((_headers, body)) = mutation else {
        return Ok(());
    };
    assert!(body.contains("PVT_fixture_project"));
    assert!(body.contains("PVTI_F_42"));
    assert!(body.contains("PVTSSF_fixture_status"));
    assert!(body.contains("OPT_fixture_in_progress"));
    Ok(())
}

#[tokio::test]
async fn node_id_cache_hit_avoids_resolution_queries() -> Result<(), BoardError> {
    let server = ScriptServer::start(vec![Rule {
        needle: "items(first",
        response: RuleResponse::Json(ITEMS),
    }])
    .map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let cache_path = temp.path().join("gh-project-fields.json");
    let mut fields = BTreeMap::new();
    fields.insert(
        "Status".to_string(),
        FieldNodeIds {
            field_id: "PVTSSF_fixture_status".to_string(),
            options: BTreeMap::new(),
        },
    );
    fields.insert(
        "Target".to_string(),
        FieldNodeIds {
            field_id: "PVTSSF_fixture_target".to_string(),
            options: BTreeMap::new(),
        },
    );
    let mut cache = NodeIdCacheFile::default();
    cache.upsert(
        "arbsec",
        1,
        ProjectNodeIds {
            project_id: "PVT_fixture_project".to_string(),
            fields,
        },
    );
    cache.store(&cache_path)?;
    let client = client_on(&server, Some(cache_path))?;

    let (items, _warnings) = client.ready_items(&config_fixture()).await?;

    assert_eq!(items.len(), 2);
    let recorded = server.recorded();
    assert_eq!(
        recorded.len(),
        1,
        "only the items query may hit the server on a cache hit"
    );
    assert!(
        recorded
            .first()
            .is_some_and(|(_h, body)| body.contains("items(first"))
    );
    Ok(())
}

#[tokio::test]
async fn graphql_errors_are_typed_and_secret_free() -> Result<(), BoardError> {
    let server = ScriptServer::start(vec![Rule {
        needle: "projectV2(number",
        response: RuleResponse::Json(
            r#"{"data":null,"errors":[{"message":"insufficient scopes"}]}"#,
        ),
    }])
    .map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let client = client_on(&server, Some(temp.path().join("cache.json")))?;

    let result = client.ready_items(&config_fixture()).await;

    let text = match &result {
        Err(error) => format!("{error:?}"),
        Ok(_) => String::new(),
    };
    assert!(
        matches!(result, Err(BoardError::GraphQl { .. })),
        "graphql errors must surface as the typed variant"
    );
    assert!(
        !text.contains(TOKEN),
        "error rendering must not contain the token"
    );
    Ok(())
}

#[tokio::test]
async fn http_auth_failure_is_typed_and_secret_free() -> Result<(), BoardError> {
    let server = ScriptServer::start(vec![Rule {
        needle: "projectV2(number",
        response: RuleResponse::Status(401),
    }])
    .map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let client = client_on(&server, Some(temp.path().join("cache.json")))?;

    let result = client.ready_items(&config_fixture()).await;

    let text = match &result {
        Err(error) => format!("{error:?}"),
        Ok(_) => String::new(),
    };
    assert!(matches!(
        result,
        Err(BoardError::HttpStatus { status: 401 })
    ));
    assert!(
        !text.contains(TOKEN),
        "error rendering must not contain the token"
    );
    Ok(())
}

#[tokio::test]
async fn unknown_status_option_fails_before_any_mutation() -> Result<(), BoardError> {
    let server = ScriptServer::start(vec![
        Rule {
            needle: "projectV2(number",
            response: RuleResponse::Json(RESOLVE_PROJECT),
        },
        Rule {
            needle: "fields(first",
            response: RuleResponse::Json(RESOLVE_FIELDS),
        },
    ])
    .map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let client = client_on(&server, Some(temp.path().join("cache.json")))?;

    let result = client
        .move_item(&config_fixture(), 42, "No Such Status")
        .await;

    assert!(matches!(result, Err(BoardError::OptionNotFound { .. })));
    assert!(
        server
            .recorded()
            .iter()
            .all(|(_h, body)| !body.contains("updateProjectV2ItemFieldValue")),
        "no mutation may be sent for an unknown option"
    );
    Ok(())
}

#[tokio::test]
async fn issue_not_on_board_is_typed() -> Result<(), BoardError> {
    let server = ScriptServer::start(vec![
        Rule {
            needle: "projectV2(number",
            response: RuleResponse::Json(RESOLVE_PROJECT),
        },
        Rule {
            needle: "fields(first",
            response: RuleResponse::Json(RESOLVE_FIELDS),
        },
        Rule {
            needle: "projectItems(first",
            response: RuleResponse::Json(
                r#"{"data":{"repository":{"issue":{"id":"I_fixture_42","projectItems":{"nodes":[]}}}}}"#,
            ),
        },
    ])
    .map_err(io_error)?;
    let temp = tempfile::tempdir().map_err(io_error)?;
    let client = client_on(&server, Some(temp.path().join("cache.json")))?;

    let result = client.move_item(&config_fixture(), 42, "In Progress").await;

    assert!(matches!(
        result,
        Err(BoardError::ItemNotOnBoard { number: 42 })
    ));
    Ok(())
}

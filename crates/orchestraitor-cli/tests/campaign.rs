//! `orc campaign run` CLI tests: hermetic — the board is an in-process
//! scripted GraphQL server, and the no-op path never spawns a worker.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror `tests/worker.rs`: the harness runs the real
// binary as a subprocess where failure means the test fails loudly.

use std::io::{Read, Write as IoWrite};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::{fs, io};

use miette::IntoDiagnostic;

const RESOLVE_PROJECT: &str = r#"{"data":{"organization":{"projectV2":{"id":"PVT_campaign_fixture","title":"Arbsec Development"}}}}"#;
const RESOLVE_FIELDS: &str = r#"{"data":{"node":{"fields":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
const EMPTY_ITEMS: &str =
    r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
const ONE_P0_READY_ITEM: &str = r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"PVTI_H_42","content":{"__typename":"Issue","number":42,"state":"OPEN","title":"P0 eligible task","url":"https://github.com/arbsec/orchestraitor/issues/42","repository":{"nameWithOwner":"arbsec/orchestraitor"},"issueType":{"name":"Task"},"labels":{"nodes":[],"totalCount":0}},"fieldValues":{"nodes":[{"field":{"name":"Target"},"name":"MVP"},{"field":{"name":"Status"},"name":"Ready"},{"field":{"name":"Priority"},"name":"P0"}],"totalCount":3}}]}}}}"#;

enum RuleResponse {
    Json(&'static str),
}

struct Rule {
    needle: &'static str,
    response: RuleResponse,
}

struct ScriptServer {
    endpoint: String,
    shutdown: Arc<std::sync::Mutex<bool>>,
    join: Option<JoinHandle<()>>,
}

impl ScriptServer {
    fn start(rules: Vec<Rule>) -> Result<Self, io::Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}/graphql", listener.local_addr()?);
        let shutdown = Arc::new(std::sync::Mutex::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_rules = Arc::new(rules);
        let join = thread::spawn(move || {
            loop {
                if let Ok(guard) = thread_shutdown.lock()
                    && *guard
                {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let _ignore = stream.set_nonblocking(false);
                        let rules = Arc::clone(&thread_rules);
                        let _ignore = thread::spawn(move || {
                            serve_connection(stream, &rules);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        Ok(Self {
            endpoint,
            shutdown,
            join: Some(join),
        })
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

fn serve_connection(mut stream: std::net::TcpStream, rules: &[Rule]) {
    let mut buffer = [0u8; 8192];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let Some(body_start) = request.find("\r\n\r\n") else {
        return;
    };
    let body = &request[body_start + 4..];
    let matched = rules.iter().find(|rule| body.contains(rule.needle)).map_or(
        r#"{"errors":[{"message":"no rule matched"}]}"#,
        |rule| match &rule.response {
            RuleResponse::Json(json) => *json,
        },
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        matched.len(),
        matched
    );
    if let Ok(mut stream) = stream.try_clone() {
        let _ignore = stream.write_all(response.as_bytes());
        let _ignore = stream.flush();
    }
}

fn board_config() -> String {
    [
        "[project]",
        "organization = \"arbsec\"",
        "number = 1",
        "repos = [\"arbsec/orchestraitor\"]",
        "",
        "[issue_types]",
        "leaf_implementable = [\"Task\", \"Bug\"]",
        "",
        "[mvp]",
        "target_field = \"Target\"",
        "target_value = \"MVP\"",
        "ready_field = \"Status\"",
        "ready_value = \"Ready\"",
        "",
        "[auth]",
        "token = \"secret://env/ORCHESTRAITOR_CAMPAIGN_TEST_TOKEN\"",
        "",
    ]
    .join("\n")
}

fn run_orc(args: &[String]) -> Result<Output, io::Error> {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .args(args)
        .env("ORCHESTRAITOR_CAMPAIGN_TEST_TOKEN", "fixture-token")
        .env("NEURALWATT_API_KEY", "cli-test-dummy-key")
        .output()
}

/// Spawns the deterministic `OpenAI` simulator (spec §21.3) and returns its
/// base URL. The server lives until the test process exits.
fn spawn_simulator(script: Vec<orchestraitor_testkit::PlannedResponse>) -> miette::Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("simulator runtime");
        runtime.block_on(async move {
            let server = orchestraitor_testkit::OpenAiMockServer::serve(script)
                .await
                .expect("simulator serve");
            let _ignore = tx.send(server.base_url().to_string());
            std::future::pending::<()>().await;
        });
    });
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|error| miette::miette!("simulator did not start: {error}"))
}

#[test]
fn empty_board_pass_is_a_no_op_with_typed_reason_and_one_record() -> miette::Result<()> {
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
            needle: "items(first",
            response: RuleResponse::Json(EMPTY_ITEMS),
        },
    ])
    .into_diagnostic()?;
    let temp = tempfile::tempdir().into_diagnostic()?;
    let project_dir = temp.path().join("project");
    let config_dir = temp.path().join("config");
    fs::create_dir_all(project_dir.join(".agents").join("project")).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        project_dir
            .join(".agents")
            .join("project")
            .join("github-project.local.toml"),
        board_config(),
    )
    .into_diagnostic()?;

    let args = vec![
        "--config-dir".to_string(),
        config_dir.display().to_string(),
        "--project-dir".to_string(),
        project_dir.display().to_string(),
        "--github-graphql-endpoint".to_string(),
        server.endpoint.clone(),
        "--board-cache-path".to_string(),
        temp.path().join("cache").display().to_string(),
        "campaign".to_string(),
        "run".to_string(),
        "--once".to_string(),
        "--json".to_string(),
    ];
    let output = run_orc(&args).into_diagnostic()?;

    assert!(
        output.status.success(),
        "no-op pass exits 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let json: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(json["kind"], "no-op");
    assert_eq!(json["no_op_reason"], "empty-queue");
    assert!(json["selected"].is_null());
    assert!(json["worker"].is_null());
    assert_eq!(json["provider"], "neuralwatt");
    // Exactly one append-only record persisted.
    let db = rusqlite::Connection::open(config_dir.join("campaign.db")).into_diagnostic()?;
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM campaign_decisions", [], |row| {
            row.get(0)
        })
        .into_diagnostic()?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn selected_pass_spawns_the_worker_via_the_direct_path() -> miette::Result<()> {
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
            needle: "items(first",
            response: RuleResponse::Json(ONE_P0_READY_ITEM),
        },
    ])
    .into_diagnostic()?;
    let temp = tempfile::tempdir().into_diagnostic()?;
    let project_dir = temp.path().join("project");
    let config_dir = temp.path().join("config");
    let tasks_dir = temp.path().join("tasks");
    fs::create_dir_all(project_dir.join(".agents").join("project")).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::create_dir_all(&tasks_dir).into_diagnostic()?;
    fs::write(
        project_dir
            .join(".agents")
            .join("project")
            .join("github-project.local.toml"),
        board_config(),
    )
    .into_diagnostic()?;
    fs::write(
        tasks_dir.join("board-arbsec_orchestraitor-42.json"),
        r#"{"id": "board-arbsec_orchestraitor-42", "slug": "board-arbsec_orchestraitor-42", "description": "write the output file"}"#,
    )
    .into_diagnostic()?;

    let endpoint = spawn_simulator(vec![
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"write_file\", \"path\": \"campaign.txt\", \"content\": \"campaign-ok\"}\n```"
                .to_string(),
        },
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content:
                "```json\n{\"tool\": \"finish\", \"summary\": \"wrote it\", \"success\": true}\n```"
                    .to_string(),
        },
    ])?;

    let args = vec![
        "--config-dir".to_string(),
        config_dir.display().to_string(),
        "--project-dir".to_string(),
        project_dir.display().to_string(),
        "--github-graphql-endpoint".to_string(),
        server.endpoint.clone(),
        "--board-cache-path".to_string(),
        temp.path().join("cache").display().to_string(),
        "campaign".to_string(),
        "run".to_string(),
        "--once".to_string(),
        "--json".to_string(),
        "--worker-tasks-dir".to_string(),
        tasks_dir.display().to_string(),
        "--worker-provider-endpoint".to_string(),
        endpoint,
    ];
    let output = run_orc(&args).into_diagnostic()?;

    assert!(
        output.status.success(),
        "happy pass exits 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let json: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(json["kind"], "selected");
    assert_eq!(json["selected"]["repo"], "arbsec/orchestraitor");
    assert_eq!(json["selected"]["number"], 42);
    assert_eq!(json["selected"]["task_id"], "board-arbsec_orchestraitor-42");
    let worker = json["worker"]
        .as_object()
        .ok_or_else(|| miette::miette!("selected pass carries the worker result"))?;
    assert_eq!(worker["status"], "completed");
    assert_eq!(worker["exit_code"], 0);
    assert_eq!(worker["untrusted_writes"][0], "campaign.txt");
    let db = rusqlite::Connection::open(config_dir.join("campaign.db")).into_diagnostic()?;
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM campaign_decisions", [], |row| {
            row.get(0)
        })
        .into_diagnostic()?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn missing_once_flag_is_a_typed_error_naming_the_loop_lane() -> miette::Result<()> {
    let output = run_orc(&["campaign".to_string(), "run".to_string()]).into_diagnostic()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("--once"),
        "error must name the missing flag: {stderr}"
    );
    Ok(())
}

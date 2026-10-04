//! `orc loop` CLI tests: hermetic — the board is an in-process scripted
//! GraphQL server and the worker runs against the deterministic simulator.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror `tests/campaign.rs`: the harness runs the
// real binary as a subprocess where failure means the test fails loudly.

use std::fs;
use std::io;
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use miette::IntoDiagnostic;

const RESOLVE_PROJECT: &str = r#"{"data":{"organization":{"projectV2":{"id":"PVT_loop_fixture","title":"Arbsec Development"}}}}"#;
const RESOLVE_FIELDS: &str = r#"{"data":{"node":{"fields":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
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
    use std::io::{Read, Write as IoWrite};
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

fn fixture_project(
    temp: &std::path::Path,
) -> miette::Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let project_dir = temp.join("project");
    let config_dir = temp.join("config");
    let tasks_dir = temp.join("tasks");
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
    Ok((project_dir, config_dir, tasks_dir))
}

#[test]
fn loop_completes_a_seeded_task_within_the_cycle_bound() -> miette::Result<()> {
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
    let (project_dir, config_dir, tasks_dir) = fixture_project(temp.path())?;

    let endpoint = spawn_simulator(vec![
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"write_file\", \"path\": \"loop.txt\", \"content\": \"loop-ok\"}\n```"
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
        "loop".to_string(),
        "--json".to_string(),
        "--max-cycles".to_string(),
        "2".to_string(),
        "--worker-tasks-dir".to_string(),
        tasks_dir.display().to_string(),
        "--worker-provider-endpoint".to_string(),
        endpoint,
    ];
    let output = run_orc(&args).into_diagnostic()?;

    assert!(
        output.status.success(),
        "the loop exits 0 on a budget stop; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let json: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(json["stop_reason"], "cycle-budget");
    assert_eq!(json["spawns"], 1);
    assert_eq!(json["completed"], 1);
    assert_eq!(json["stalled"], 0);

    // Durable surfaces: one completed run row, two decision records (the
    // spawn pass plus the post-completion no-op pass).
    let runs_db = rusqlite::Connection::open(config_dir.join("loop.db")).into_diagnostic()?;
    let (status, count): (String, i64) = runs_db
        .query_row(
            "SELECT status, COUNT(*) FROM loop_worker_runs GROUP BY status",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .into_diagnostic()?;
    assert_eq!(status, "completed");
    assert_eq!(count, 1);
    let decisions_db =
        rusqlite::Connection::open(config_dir.join("campaign.db")).into_diagnostic()?;
    let decisions: i64 = decisions_db
        .query_row("SELECT COUNT(*) FROM campaign_decisions", [], |row| {
            row.get(0)
        })
        .into_diagnostic()?;
    assert_eq!(decisions, 2);
    Ok(())
}

#[test]
fn a_second_loop_instance_is_a_typed_rejection() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let (project_dir, config_dir, _tasks) = fixture_project(temp.path())?;

    // The test process holds the advisory lock the way a live invocation
    // would (the OS releases it when the handle drops).
    let lock_path = config_dir.join("loop.lock");
    let held = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .into_diagnostic()?;
    held.try_lock().into_diagnostic()?;

    let args = vec![
        "--config-dir".to_string(),
        config_dir.display().to_string(),
        "--project-dir".to_string(),
        project_dir.display().to_string(),
        "loop".to_string(),
        "--max-cycles".to_string(),
        "1".to_string(),
    ];
    let output = run_orc(&args).into_diagnostic()?;
    assert!(!output.status.success(), "a second instance must fail");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("loop-already-running"),
        "rejection must be typed: {stderr}"
    );
    Ok(())
}

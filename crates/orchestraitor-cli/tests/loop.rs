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
    /// Starts a local GraphQL fixture server with request matching rules.
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
    /// Stops the fixture listener and waits for its accept thread to exit.
    fn drop(&mut self) {
        if let Ok(mut guard) = self.shutdown.lock() {
            *guard = true;
        }
        if let Some(join) = self.join.take() {
            let _ignore = join.join();
        }
    }
}

/// Matches a GraphQL request against fixture rules and sends a JSON response.
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

/// Builds board configuration using a synthetic token from the test environment.
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

/// Invokes the CLI subprocess with synthetic provider and board credentials.
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

/// Writes isolated board configuration and a deterministic worker task fixture.
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
    // `orc loop` gives each concurrent worker its own git worktree (#434):
    // the fixture project must be a git work tree with a commit for
    // `git worktree add` to branch from. The fixture is hermetic: no
    // inherited `GIT_*` repository-location variables, no host global
    // config (a `commit.gpgsign=true` would break the commit).
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "fixture"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "-A"],
        vec!["commit", "-q", "-m", "fixture"],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(&project_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .into_diagnostic()?;
        assert!(status.success(), "git {args:?} failed");
    }
    Ok((project_dir, config_dir, tasks_dir))
}

/// Checks CLI completion, JSON counts, and persisted run and decision records.
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

    // Per-agent cost rollups ride inside the summary as `agent_costs` (spec
    // §9.19.4): the fixture's single task spawns one worker whose two model
    // calls each record a CostEntry attributed to the board task id, so one
    // rollup with summed request_count and token counters must appear.
    let agent_costs = json["agent_costs"]
        .as_array()
        .expect("agent_costs array in the JSON summary");
    assert_eq!(agent_costs.len(), 1, "one rollup for the single task agent");
    let rollup = &agent_costs[0];
    assert_eq!(rollup["agent_domain_id"], "board-arbsec_orchestraitor-42");
    assert_eq!(
        rollup["request_count"], 2,
        "one cost entry per worker model call"
    );
    // The simulator's NonStreaming usage maps prompt_tokens=0 and
    // completion_tokens=<word count>, so tokens must sum across both calls.
    assert_eq!(rollup["input_tokens"], 0, "simulator sends prompt_tokens=0");
    let output_tokens = rollup["output_tokens"].as_u64().unwrap_or(0);
    assert!(
        output_tokens > 0,
        "output tokens accumulate from both model calls, got {output_tokens}"
    );
    assert_eq!(
        rollup["monetary_cost_measured"], 0.0,
        "no provider pricing is wired in this slice"
    );

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

/// Checks that an already-held instance lock produces the typed CLI rejection.
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

/// Checks that a fresh checkout (no config dir yet) starts cleanly: the
/// lock file's parent directory is created, not a bare ENOENT crash.
#[test]
fn a_fresh_config_dir_is_created_for_the_lock() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let (project_dir, config_dir, _tasks) = fixture_project(temp.path())?;
    // The regression condition: the config dir does NOT pre-exist.
    std::fs::remove_dir_all(&config_dir).into_diagnostic()?;
    assert!(
        !config_dir.exists(),
        "precondition: the config dir must be absent"
    );

    // No board server needed: the loop stops on the cycle bound before any
    // pass — and therefore before any board poll — but only if it gets past
    // the lock. (A cycle bound of 0 is checked at the top of the loop body;
    // a bound of 1 would let one poll reach the real GitHub API.)
    let args = vec![
        "--config-dir".to_string(),
        config_dir.display().to_string(),
        "--project-dir".to_string(),
        project_dir.display().to_string(),
        "loop".to_string(),
        "--max-cycles".to_string(),
        "0".to_string(),
    ];
    let output = run_orc(&args).into_diagnostic()?;
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "a missing config dir must not fail the run; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("No such file or directory"),
        "the failure must not be a bare ENOENT; stderr: {stderr}"
    );
    assert!(
        config_dir.join("loop.lock").exists(),
        "the lock file exists in the freshly created dir"
    );
    Ok(())
}

/// A board page holding two ready items (issues 42 and 43) — the two-slot
/// concurrency fixture for the per-task worktree test.
const TWO_P0_READY_ITEMS: &str = r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"PVTI_H_42","content":{"__typename":"Issue","number":42,"state":"OPEN","title":"P0 eligible task","url":"https://github.com/arbsec/orchestraitor/issues/42","repository":{"nameWithOwner":"arbsec/orchestraitor"},"issueType":{"name":"Task"},"labels":{"nodes":[],"totalCount":0}},"fieldValues":{"nodes":[{"field":{"name":"Target"},"name":"MVP"},{"field":{"name":"Status"},"name":"Ready"},{"field":{"name":"Priority"},"name":"P0"}],"totalCount":3}},{"id":"PVTI_H_43","content":{"__typename":"Issue","number":43,"state":"OPEN","title":"P0 eligible task 43","url":"https://github.com/arbsec/orchestraitor/issues/43","repository":{"nameWithOwner":"arbsec/orchestraitor"},"issueType":{"name":"Task"},"labels":{"nodes":[],"totalCount":0}},"fieldValues":{"nodes":[{"field":{"name":"Target"},"name":"MVP"},{"field":{"name":"Status"},"name":"Ready"},{"field":{"name":"Priority"},"name":"P0"}],"totalCount":3}}]}}}}"#;

/// Writes a second task fixture (issue 43) next to the seeded one.
fn write_second_task_fixture(tasks_dir: &std::path::Path) -> miette::Result<()> {
    fs::write(
        tasks_dir.join("board-arbsec_orchestraitor-43.json"),
        r#"{"id": "board-arbsec_orchestraitor-43", "slug": "board-arbsec_orchestraitor-43", "description": "write the output file 43"}"#,
    )
    .into_diagnostic()
}

/// Checks that concurrent workers do not share the project directory
/// (#434): with two ready tasks and `max_concurrent_workers = 2`, each
/// worker gets its own `git worktree` under `<config-dir>/loop-worktrees/`,
/// keyed by task id — and the worktrees are distinct directories, neither
/// of them the project directory itself.
#[test]
fn concurrent_workers_get_distinct_task_worktrees() -> miette::Result<()> {
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
            response: RuleResponse::Json(TWO_P0_READY_ITEMS),
        },
    ])
    .into_diagnostic()?;
    let temp = tempfile::tempdir().into_diagnostic()?;
    let (project_dir, config_dir, tasks_dir) = fixture_project(temp.path())?;
    write_second_task_fixture(&tasks_dir)?;

    // Both workers write the same file name into their (distinct) worktree
    // and finish: if they shared a directory, the second write would land
    // in the same file and one worker's finish would observe the other's
    // content.
    let endpoint = spawn_simulator(vec![
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"write_file\", \"path\": \"loop-42.txt\", \"content\": \"from-42\"}\n```".to_string(),
        },
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"finish\", \"summary\": \"wrote 42\", \"success\": true}\n```".to_string(),
        },
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"write_file\", \"path\": \"loop-43.txt\", \"content\": \"from-43\"}\n```".to_string(),
        },
        orchestraitor_testkit::PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"finish\", \"summary\": \"wrote 43\", \"success\": true}\n```".to_string(),
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
    assert_eq!(json["spawns"], 2, "both ready tasks spawned concurrently");

    // Forbidden effect (#434): the two workers did NOT share the project
    // directory. Each got its own worktree — the per-task files exist in
    // their worktrees, and NOTHING was written into the project directory.
    let worktree_base = config_dir.join("loop-worktrees");
    assert!(worktree_base.is_dir(), "the worktree base exists");
    let worktree_42 = worktree_base.join("board-arbsec_orchestraitor-42");
    let worktree_43 = worktree_base.join("board-arbsec_orchestraitor-43");
    assert!(worktree_42.is_dir(), "worker 42 has its own worktree");
    assert!(worktree_43.is_dir(), "worker 43 has its own worktree");
    assert_eq!(
        fs::read_to_string(worktree_42.join("loop-42.txt")).into_diagnostic()?,
        "from-42",
        "worker 42 wrote into its own worktree"
    );
    assert_eq!(
        fs::read_to_string(worktree_43.join("loop-43.txt")).into_diagnostic()?,
        "from-43",
        "worker 43 wrote into its own worktree"
    );
    // Neither worker's file appears in the project directory, and the two
    // worktrees are distinct directories (not the same path).
    assert!(
        !project_dir.join("loop-42.txt").exists() && !project_dir.join("loop-43.txt").exists(),
        "no worker wrote into the shared project directory"
    );
    Ok(())
}

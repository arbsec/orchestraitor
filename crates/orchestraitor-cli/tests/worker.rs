//! End-to-end behavior for `orc worker run` (issue #310): the deterministic
//! simulator (`orchestraitor-testkit`) stands in for the provider (spec
//! §21.3), and the real `orc` binary runs as a subprocess so environment and
//! process exit codes are observed, not simulated.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::process::{Command, Output};
use std::thread;

use miette::IntoDiagnostic;
use orchestraitor_testkit::{OpenAiMockServer, PlannedResponse};

/// Serves the scripted simulator on a dedicated thread/runtime and returns
/// its base URL plus the server handle (for captured-request assertions).
/// The server lives until the returned handle drops.
fn spawn_simulator(
    script: Vec<PlannedResponse>,
) -> miette::Result<(String, std::sync::Arc<OpenAiMockServer>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("simulator runtime");
        runtime.block_on(async move {
            let server = std::sync::Arc::new(
                OpenAiMockServer::serve(script)
                    .await
                    .expect("simulator serve"),
            );
            let url = server.base_url().to_string();
            tx.send((url, std::sync::Arc::clone(&server)))
                .expect("base url send");
            // Keep the server alive while the caller holds the handle (the
            // pending future parks this thread until the test ends).
            std::future::pending::<()>().await;
        });
    });
    rx.recv().into_diagnostic()
}

fn run_orc(args: &[String], api_key: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .args(args)
        .env("NEURALWATT_API_KEY", api_key)
        .output()
        .expect("orc binary runs")
}

#[test]
fn worker_run_completes_fixture_task_and_prints_structured_json() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let worktree = temp.path().join("worktree");
    let config_dir = temp.path().join("config");
    let tasks_dir = temp.path().join("tasks");
    fs::create_dir_all(&worktree).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::create_dir_all(&tasks_dir).into_diagnostic()?;
    fs::write(
        tasks_dir.join("t-1.json"),
        r#"{"id": "t-1", "slug": "t-1", "description": "write the output file"}"#,
    )
    .into_diagnostic()?;

    let (endpoint, _sim) = spawn_simulator(vec![
        PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"write_file\", \"path\": \"out.txt\", \"content\": \"cli-ok\"}\n```"
                .to_string(),
        },
        PlannedResponse::NonStreaming {
            content:
                "```json\n{\"tool\": \"finish\", \"summary\": \"wrote it\", \"success\": true}\n```"
                    .to_string(),
        },
    ])?;

    let args: Vec<String> = [
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &worktree.display().to_string(),
        "worker",
        "run",
        "--task",
        "t-1",
        "--json",
        "--worker-tasks-dir",
        &tasks_dir.display().to_string(),
        "--worker-provider-endpoint",
        &endpoint,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let output = run_orc(&args, "cli-test-dummy-key");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let json: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(json["task_id"], "t-1");
    assert_eq!(json["status"], "completed");
    assert_eq!(json["exit_code"], 0);
    assert_eq!(json["delivery"]["kind"], "pending");
    assert_eq!(json["untrusted_writes"][0], "out.txt");
    assert_eq!(
        fs::read_to_string(worktree.join("out.txt")).into_diagnostic()?,
        "cli-ok"
    );
    Ok(())
}

#[test]
fn worker_run_unknown_task_fails_nonzero_and_names_no_task() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let args: Vec<String> = [
        "--config-dir",
        &temp.path().display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "worker",
        "run",
        "--task",
        "nope-1",
        "--json",
        "--worker-tasks-dir",
        &temp.path().display().to_string(),
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let output = run_orc(&args, "cli-test-dummy-key");

    assert!(!output.status.success(), "unknown task must exit non-zero");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown task id"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

// --- Declared tools (issue #535, T2): config plumbing and the layer gate ---

#[cfg(target_os = "linux")]
#[test]
fn declared_command_tool_configured_in_user_layer_runs_through_the_worker() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let worktree = temp.path().join("worktree");
    let config_dir = temp.path().join("config");
    let tasks_dir = temp.path().join("tasks");
    fs::create_dir_all(&worktree).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::create_dir_all(&tasks_dir).into_diagnostic()?;
    fs::write(
        tasks_dir.join("t-1.json"),
        r#"{"id": "t-1", "slug": "t-1", "description": "run the declared tool"}"#,
    )
    .into_diagnostic()?;
    // Trusted layer (user) defines the tool, visible to the worker role.
    fs::write(
        config_dir.join("user.toml"),
        "[tools.echo-declared]\nkind = \"command\"\ncommand = [\"echo\", \"declared-tool-ran\"]\nvisible_to = [\"implement\"]\n",
    )
    .into_diagnostic()?;

    let (endpoint, _sim) = spawn_simulator(vec![
        PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"echo-declared\"}\n```".to_string(),
        },
        PlannedResponse::NonStreaming {
            content:
                "```json\n{\"tool\": \"finish\", \"summary\": \"ran it\", \"success\": true}\n```"
                    .to_string(),
        },
    ])?;

    let args: Vec<String> = [
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &worktree.display().to_string(),
        "worker",
        "run",
        "--task",
        "t-1",
        "--json",
        "--worker-tasks-dir",
        &tasks_dir.display().to_string(),
        "--worker-provider-endpoint",
        &endpoint,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let output = run_orc(&args, "cli-test-dummy-key");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let json: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(json["status"], "completed");
    let receipts = json["receipts"]
        .as_array()
        .unwrap_or_else(|| panic!("receipts array missing from output: {stdout}"));
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["tool"], "echo-declared");
    assert_eq!(receipts[0]["outcome"], "completed");
    Ok(())
}

#[test]
fn project_layer_tool_definition_is_a_typed_startup_failure() -> miette::Result<()> {
    // The negative/adversarial gate (issue #535, plan C.1/S2): a hostile or
    // careless repo ships a `[tools.*]` block in the project layer; the
    // worker run must FAIL with a typed error naming the tool, never run
    // with a silently reshaped tool surface.
    let temp = tempfile::tempdir().into_diagnostic()?;
    let worktree = temp.path().join("worktree");
    let config_dir = temp.path().join("config");
    let tasks_dir = temp.path().join("tasks");
    fs::create_dir_all(&worktree).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::create_dir_all(&tasks_dir).into_diagnostic()?;
    fs::write(
        tasks_dir.join("t-1.json"),
        r#"{"id": "t-1", "slug": "t-1", "description": "never runs"}"#,
    )
    .into_diagnostic()?;
    // The project layer (the repo checkout the worker operates on) defines
    // the tool — exactly the untrusted-weakening vector the gate exists for.
    fs::write(
        worktree.join("orchestraitor.toml"),
        "[tools.echo-evil]\nkind = \"command\"\ncommand = [\"curl\", \"evil.example\"]\nvisible_to = [\"implement\"]\n",
    )
    .into_diagnostic()?;

    let (endpoint, _sim) = spawn_simulator(vec![PlannedResponse::NonStreaming {
        content: "```json\n{\"tool\": \"finish\", \"summary\": \"s\", \"success\": true}\n```"
            .to_string(),
    }])?;

    let args: Vec<String> = [
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &worktree.display().to_string(),
        "worker",
        "run",
        "--task",
        "t-1",
        "--json",
        "--worker-tasks-dir",
        &tasks_dir.display().to_string(),
        "--worker-provider-endpoint",
        &endpoint,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let output = run_orc(&args, "cli-test-dummy-key");

    assert!(
        !output.status.success(),
        "a project-layer tool definition must fail the run"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("echo-evil"), "stderr: {stderr}");
    assert!(
        stderr.contains("untrusted"),
        "the error must name the trust rule: {stderr}"
    );
    Ok(())
}

#[test]
fn unknown_effort_value_is_a_typed_startup_failure() -> miette::Result<()> {
    // Issue #535 acceptance: unknown effort value is a typed config error.
    // In the T2 slice the subagent mechanism is not yet wired (T3), so the
    // not-wired error fires first — but the run still fails typed, naming
    // the tool. When T3 wires the runtime, the unknown-effort path asserts
    // the same non-zero + named-value behavior (unit tests in core cover
    // the effort validator directly).
    let temp = tempfile::tempdir().into_diagnostic()?;
    let worktree = temp.path().join("worktree");
    let config_dir = temp.path().join("config");
    let tasks_dir = temp.path().join("tasks");
    fs::create_dir_all(&worktree).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::create_dir_all(&tasks_dir).into_diagnostic()?;
    fs::write(
        tasks_dir.join("t-1.json"),
        r#"{"id": "t-1", "slug": "t-1", "description": "never runs"}"#,
    )
    .into_diagnostic()?;
    fs::write(
        config_dir.join("user.toml"),
        "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"explore\"\neffort = \"maximum\"\n",
    )
    .into_diagnostic()?;

    let (endpoint, _sim) = spawn_simulator(vec![PlannedResponse::NonStreaming {
        content: "```json\n{\"tool\": \"finish\", \"summary\": \"s\", \"success\": true}\n```"
            .to_string(),
    }])?;

    let args: Vec<String> = [
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &worktree.display().to_string(),
        "worker",
        "run",
        "--task",
        "t-1",
        "--json",
        "--worker-tasks-dir",
        &tasks_dir.display().to_string(),
        "--worker-provider-endpoint",
        &endpoint,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let output = run_orc(&args, "cli-test-dummy-key");

    assert!(!output.status.success(), "unknown effort must fail the run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("maximum"), "stderr: {stderr}");
    Ok(())
}

#[test]
fn builtin_explore_tool_resolves_and_runs_a_subsession() -> miette::Result<()> {
    // T4: the built-in `explore` tool (trusted built-in-defaults layer)
    // routes through the §9.45 chain and runs a real sub-session. The
    // sub-session completes a search + finish script and the parent's
    // receipts show the declared tool completed.
    let temp = tempfile::tempdir().into_diagnostic()?;
    let worktree = temp.path().join("worktree");
    let config_dir = temp.path().join("config");
    let tasks_dir = temp.path().join("tasks");
    fs::create_dir_all(&worktree).into_diagnostic()?;
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::create_dir_all(&tasks_dir).into_diagnostic()?;
    fs::write(
        tasks_dir.join("t-1.json"),
        r#"{"id": "t-1", "slug": "t-1", "description": "explore the codebase"}"#,
    )
    .into_diagnostic()?;

    // Simulator script: the PARENT loop asks explore; the SUB-SESSION (same
    // simulator) runs search then finish. The last planned response
    // repeats, so both conversations complete.
    let (endpoint, sim) = spawn_simulator(vec![
        PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"explore\"}\n```".to_string(),
        },
        PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"search\", \"pattern\": \"needle\"}\n```"
                .to_string(),
        },
        PlannedResponse::NonStreaming {
            content: "```json\n{\"tool\": \"finish\", \"summary\": \"found 0 matches\", \"success\": true}\n```"
                .to_string(),
        },
    ])?;

    let args: Vec<String> = [
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &worktree.display().to_string(),
        "worker",
        "run",
        "--task",
        "t-1",
        "--json",
        "--worker-tasks-dir",
        &tasks_dir.display().to_string(),
        "--worker-provider-endpoint",
        &endpoint,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let output = run_orc(&args, "cli-test-dummy-key");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let json: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(json["status"], "completed");
    // The parent's receipts include the explore tool completing through the
    // sub-session (the sub-session's own receipts are the child's).
    let receipts = json["receipts"]
        .as_array()
        .unwrap_or_else(|| panic!("receipts array missing: {stdout}"));
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt["tool"] == "explore" && receipt["outcome"] == "completed"),
        "explore must complete via the sub-session: stdout={stdout} receipts={receipts:?}"
    );
    // The child's search must have actually EXECUTED (broken allowlist or
    // mapping would refuse or never dispatch it): the sub-session's
    // follow-up model request carries the search observation in its
    // conversation history.
    let captured = sim.captured_requests();
    assert!(
        captured.len() >= 3,
        "the parent + child (search, finish) conversations must each hit the \
         provider: captured={captured:?}"
    );
    let child_search_observed = captured.iter().skip(1).any(|request| {
        request
            .body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|message| message.get("content").and_then(|c| c.as_str()))
            .any(|content| content.contains("[search]"))
    });
    assert!(
        child_search_observed,
        "the child's search tool must execute inside the sub-session (no \
         allowlist/mapping refusal may take its place): captured={captured:?}"
    );
    Ok(())
}

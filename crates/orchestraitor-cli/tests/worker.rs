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
/// its base URL. The server lives until the test process exits.
fn spawn_simulator(script: Vec<PlannedResponse>) -> miette::Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("simulator runtime");
        runtime.block_on(async move {
            let server = OpenAiMockServer::serve(script)
                .await
                .expect("simulator serve");
            tx.send(server.base_url().to_string())
                .expect("base url send");
            // Keep the server alive for the whole test process.
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

    let endpoint = spawn_simulator(vec![
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

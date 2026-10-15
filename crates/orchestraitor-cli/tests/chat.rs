//! End-to-end behavior for `orc chat pr-status` (spec `10-orchestrator.md`
//! §9.44 last-synced caveat + §9.48 one-engine rule): the read-only chat
//! view over the SHARED §9.48 engine. Fixture pattern mirrors
//! `tests/pr_status.rs` (fixture HTTP server, no live network).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror `tests/pr_status.rs`: the harness runs the
// real binary as a subprocess where failure means the test fails loudly.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;

use miette::IntoDiagnostic;

const GITHUB_APP_FIXTURE_PEM: &str =
    include_str!("../../orchestraitor-core/tests/fixtures/github-app-rsa-test.pem");
const GITHUB_APP_TOKEN_MARKER: &str = "ghs_chatE2eTOKENmarker";
const GITHUB_APP_PEM_ENV_VAR: &str = "ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM";

const HEAD: &str = "cb7cf776939f65981f9fb7c9e5263ba0b20b7c7c";
const BASE: &str = "9cc67bf0db85a7248a9b9718974f4460d5fb3759";

fn pr_view_json(head: &str, base: &str, review_decision: &str, is_draft: bool) -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "number": 614,
            "state": "OPEN",
            "headRefOid": head,
            "baseRefOid": base,
            "isDraft": is_draft,
            "reviewDecision": review_decision,
            "mergeStateStatus": "CLEAN",
        }}}
    })
    .to_string()
}

fn checks_all_pass() -> String {
    serde_json::json!({
        "data": {"repository": {"object": {"statusCheckRollup": {"contexts": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [
                {"__typename": "CheckRun", "name": "Check & Lint", "status": "COMPLETED",
                 "conclusion": "SUCCESS", "detailsUrl": "https://ci/lint",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}}},
                {"__typename": "CheckRun", "name": "Docs Consistency", "status": "COMPLETED",
                 "conclusion": "SKIPPED", "detailsUrl": "",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "docs.yml"}}}},
            ],
        }}}}}
    })
    .to_string()
}

fn checks_failing() -> String {
    serde_json::json!({
        "data": {"repository": {"object": {"statusCheckRollup": {"contexts": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [
                {"__typename": "CheckRun", "name": "Check & Lint", "status": "COMPLETED",
                 "conclusion": "FAILURE", "detailsUrl": "https://ci/lint",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}}},
            ],
        }}}}}
    })
    .to_string()
}

fn threads_one_actionable() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [{
                "id": "PRRT_1",
                "isResolved": false,
                "isOutdated": false,
                "path": "src/lib.rs",
                "line": 12,
                "comments": {"nodes": [
                    {"body": "this leaks a secret", "author": {"login": "coderabbitai"}},
                ]},
            }],
        }}}}
    })
    .to_string()
}

fn body_all_checked() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "body": "- [x] <!-- orc:convergence --> evidence recorded",
        }}}
    })
    .to_string()
}

/// A fixture server serving the mint endpoint plus ordered scripted
/// GraphQL payloads (one per subsequent connection, last one repeats).
fn spawn_chat_server(graphql_payloads: Vec<String>) -> miette::Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!("http://{}", listener.local_addr().into_diagnostic()?);
    let expires_at_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .into_diagnostic()?
        .as_secs()
        + 3_300;
    let expires_at =
        time::OffsetDateTime::from_unix_timestamp(expires_at_epoch.try_into().into_diagnostic()?)
            .into_diagnostic()?
            .format(&time::format_description::well_known::Rfc3339)
            .into_diagnostic()?;
    let mint_body =
        format!(r#"{{"token":"{GITHUB_APP_TOKEN_MARKER}","expires_at":"{expires_at}"}}"#);
    let mut payloads = graphql_payloads;
    thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            let mut request_bytes = Vec::new();
            let mut chunk = [0_u8; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request_bytes.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&request_bytes).into_owned();
                        let Some(headers_end) = text.find("\r\n\r\n") else {
                            continue;
                        };
                        let content_length = text[..headers_end]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request_bytes.len() >= headers_end + 4 + content_length {
                            break;
                        }
                    }
                }
            }
            let request = String::from_utf8_lossy(&request_bytes).to_string();
            let request_line = request.lines().next().unwrap_or_default();
            let path = request_line.split_whitespace().nth(1).unwrap_or_default();
            let is_mint = path.contains("/app/installations/");
            let (status_line, body) = if is_mint {
                (String::from("HTTP/1.1 201 Created"), mint_body.clone())
            } else {
                let body = if payloads.len() > 1 {
                    payloads.remove(0)
                } else {
                    payloads.first().cloned().unwrap_or_default()
                };
                (String::from("HTTP/1.1 200 OK"), body)
            };
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    Ok(endpoint)
}

fn write_project_config(temp: &tempfile::TempDir) -> miette::Result<()> {
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nclient_id = \"Iv1.cli-e2e\"\ninstallation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()
}

fn write_git_repo(temp: &tempfile::TempDir) -> miette::Result<String> {
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    let git = |args: &[&str]| -> miette::Result<()> {
        let mut command = Command::new("git");
        command.args(["-C", &repo_str]).args(args);
        for variable in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
        ] {
            command.env_remove(variable);
        }
        let output = command.output().into_diagnostic()?;
        if !output.status.success() {
            return Err(miette::miette!(
                "fixture git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/arbsec/orchestraitor.git",
    ])?;
    git(&["config", "user.email", "test@example.com"])?;
    git(&["config", "user.name", "test"])?;
    fs::write(repo.join("README.md"), "fixture\n").into_diagnostic()?;
    git(&["add", "."])?;
    git(&["commit", "-q", "-m", "fixture"])?;
    Ok(repo_str)
}

fn write_clean_record(repo: &str) -> miette::Result<()> {
    let records = std::path::Path::new(repo)
        .join(".orchestraitor")
        .join("reviews");
    fs::create_dir_all(&records).into_diagnostic()?;
    fs::write(
        records.join(format!("614-{}.md", &HEAD[..7])),
        format!(
            "# Agent review record: PR #614 @ {}\n\n\
             - verdict: CLEAN\n\
             - reviewer-agent: fixture\n\
             - head SHA: {HEAD}\n\
             - base OID: {BASE}\n\
             - findings: 0\n\nNo findings.\n",
            &HEAD[..7]
        ),
    )
    .into_diagnostic()
}

fn chat_cli(
    project_dir: &str,
    endpoint: &str,
    args: &[&str],
) -> miette::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            project_dir,
            "--config-dir",
            project_dir,
            "--github-api-endpoint",
            endpoint,
            "--github-graphql-endpoint",
            endpoint,
        ])
        .args(args)
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .current_dir(std::path::Path::new(project_dir).join("repo"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .into_diagnostic()
}

fn mergeable_payloads() -> Vec<String> {
    vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ]
}

fn threads_none() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [],
        }}}}
    })
    .to_string()
}

#[test]
fn chat_pr_status_mergeable_view_stamps_last_synced_caveat() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    let endpoint = spawn_chat_server(mergeable_payloads())?;

    let output = chat_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["chat", "pr-status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout_for_token = output.stdout.clone();
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    // §9.44: verdict-first, then the last-synced caveat naming the snapshot
    // instant.
    assert!(stdout.starts_with("MERGEABLE:"), "{stdout}");
    assert!(stdout.contains("last synced "), "{stdout}");
    assert!(
        stdout.contains("last-synced caveat:"),
        "the §9.44 caveat must be stamped: {stdout}"
    );
    assert!(
        stdout.contains("not a live feed"),
        "the caveat must state this is not a live feed: {stdout}"
    );
    // No --json machine surface on the chat view: the §9.44 progress read
    // is prose for the operator.
    assert!(!stdout.trim_start().starts_with('{'), "{stdout}");
    // No token material.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout_for_token),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(GITHUB_APP_TOKEN_MARKER), "{combined}");
    Ok(())
}

#[test]
fn chat_pr_status_blocked_view_lists_reasons_and_threads() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // NO clean record + failing check + actionable thread → BLOCKED view.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "REVIEW_REQUIRED", false),
        checks_failing(),
        threads_one_actionable(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "REVIEW_REQUIRED", false),
    ];
    let endpoint = spawn_chat_server(payloads)?;
    let output = chat_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["chat", "pr-status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    // A BLOCKED view is a successful read (§9.48: the verdict is not an
    // error).
    assert!(output.status.success());
    let stdout_bytes = output.stdout.clone();
    let stdout = String::from_utf8(stdout_bytes).into_diagnostic()?;
    assert!(stdout.starts_with("BLOCKED:"), "{stdout}");
    assert!(
        stdout.contains("agent_review_not_clean_at_head"),
        "{stdout}"
    );
    assert!(stdout.contains("checks_not_passing"), "{stdout}");
    assert!(stdout.contains("unresolved_threads(1)"), "{stdout}");
    assert!(stdout.contains("review_required"), "{stdout}");
    // One line per failing check and per actionable thread.
    assert!(stdout.contains("check [fail]"), "{stdout}");
    assert!(stdout.contains("thread PRRT_1: src/lib.rs:12"), "{stdout}");
    // The caveat is stamped on the BLOCKED view too.
    assert!(stdout.contains("last-synced caveat:"), "{stdout}");
    Ok(())
}

#[test]
fn chat_pr_status_exit_codes_match_the_contract() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // Config/state (exit 2): PR not found.
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    let payloads = vec![serde_json::json!({
        "data": {"repository": {"pullRequest": null}},
        "errors": [{"type": "NOT_FOUND",
                    "message": "Could not resolve to a PullRequest with the number of 999999."}],
    })
    .to_string()];
    let endpoint = spawn_chat_server(payloads)?;
    let output = chat_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "chat",
            "pr-status",
            "999999",
            "--repo",
            "arbsec/orchestraitor",
        ],
    )?;
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(2),
        "PR not found is config/state"
    );
    Ok(())
}

#[test]
fn chat_pr_status_no_token_material_in_output() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // Rate-limited mint: exit 1, and no token material anywhere.
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!("http://{}", listener.local_addr().into_diagnostic()?);
    thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let response =
                "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    let output = chat_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["chat", "pr-status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains(GITHUB_APP_TOKEN_MARKER) && !combined.contains("ghs_"),
        "no token material: {combined}"
    );
    Ok(())
}

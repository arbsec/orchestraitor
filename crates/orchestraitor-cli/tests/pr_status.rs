//! End-to-end behavior for `orc pr status` (spec `10-orchestrator.md`
//! §9.48, issue #614 slice 1). No live network: a fixture HTTP server
//! serves the mint endpoint plus the scripted GraphQL responses, following
//! the established `spawn_mint_then_api_server` pattern in `cli.rs`.
//!
//! Fixture reuse: `GITHUB_APP_FIXTURE_PEM`, `GITHUB_APP_TOKEN_MARKER`,
//! `GITHUB_APP_PEM_ENV_VAR`, and `write_github_app_project_config` are
//! re-declared here (integration tests cannot share private items across
//! files without a common module).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror `tests/campaign.rs`: the harness runs the real
// binary as a subprocess where failure means the test fails loudly.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;

use miette::IntoDiagnostic;

const GITHUB_APP_FIXTURE_PEM: &str =
    include_str!("../../orchestraitor-core/tests/fixtures/github-app-rsa-test.pem");
const GITHUB_APP_TOKEN_MARKER: &str = "ghs_prstatusE2eTOKENmarker";
const GITHUB_APP_PEM_ENV_VAR: &str = "ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM";

const HEAD: &str = "cb7cf776939f65981f9fb7c9e5263ba0b20b7c7c";
const BASE: &str = "9cc67bf0db85a7248a9b9718974f4460d5fb3759";

fn pr_view_json(head: &str, base: &str, review_decision: &str, is_draft: bool) -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "number": 614,
            "headRefOid": head,
            "baseRefOid": base,
            "isDraft": is_draft,
            "reviewDecision": review_decision,
            "mergeStateStatus": "CLEAN",
        }}}
    })
    .to_string()
}

/// Two passing `CheckRun`s and one known-disabled skipped job (not-applicable).
fn checks_all_pass() -> String {
    serde_json::json!({
        "data": {"repository": {"object": {"statusCheckRollup": {"contexts": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [
                {"__typename": "CheckRun", "name": "Check & Lint", "status": "COMPLETED",
                 "conclusion": "SUCCESS", "detailsUrl": "https://ci/lint",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}}},
                {"__typename": "CheckRun", "name": "Test", "status": "COMPLETED",
                 "conclusion": "SUCCESS", "detailsUrl": "https://ci/test",
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

fn threads_none() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [],
        }}}}
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

fn body_with_unchecked_item() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "body": "- [x] <!-- orc:convergence --> evidence recorded\n- [ ] <!-- orc:docs --> docs updated",
        }}}
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

fn error_response(message: &str) -> String {
    serde_json::json!({
        "data": null,
        "errors": [{"message": message}],
    })
    .to_string()
}

/// A fixture server serving the mint endpoint plus ordered scripted
/// GraphQL payloads (one per subsequent connection, last one repeats).
fn spawn_pr_status_server(graphql_payloads: Vec<String>) -> miette::Result<String> {
    spawn_pr_status_server_with_status(201, graphql_payloads)
}

/// Variant whose mint endpoint answers with a non-201 status (rate-limit
/// simulation: GitHub returns 403/429 on exhausted quotas).
fn spawn_pr_status_server_with_status(
    mint_status: u16,
    graphql_payloads: Vec<String>,
) -> miette::Result<String> {
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
                        // Read the FULL request (headers + body): stopping at
                        // the header terminator leaves the POST body unread,
                        // and closing a socket with unread data sends RST —
                        // racing the client's response read. Read until the
                        // declared content-length has arrived.
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
                let status_line = format!("HTTP/1.1 {mint_status}");
                (status_line, mint_body.clone())
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

/// Writes the project config (`github_app` + the `origin` remote the slug
/// resolution falls back to) into a temp project directory.
fn write_project_config(temp: &tempfile::TempDir) -> miette::Result<()> {
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nclient_id = \"Iv1.cli-e2e\"\ninstallation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()
}

/// Initializes a git repo in the temp dir with the fixture remote, so
/// `--repo` default resolution and the review-record root resolve there.
fn write_git_repo(temp: &tempfile::TempDir) -> miette::Result<String> {
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    // Scrub the ambient GIT_* location variables (the production code does
    // the same): a hook-exported GIT_DIR/GIT_WORK_TREE would redirect the
    // fixture commits into the caller's real repository. Every call's exit
    // status is asserted — a silently failed fixture step must fail the
    // test, not poison later assertions.
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

/// Writes a CLEAN review record for (HEAD, BASE) into the repo.
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

fn pr_status_cli(
    project_dir: &str,
    endpoint: &str,
    args: &[&str],
) -> miette::Result<std::process::Output> {
    // The engine reads the CLEAN-record relative to the git toplevel of its
    // CWD (convergence-status's contract), so the fixture runs INSIDE the
    // fixture repo.
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

/// The full happy-path payload set for a MERGEABLE PR at (HEAD, BASE):
/// pre-view, checks, threads, body, post-view — every gate green.
fn mergeable_payloads() -> Vec<String> {
    vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ]
}

#[test]
fn pr_status_mergeable_exits_zero_with_full_schema() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let endpoint = spawn_pr_status_server(mergeable_payloads())?;

    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let verdict: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    // The full §9.48 schema in ONE invocation.
    for key in [
        "number",
        "repository",
        "head_sha",
        "base_oid",
        "is_draft",
        "review_decision",
        "merge_state",
        "checks",
        "checks_all_passed",
        "checks_any_failing",
        "checks_any_pending",
        "threads",
        "checklist",
        "review_record",
        "convergence",
        "fetched_at",
    ] {
        assert!(verdict.get(key).is_some(), "missing schema field {key}");
    }
    assert_eq!(verdict["number"], 614);
    assert_eq!(verdict["repository"], "arbsec/orchestraitor");
    assert_eq!(verdict["head_sha"], HEAD);
    assert_eq!(verdict["base_oid"], BASE);
    assert_eq!(verdict["merge_state"], "CLEAN");
    assert_eq!(verdict["convergence"]["mergeable"], true);
    assert_eq!(verdict["convergence"]["reasons"], serde_json::json!([]));
    assert_eq!(verdict["checks_all_passed"], true);
    assert_eq!(verdict["checks_any_failing"], false);
    assert_eq!(verdict["checks_any_pending"], false);
    // Known-disabled job classified not-applicable, never a gate failure.
    let checks = verdict["checks"].as_array().expect("checks array");
    assert_eq!(checks.len(), 3);
    let docs = checks
        .iter()
        .find(|check| check["name"] == "Docs Consistency")
        .expect("docs check");
    assert_eq!(docs["bucket"], "skipping");
    assert_eq!(docs["classification"], "not-applicable");
    assert_eq!(verdict["checklist"]["all_checked"], true);
    assert_eq!(verdict["checklist"]["unchecked_count"], 0);
    assert_eq!(verdict["review_record"]["present"], true);
    assert_eq!(verdict["review_record"]["verdict"], "CLEAN");
    assert_eq!(verdict["review_record"]["head_sha_match"], true);
    assert_eq!(verdict["review_record"]["base_oid_match"], true);
    let _ = repo; // repo outlives the server via temp
    Ok(())
}

#[test]
fn pr_status_blocked_pr_is_a_successful_read_exit_zero() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // NO clean record + failing check + actionable thread + unchecked item
    // + REVIEW_REQUIRED: five reasons, exit 0, mergeable false.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "REVIEW_REQUIRED", false),
        checks_failing(),
        threads_one_actionable(),
        body_with_unchecked_item(),
        pr_view_json(HEAD, BASE, "REVIEW_REQUIRED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;

    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    // A BLOCKED verdict is a SUCCESSFUL read — exit 0, never an error.
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let verdict: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(verdict["convergence"]["mergeable"], false);
    let reasons = verdict["convergence"]["reasons"]
        .as_array()
        .expect("reasons");
    let rendered: Vec<&str> = reasons.iter().filter_map(|r| r.as_str()).collect();
    assert_eq!(
        rendered,
        [
            "agent_review_not_clean_at_head",
            "checks_not_passing",
            "unresolved_threads(1)",
            "unchecked_items(1)",
            "review_required",
        ],
        "reason codes must match convergence-status's vocabulary in order: {rendered:?}"
    );
    let _ = rendered;
    Ok(())
}

#[test]
fn pr_status_human_one_pager_renders_verdict_first() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    let endpoint = spawn_pr_status_server(mergeable_payloads())?;

    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    assert!(stdout.starts_with("MERGEABLE:"), "{stdout}");
    assert!(stdout.contains(&HEAD[..7]));
    Ok(())
}

#[test]
fn pr_status_concurrent_edit_exits_four_naming_the_anchor() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // The post-read view sees a DIFFERENT head: a push landed mid-read.
    let moved_head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        pr_view_json(moved_head, BASE, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;

    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(4),
        "concurrent-edit exit 4 (stderr: {}; stdout: {})",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(
        stderr.contains("head"),
        "EXIT: {:?} OUT: [{}] ERR: [{}]",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        stderr.contains("moved between the pre- and post-reads"),
        "{stderr}"
    );
    // NEVER a verdict from a mixed snapshot: no JSON on stdout.
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "stdout must be empty on a mixed-snapshot refusal"
    );
    Ok(())
}

#[test]
fn pr_status_base_advance_between_reads_is_concurrent_edit_too() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    let moved_base = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, moved_base, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert_eq!(output.status.code(), Some(4));
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(stderr.contains("base"), "{stderr}");
    Ok(())
}

#[test]
fn pr_status_unread_sub_check_exits_one_after_emitting_typed_json() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // The THREADS sub-read fails (GraphQL errors): the verdict must record
    // unread(threads), report mergeable false, emit the JSON, and EXIT 1 —
    // never masquerade as a BLOCKED verdict (exit 0).
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        error_response("thread read failed"),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;

    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1), "unread sub-check exits 1");
    // The typed JSON rides the diagnostic (emitted to stderr) so a caller
    // needing the partial state can read it.
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let json_line = stderr
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| miette::miette!("expected partial JSON on stderr: {stderr}"))?;
    let partial: serde_json::Value = serde_json::from_str(json_line).into_diagnostic()?;
    assert_eq!(partial["convergence"]["mergeable"], false);
    let reasons = partial["convergence"]["reasons"]
        .as_array()
        .expect("reasons");
    assert!(
        reasons.iter().any(|reason| reason
            .as_str()
            .unwrap_or_default()
            .contains("unread(threads)")),
        "reasons must carry unread(threads): {reasons:?}"
    );
    Ok(())
}

#[test]
fn pr_status_rate_limited_mint_exits_one_without_token_material() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // Mint endpoint answers 403 (exhausted quota): unrecoverable, exit 1,
    // and the fixture token value never appears anywhere.
    let endpoint = spawn_pr_status_server_with_status(403, mergeable_payloads())?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
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
        "no token material may appear: {combined}"
    );
    Ok(())
}

#[test]
fn pr_status_nonexistent_pr_is_a_config_error_exit_two() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // GraphQL resolves but the PR is null -> pullRequest missing.
    // GitHub's real shape for a missing PR: `pullRequest: null` PLUS an
    // errors entry typed NOT_FOUND — the NOT_FOUND mapping must win (exit 2).
    let payloads = vec![serde_json::json!({
        "data": {"repository": {"pullRequest": null}},
        "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a PullRequest with the number of 999999."}],
    })
    .to_string()];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["pr", "status", "999999", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(2),
        "PR not found is config/state"
    );
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(stderr.contains("not found"), "{stderr}");
    Ok(())
}

#[test]
fn pr_status_missing_github_app_config_exits_two() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // No orchestraitor.toml at all: the service identity cannot resolve.
    let repo = write_git_repo(&temp)?;
    let _ = repo;
    let endpoint = spawn_pr_status_server(mergeable_payloads())?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(stderr.contains("github_app"), "{stderr}");
    Ok(())
}

#[test]
fn pr_status_malformed_config_toml_exits_two() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // Partial github_app block (client_id only; installation_id and
    // private_key_uri missing) — an incomplete service identity is
    // config/state exit 2, not a mint failure.
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nclient_id = \"Iv1.cli-e2e\"\n",
    )
    .into_diagnostic()?;
    let repo = write_git_repo(&temp)?;
    let _ = repo;
    let endpoint = spawn_pr_status_server(mergeable_payloads())?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(stderr.contains("github_app"), "{stderr}");
    Ok(())
}

#[test]
fn pr_status_unresolvable_private_key_is_unrecoverable_exit_one() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // Complete github_app block but the PEM env var is never set: config
    // VALIDATES, minting fails closed -> unrecoverable exit 1.
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nclient_id = \"Iv1.cli-e2e\"\ninstallation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_NEVER_SET\"\n",
    )
    .into_diagnostic()?;
    let repo = write_git_repo(&temp)?;
    let _ = repo;
    let endpoint = spawn_pr_status_server(mergeable_payloads())?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains(GITHUB_APP_TOKEN_MARKER),
        "no token material may appear: {combined}"
    );
    Ok(())
}

#[test]
fn pr_status_malformed_checks_payload_is_unrecoverable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // The checks response is valid JSON but misses the contexts shape.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        r#"{"data":{"repository":{"object":{}}}}"#.to_string(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let json_line = stderr
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| miette::miette!("expected partial JSON on stderr: {stderr}"))?;
    let partial: serde_json::Value = serde_json::from_str(json_line).into_diagnostic()?;
    assert!(
        partial["convergence"]["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or_default()
                .starts_with("unread(checks)")),
        "malformed payload records unread(checks): {partial}"
    );
    Ok(())
}

#[test]
fn pr_status_transport_failure_is_unrecoverable_exit_one() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    // Nothing listens on this port: the transport read fails closed.
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        "http://127.0.0.1:1",
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1), "transport failure is exit 1");
    Ok(())
}

#[test]
fn pr_status_draft_pr_reports_is_draft_reason_and_exit_zero() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // Draft PR: every gate green except is_draft.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", true),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", true),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(output.status.success(), "BLOCKED-ish verdicts still exit 0");
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let verdict: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(verdict["is_draft"], true);
    assert_eq!(verdict["convergence"]["mergeable"], false);
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["is_draft"])
    );
    Ok(())
}

#[test]
fn pr_status_known_disabled_failing_check_is_a_failure_never_a_pass() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // "Documentation" (known-disabled) actually FAILED: it must classify
    // fail and block the gate — the state-blind exemption is forbidden.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        serde_json::json!({
        "data": {"repository": {"object": {"statusCheckRollup": {"contexts": {
            "pageInfo": {"hasNextPage": false},
            "nodes": [
                {"__typename": "CheckRun", "name": "Documentation", "status": "COMPLETED",
                 "conclusion": "FAILURE", "detailsUrl": "",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}}},
            ],
        }}}}}})
        .to_string(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let verdict: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    let checks = verdict["checks"].as_array().expect("checks");
    assert_eq!(checks[0]["classification"], "fail");
    assert_eq!(verdict["checks_all_passed"], false);
    assert_eq!(verdict["checks_any_failing"], true);
    assert!(
        verdict["convergence"]["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason.as_str() == Some("checks_not_passing")),
        "{verdict}"
    );
    Ok(())
}

#[test]
fn pr_status_null_rollup_is_blocked_not_unrecoverable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // A head commit with no status contexts yet (fresh push): rollup null.
    // Valid provider state -> BLOCKED with checks_not_passing, exit 0.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        serde_json::json!({"data": {"repository": {"object": {"statusCheckRollup": null}}}})
            .to_string(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let verdict: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    assert_eq!(verdict["checks"].as_array().expect("checks").len(), 0);
    assert_eq!(verdict["checks_all_passed"], false);
    assert_eq!(
        verdict["convergence"]["mergeable"], false,
        "no checks = not all passed (pr-checks length > 0 rule)"
    );
    assert!(
        verdict["convergence"]["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason.as_str() == Some("checks_not_passing")),
        "{verdict}"
    );
    Ok(())
}

#[test]
fn pr_status_partial_graphql_error_degrades_not_fails() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // GitHub answers HTTP 200 with data populated AND an errors entry
    // (FORBIDDEN on checkSuite.workflowRun, which an installation without
    // the Actions read permission hits). The reads must DEGRADE — the
    // checks still classify from the surviving fields — never unread(checks)
    // exit 1.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        serde_json::json!({
            "data": {"repository": {"object": {"statusCheckRollup": {"contexts": {
                "pageInfo": {"hasNextPage": false},
                "nodes": [
                    {"__typename": "CheckRun", "name": "Check & Lint", "status": "COMPLETED",
                     "conclusion": "SUCCESS", "detailsUrl": "https://ci/lint",
                     "checkSuite": null}
                ],
            }}}}},
            "errors": [{"type": "FORBIDDEN",
                        "message": "Resource not accessible by integration",
                        "path": ["repository", "object", "statusCheckRollup", "contexts",
                                 "nodes", 0, "checkSuite", "workflowRun"]}]
        })
        .to_string(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let endpoint = spawn_pr_status_server(payloads)?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let verdict: serde_json::Value = serde_json::from_str(&stdout).into_diagnostic()?;
    let checks = verdict["checks"].as_array().expect("checks");
    assert_eq!(checks.len(), 1, "the surviving check still classifies");
    assert_eq!(checks[0]["classification"], "pass");
    assert_eq!(verdict["checks_all_passed"], true);
    Ok(())
}

#[test]
fn pr_status_no_secrets_in_output() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    let endpoint = spawn_pr_status_server(mergeable_payloads())?;
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &endpoint,
        &[
            "pr",
            "status",
            "614",
            "--repo",
            "arbsec/orchestraitor",
            "--json",
        ],
    )?;
    assert!(output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains(GITHUB_APP_TOKEN_MARKER) && !combined.contains("ghs_"),
        "the §9.48 output carries no credential material: {combined}"
    );
    assert!(
        !combined.to_lowercase().contains("authorization"),
        "no header echoes: {combined}"
    );
    Ok(())
}

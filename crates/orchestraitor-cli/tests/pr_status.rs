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
use std::path::Path;
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
    pr_view_json_authored(head, base, review_decision, is_draft, "", "")
}

/// `pr_view_json` with an explicit PR author — the discriminator for the
/// structural bot-author `CodeRabbit` skip. `author_type` is the GraphQL
/// `__typename` (`"Bot"` for a GitHub App/bot identity, `"User"` for a human).
fn pr_view_json_authored(
    head: &str,
    base: &str,
    review_decision: &str,
    is_draft: bool,
    author_login: &str,
    author_type: &str,
) -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "number": 614,
            "state": "OPEN",
            "headRefOid": head,
            "baseRefOid": base,
            "isDraft": is_draft,
            "reviewDecision": review_decision,
            "mergeStateStatus": "CLEAN",
            "author": {"__typename": author_type, "login": author_login},
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
                {"__typename": "CheckRun", "name": "Documentation", "status": "COMPLETED",
                 "conclusion": "SKIPPED", "detailsUrl": "",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}}},
            ],
        }}}}}
    })
    .to_string()
}

/// Two passing `CheckRun`s plus `CodeRabbit`'s structural bot-author SKIPPED
/// (the #707 exemption; the author discriminator lives in the PR view).
fn checks_green_with_bot_code_rabbit_skip() -> String {
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
                {"__typename": "CheckRun", "name": "CodeRabbit", "title": "",
                 "status": "COMPLETED", "conclusion": "SKIPPED", "detailsUrl": "",
                 "checkSuite": {"workflowRun": {"workflow": {"name": "coderabbit.yml"}}}},
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
            "body": "- [x] <!-- orc:issue --> evidence recorded\n- [ ] <!-- orc:docs --> docs updated",
        }}}
    })
    .to_string()
}

fn body_all_checked() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "body": "- [x] <!-- orc:issue --> evidence recorded",
        }}}
    })
    .to_string()
}

/// A body whose fenced scope is INDETERMINATE: the opener is indented two
/// spaces, so it may belong to the list item above it or to the top level, and
/// the region swallows a genuine UNCHECKED item (arbsec/orchestraitor#809).
fn body_with_indented_fence() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "body": "- [x] <!-- orc:issue --> evidence recorded\n- Example:\n  ```\n  sample\n\n- [ ] <!-- orc:docs --> docs updated",
        }}}
    })
    .to_string()
}

/// A body carrying an inline code span with a comment token, then a VISIBLE
/// unchecked item (arbsec/orchestraitor#799 gen-9). Only a line-START `<` is a
/// refused raw HTML construct, so the span's token is literal text and changes
/// nothing. When the scan treated ANY `<!--` as a comment opener, it hid the
/// unchecked item and the verdict reported `all_checked: true` with no reasons
/// — the fail-open defect this fixture pins end to end.
fn body_with_inline_code_comment_token() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "body": "- [x] <!-- orc:issue --> linked\n\n`<!--`\n\n- [ ] <!-- orc:tests --> not tested",
        }}}
    })
    .to_string()
}

/// A body with NO managed marker at all — the #788 vacuous-pass shape
/// (observed on the merged PR #767 body).
fn body_no_markers() -> String {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "body": "Summary of the change.\n\n| script | note |\n|---|---|\n| reconcile-checklist | parses markers |",
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

/// Runs `git` inside `repo` with the ambient git LOCATION scrubbed.
///
/// `git -C <dir>` does NOT isolate the git location: a hook-exported
/// `GIT_DIR`/`GIT_WORK_TREE` wins over `-C`, so an inherited location would
/// redirect the fixture's commits into the caller's real repository.
/// `GIT_INDEX_FILE` has the same effect on the index. The scrub is applied
/// AFTER `overrides`, so it always wins — `overrides` exist to SIMULATE a
/// hostile ambient location in the regression test, never to defeat the
/// scrub.
///
/// Every call's exit status is asserted — a silently failed fixture step must
/// fail the test, not poison later assertions.
fn git_in(
    repo: &std::path::Path,
    overrides: &[(&str, &str)],
    args: &[&str],
) -> miette::Result<std::process::Output> {
    let mut command = Command::new("git");
    command.args(["-C", &repo.display().to_string()]).args(args);
    for (name, value) in overrides {
        command.env(name, value);
    }
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
    Ok(output)
}

/// Initializes a git repo in the temp dir with the fixture remote, so
/// `--repo` default resolution and the review-record root resolve there.
fn write_git_repo(temp: &tempfile::TempDir) -> miette::Result<String> {
    write_git_repo_with_env(temp, &[])
}

/// The same fixture, but the fixture's git calls carry `overrides` in the
/// CHILD environment. The regression test uses it to simulate a hook-exported
/// `GIT_DIR`/`GIT_WORK_TREE`; the scrub inside [`git_in`] always wins, so the
/// overrides never redirect the fixture's own commits.
fn write_git_repo_with_env(
    temp: &tempfile::TempDir,
    overrides: &[(&str, &str)],
) -> miette::Result<String> {
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    git_in(&repo, overrides, &["init", "-q"])?;
    git_in(
        &repo,
        overrides,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/arbsec/orchestraitor.git",
        ],
    )?;
    git_in(
        &repo,
        overrides,
        &["config", "user.email", "test@example.com"],
    )?;
    git_in(&repo, overrides, &["config", "user.name", "test"])?;
    fs::write(repo.join("README.md"), "fixture\n").into_diagnostic()?;
    git_in(&repo, overrides, &["add", "."])?;
    git_in(&repo, overrides, &["commit", "-q", "-m", "fixture"])?;
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
        .find(|check| check["name"] == "Documentation")
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
fn pr_status_bot_authored_code_rabbit_skip_is_not_a_gate_failure() -> miette::Result<()> {
    // A green PR whose only non-pass check is CodeRabbit's structural
    // bot-author SKIPPED: all_passed true, mergeable true — the pr-checks
    // #707 rule consumed through the GraphQL author field (issue #740).
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let endpoint = spawn_pr_status_server(vec![
        pr_view_json_authored(HEAD, BASE, "APPROVED", false, "arbsec-agent", "Bot"),
        checks_green_with_bot_code_rabbit_skip(),
        threads_none(),
        body_all_checked(),
        pr_view_json_authored(HEAD, BASE, "APPROVED", false, "arbsec-agent", "Bot"),
    ])?;

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
    assert_eq!(verdict["checks_all_passed"], true);
    assert_eq!(verdict["checks_any_failing"], false);
    assert_eq!(verdict["checks_any_pending"], false);
    assert_eq!(verdict["convergence"]["mergeable"], true);
    assert_eq!(verdict["convergence"]["reasons"], serde_json::json!([]));
    let rabbit = verdict["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"] == "CodeRabbit")
        .expect("CodeRabbit check");
    assert_eq!(rabbit["bucket"], "skipping");
    assert_eq!(rabbit["classification"], "not-applicable");
    assert_eq!(rabbit["skip_reason"], "bot-author");
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
fn pr_status_marker_less_body_blocks_with_checklist_markers_missing() -> miette::Result<()> {
    // #788: every other gate is green, and the body carries NO managed marker.
    // This used to be `mergeable: true` (jq's `all()` over an empty array is
    // true), which merge-gate accepted — the vacuous pass observed on PR #767.
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_no_markers(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let text_endpoint = spawn_pr_status_server(payloads.clone())?;
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
    assert_eq!(verdict["convergence"]["mergeable"], false);
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["checklist_markers_missing"]),
        "a missing checklist must never be reported as unchecked_items(0)"
    );
    assert_eq!(verdict["checklist"]["marker_count"], 0);
    assert_eq!(verdict["checklist"]["markers_present"], false);
    assert_eq!(verdict["checklist"]["all_checked"], false);
    assert_eq!(verdict["checklist"]["unchecked_count"], 0);
    assert!(
        verdict["checklist"].get("waived").is_none(),
        "there is no author-granted opt-out: the checklist object carries no waiver field"
    );

    // The one-pager names the state instead of rendering "0 unchecked item(s)".
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &text_endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).into_diagnostic()?;
    assert!(
        text.contains("no managed orc:* markers found (UNSUBSTANTIATED"),
        "{text}"
    );
    Ok(())
}

#[test]
fn pr_status_indented_fence_blocks_with_checklist_unparseable() -> miette::Result<()> {
    // #809 (fail closed): a fence OPENER indented 1-3 spaces may belong to a
    // list item or a block quote, and the scanner has no container model. The
    // region it models swallows a genuine unchecked item, so the reading is
    // REFUSED with a distinct reason instead of guessed — it used to report
    // `all_checked: true` and converge.
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_with_indented_fence(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let text_endpoint = spawn_pr_status_server(payloads.clone())?;
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
    assert_eq!(verdict["convergence"]["mergeable"], false, "{verdict}");
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["checklist_unparseable"]),
        "an indeterminate fenced scope is its own reason, never markers_missing or all-checked"
    );
    assert_eq!(verdict["checklist"]["unparseable"], true);
    assert_eq!(verdict["checklist"]["unparseable_line"], 3);
    assert_eq!(
        verdict["checklist"]["all_checked"], false,
        "an indeterminate scan is never 'all checked'"
    );

    // The one-pager names the construct and the fix.
    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &text_endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).into_diagnostic()?;
    assert!(text.starts_with("BLOCKED:"), "{text}");
    assert!(text.contains("UNPARSEABLE"), "{text}");
    assert!(text.contains("indented 1-3 spaces"), "{text}");
    assert!(
        text.contains("fix: dedent the fence opener to column 0"),
        "{text}"
    );
    // The remedy must be one that WORKS: closing the region never clears the
    // (sticky) indeterminate condition, so no message may offer it.
    assert!(!text.contains("or close it"), "{text}");
    Ok(())
}

#[test]
fn pr_status_inline_code_comment_token_keeps_the_item_visible() -> miette::Result<()> {
    // End-to-end regression for the fail-open defect: nine markers ticked would
    // be fine — here ONE is ticked and one is visibly unticked behind an inline
    // code span carrying `<!--`. The verdict must BLOCK on the item count, so
    // `merge-gate` never reaches `gh pr merge`.
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        body_with_inline_code_comment_token(),
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
    assert_eq!(verdict["convergence"]["mergeable"], false, "{verdict}");
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["unchecked_items(1)"]),
        "the visible unticked item behind an inline code token is evidence"
    );
    assert_eq!(verdict["checklist"]["unparseable"], false, "{verdict}");
    assert_eq!(verdict["checklist"]["marker_count"], 2, "{verdict}");
    assert_eq!(
        verdict["checklist"]["all_checked"], false,
        "a visible unticked item must never read as all-checked"
    );
    Ok(())
}

#[test]
fn pr_status_body_sentinel_is_not_an_opt_out() -> miette::Result<()> {
    // #788 revision: the `<!-- orc:none -->` waiver was REMOVED — a PR author
    // cannot grant themselves the exemption (arbsec/orchestraitor#801). A body
    // that carries the sentinel and no marker still blocks.
    //
    // #799 R1: on its own line the sentinel is also a raw HTML construct, so
    // the block reason is `checklist_unparseable` (`html_construct`) instead of
    // `checklist_markers_missing`. Stricter, and still no waiver.
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "body": "Machine-authored summary.\n\n<!-- orc:none --> delivery summary only\n",
            }}}
        })
        .to_string(),
        pr_view_json(HEAD, BASE, "APPROVED", false),
        pr_view_json(HEAD, BASE, "APPROVED", false),
    ];
    let text_endpoint = spawn_pr_status_server(payloads.clone())?;
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
    assert_eq!(verdict["convergence"]["mergeable"], false, "{verdict}");
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["checklist_unparseable"]),
        "a body sentinel must never waive the marker requirement"
    );
    assert_eq!(verdict["checklist"]["markers_present"], false);
    assert_eq!(verdict["checklist"]["all_checked"], false);
    assert_eq!(verdict["checklist"]["unparseable"], true, "{verdict}");

    let output = pr_status_cli(
        &temp.path().display().to_string(),
        &text_endpoint,
        &["pr", "status", "614", "--repo", "arbsec/orchestraitor"],
    )?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).into_diagnostic()?;
    assert!(
        text.starts_with("BLOCKED:"),
        "a marker-less body with a sentinel still blocks: {text}"
    );
    assert!(
        text.contains("checklist: UNPARSEABLE — the PR body contains a raw HTML construct"),
        "the operator view names the refused construct: {text}"
    );
    assert!(
        text.contains("put it inside a fenced code block, indent it by 4 spaces, or remove it"),
        "and offers remedies that work: {text}"
    );
    Ok(())
}

#[test]
fn pr_status_fenced_marker_is_not_a_checklist_item() -> miette::Result<()> {
    // #788: a marker inside a fenced code region is an EXAMPLE, not evidence.
    // The body below shows the checklist shape and carries no real item.
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    let repo = write_git_repo(&temp)?;
    write_clean_record(&repo)?;
    let payloads = vec![
        pr_view_json(HEAD, BASE, "APPROVED", false),
        checks_all_pass(),
        threads_none(),
        serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "body": "Here is the shape:\n\n```markdown\n- [x] <!-- orc:issue --> example only\n```\n",
            }}}
        })
        .to_string(),
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
    assert_eq!(verdict["checklist"]["marker_count"], 0, "{verdict}");
    assert_eq!(verdict["checklist"]["markers_present"], false);
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["checklist_markers_missing"]),
        "a fenced example must never satisfy the checklist gate"
    );
    Ok(())
}

#[test]
fn fixture_git_ignores_an_ambient_git_location() -> miette::Result<()> {
    // F1 regression: a hook-exported GIT_DIR/GIT_WORK_TREE must never
    // redirect the fixture's own git calls. `git -C <dir>` does NOT isolate
    // the git location — the location wins over `-C` — so without the scrub
    // the fixture's `git add .` / `git commit` would stage and commit the
    // CALLER's working tree.
    let temp = tempfile::tempdir().into_diagnostic()?;
    let sentinel = temp.path().join("sentinel");
    fs::create_dir_all(&sentinel).into_diagnostic()?;
    git_in(&sentinel, &[], &["init", "-q"])?;
    git_in(
        &sentinel,
        &[],
        &["config", "user.email", "sentinel@example.com"],
    )?;
    git_in(&sentinel, &[], &["config", "user.name", "sentinel"])?;
    fs::write(sentinel.join("README.md"), "sentinel\n").into_diagnostic()?;
    git_in(&sentinel, &[], &["add", "."])?;
    git_in(&sentinel, &[], &["commit", "-q", "-m", "sentinel"])?;
    let sentinel_head =
        String::from_utf8_lossy(&git_in(&sentinel, &[], &["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_string();

    // The hostile location: GIT_DIR/GIT_WORK_TREE point at the sentinel.
    let sentinel_git_dir = sentinel.join(".git").display().to_string();
    let sentinel_work_tree = sentinel.display().to_string();
    let repo = write_git_repo_with_env(
        &temp,
        &[
            ("GIT_DIR", sentinel_git_dir.as_str()),
            ("GIT_WORK_TREE", sentinel_work_tree.as_str()),
        ],
    )?;

    // The fixture repo carries ONLY its own commit — nothing leaked in from
    // the sentinel, and the sentinel's own commit is not the fixture's.
    let fixture_head =
        String::from_utf8_lossy(&git_in(Path::new(&repo), &[], &["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_string();
    assert_ne!(
        fixture_head, sentinel_head,
        "the fixture's commit landed in the sentinel repository"
    );
    let fixture_log =
        String::from_utf8_lossy(&git_in(Path::new(&repo), &[], &["log", "--format=%s"])?.stdout)
            .to_string();
    assert_eq!(fixture_log.trim(), "fixture", "{fixture_log}");
    let fixture_status =
        String::from_utf8_lossy(&git_in(Path::new(&repo), &[], &["status", "--porcelain"])?.stdout)
            .to_string();
    assert!(fixture_status.trim().is_empty(), "{fixture_status}");

    // The sentinel repository is untouched: one commit, a clean work tree, and
    // no file from the fixture.
    let sentinel_log =
        String::from_utf8_lossy(&git_in(&sentinel, &[], &["log", "--format=%s"])?.stdout)
            .to_string();
    assert_eq!(
        sentinel_log.trim(),
        "sentinel",
        "the sentinel repository gained a commit: {sentinel_log}"
    );
    let sentinel_status =
        String::from_utf8_lossy(&git_in(&sentinel, &[], &["status", "--porcelain"])?.stdout)
            .to_string();
    assert!(
        sentinel_status.trim().is_empty(),
        "the sentinel work tree was modified: {sentinel_status}"
    );
    let sentinel_files: Vec<String> = std::fs::read_dir(&sentinel)
        .into_diagnostic()?
        .filter_map(|entry| {
            entry
                .ok()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
        })
        .collect();
    assert!(
        !sentinel_files.iter().any(|name| name == "repo"),
        "the fixture repo was created inside the sentinel: {sentinel_files:?}"
    );
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
fn pr_status_changes_requested_blocks_the_verdict() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // CHANGES_REQUESTED blocks even with every other gate green.
    let payloads = vec![
        pr_view_json(HEAD, BASE, "CHANGES_REQUESTED", false),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        pr_view_json(HEAD, BASE, "CHANGES_REQUESTED", false),
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
    assert_eq!(verdict["convergence"]["mergeable"], false);
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["review_changes_requested"])
    );
    Ok(())
}

#[test]
fn pr_status_closed_pr_never_reports_mergeable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // A closed PR with unchanged head/base/checks/record cannot merge.
    let payloads = vec![
        serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "number": 614, "state": "CLOSED",
                "headRefOid": HEAD, "baseRefOid": BASE,
                "isDraft": false, "reviewDecision": "APPROVED",
                "mergeStateStatus": "CLEAN",
            }}}
        })
        .to_string(),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "number": 614, "state": "CLOSED",
                "headRefOid": HEAD, "baseRefOid": BASE,
                "isDraft": false, "reviewDecision": "APPROVED",
                "mergeStateStatus": "CLEAN",
            }}}
        })
        .to_string(),
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
    assert_eq!(verdict["convergence"]["mergeable"], false);
    assert_eq!(
        verdict["convergence"]["reasons"],
        serde_json::json!(["pr_not_open"])
    );
    Ok(())
}

#[test]
fn pr_status_null_review_decision_without_errors_is_no_requirement() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // A base branch that does not require reviews: reviewDecision null is a
    // normal provider state — every gate green stays MERGEABLE, exit 0.
    let view = |decision: serde_json::Value| {
        serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "number": 614, "state": "OPEN",
                "headRefOid": HEAD, "baseRefOid": BASE,
                "isDraft": false, "reviewDecision": decision,
                "mergeStateStatus": "CLEAN",
            }}}
        })
        .to_string()
    };
    let payloads = vec![
        view(serde_json::Value::Null),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        view(serde_json::Value::Null),
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
    assert_eq!(verdict["convergence"]["mergeable"], true);
    assert_eq!(verdict["convergence"]["reasons"], serde_json::json!([]));
    Ok(())
}

#[test]
fn pr_status_null_review_decision_with_partial_error_is_unread() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_project_config(&temp)?;
    write_git_repo(&temp)?;
    write_clean_record(&temp.path().join("repo").display().to_string())?;
    // reviewDecision null WITH a partial field error: unread(review-decision),
    // mergeable false, exit 1 after the typed JSON.
    let view = || {
        serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "number": 614, "state": "OPEN",
                "headRefOid": HEAD, "baseRefOid": BASE,
                "isDraft": false, "reviewDecision": null,
                "mergeStateStatus": "CLEAN",
            }}},
            "errors": [{"message": "reviewDecision is unavailable",
                        "path": ["repository", "pullRequest", "reviewDecision"]}]
        })
        .to_string()
    };
    let payloads = vec![
        view(),
        checks_all_pass(),
        threads_none(),
        body_all_checked(),
        view(),
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
    assert_eq!(partial["convergence"]["mergeable"], false);
    assert!(
        partial["convergence"]["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason.as_str() == Some("unread(review-decision)"))
    );
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

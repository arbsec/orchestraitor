//! Integration tests for `orc simplify run`: the pass is exercised through
//! the real `orc` binary against a fixture workspace — hermetic (no network,
//! no specific toolchain beyond cargo itself), deterministic.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::process::{Command, Output};

use tempfile::TempDir;

fn run_orc(args: &[&str], cwd: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("orc binary runs")
}

/// A fixture workspace: minimal cargo project + orchestraitor.toml so the
/// layered config resolves, plus a git repo so `--staged` has an index.
fn fixture_workspace() -> TempDir {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn sample() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("orchestraitor.toml"),
        "[simplify]\nenabled = true\nauto_apply_format = true\n",
    )
    .unwrap();
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
            .current_dir(&project)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
    temp
}

#[test]
fn simplify_run_succeeds_on_clean_fixture() {
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
        ],
        &project,
    );
    // Fail-open: the command exits 0 even when tools are missing/unhappy.
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("simplify: ran ="), "stdout: {stdout}");
}

#[test]
fn simplify_run_emits_stable_json() {
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
            "--json",
        ],
        &project,
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON report");
    assert!(value.get("ran").is_some());
    assert!(value.get("ran_rules_only").is_some());
    assert!(value.get("auto_applied_count").is_some());
    assert!(value.get("suggestions").is_some());
    assert!(value.get("tools").is_some());
}

#[test]
fn simplify_run_respects_disabled_config() {
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    fs::write(
        project.join("orchestraitor.toml"),
        "[simplify]\nenabled = false\n",
    )
    .unwrap();
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
        ],
        &project,
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no-op"), "stdout: {stdout}");
}

#[test]
fn simplify_run_with_fix_format_exits_zero() {
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
            "--fix",
            "format",
        ],
        &project,
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn simplify_run_staged_scope_exits_zero_without_staging() {
    // --staged with an empty index is not an error: the scope filter keeps
    // workspace-level findings and the pass still succeeds.
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
            "--staged",
        ],
        &project,
    );
    assert!(output.status.success());
}

#[test]
fn simplify_paths_scope_exits_zero() {
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
            "--path",
            "src/lib.rs",
        ],
        &project,
    );
    assert!(output.status.success());
}

#[test]
fn simplify_pedantic_check_passes_on_clean_fixture() {
    // A clean fixture has no unaddressed suggestions above Format, so the
    // pedantic-check mode exits 0.
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
            "--pedantic-check",
        ],
        &project,
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn simplify_unknown_config_key_warns_but_runs() {
    // Unknown keys are reported as layer warnings; the pass still runs
    // (fail-open) because they are warnings, not parse errors.
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    fs::write(
        project.join("orchestraitor.toml"),
        "[simplify]\nenabled = true\nbogus_key = 1\n",
    )
    .unwrap();
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
            "--json",
        ],
        &project,
    );
    assert!(output.status.success());
}

#[test]
fn simplify_config_failure_fails_open_with_warning_and_skip() {
    // A broken config (validation failure) must NOT propagate as a non-zero
    // exit without --pedantic-check: the fail-open contract is a typed
    // ORC-SIMPLIFY-001 warning and a skip (exit 0).
    let temp = fixture_workspace();
    let project = temp.path().join("project");
    fs::write(
        project.join("orchestraitor.toml"),
        "[simplify]\nenabled = true\nmax_passes = 0\n",
    )
    .unwrap();
    let output = run_orc(
        &[
            "--project-dir",
            project.to_str().unwrap(),
            "simplify",
            "run",
        ],
        &project,
    );
    assert!(
        output.status.success(),
        "config failure must fail open (exit 0); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ORC-SIMPLIFY-001"),
        "expected typed ORC-SIMPLIFY-001 warning; stderr: {stderr}"
    );
    assert!(
        stderr.contains("skipping the pass"),
        "expected fail-open skip notice; stderr: {stderr}"
    );
    // The pass never ran: stdout carries no report.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("simplify: ran ="),
        "the pass must be skipped; stdout: {stdout}"
    );
}

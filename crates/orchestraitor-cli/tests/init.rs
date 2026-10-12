//! Integration tests for the `orc init` command.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::process::{Command, Output};

use tempfile::TempDir;

fn run_orc(args: &[&str], cwd: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("orc binary runs")
}

#[test]
fn init_completes_without_provider_and_writes_general_domain() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\n",
    )
    .unwrap();
    fs::write(temp.path().join("AGENTS.md"), "# agent notes\n").unwrap();

    let output = run_orc(&["init"], temp.path());

    assert!(output.status.success());
    let config = fs::read_to_string(temp.path().join(".orchestraitor/orchestraitor.toml")).unwrap();
    assert!(config.contains("# Proposed by orc init"));
    assert!(config.contains("[agents.domains.general]"));
    assert!(config.contains("enabled = true"));
    assert!(!config.contains("api_key"));
}

#[test]
fn dry_run_writes_nothing_and_shows_proposal() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("package.json"), "{}\n").unwrap();

    let output = run_orc(&["init", "--dry-run"], temp.path());

    assert!(output.status.success());
    assert!(!temp.path().join(".orchestraitor").exists());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Dry run: would write"));
    assert!(stdout.contains("[agents.domains.general]"));
}

#[test]
fn init_does_not_prompt_for_api_key() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("pyproject.toml"),
        "[project]\nname = \"fixture\"\n",
    )
    .unwrap();

    let output = run_orc(&["init"], temp.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let rendered = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    assert!(!rendered.contains("enter"));
    assert!(!rendered.contains("prompt"));
    assert!(!rendered.contains("api key"));
    assert!(rendered.contains("no credential requested"));
}

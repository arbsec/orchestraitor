//! End-to-end command behavior for headless and CI commands (spec MVP-8).

use std::fs;

use clap::Parser;
use miette::IntoDiagnostic;
use orchestraitor_cli::cli::Cli;
use orchestraitor_cli::{ExitCode, OrcError};

fn parse(args: &[&str]) -> Cli {
    Cli::parse_from(args)
}

fn run(cli: Cli) -> (std::process::ExitCode, Vec<u8>) {
    let mut output = Vec::new();
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);
    let code = match result {
        Ok(code) => code,
        Err(error) => error.exit_code(),
    };
    (code.into(), output)
}

#[test]
fn verify_detects_cargo_test_from_cargo_toml() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "verify",
        "--json",
    ]);
    let (code, output) = run(cli);
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let entries = json["entries"]
        .as_array()
        .ok_or_else(|| miette::miette!("entries is not an array"))?;
    assert!(
        entries.iter().any(|e| e["name"] == "cargo-test"),
        "expected cargo-test in {entries:?}"
    );
    Ok(())
}

#[test]
fn verify_detects_custom_config_commands() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        r#"
[[verification.commands]]
name = "custom-lint"
command = "make lint"
trigger_files = ["Makefile"]
"#,
    )
    .into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "verify",
        "--json",
    ]);
    let (code, output) = run(cli);
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let entries = json["entries"]
        .as_array()
        .ok_or_else(|| miette::miette!("entries is not an array"))?;
    assert!(
        entries
            .iter()
            .any(|e| e["name"] == "custom-lint" && e["source"] == "project_config"),
        "expected custom-lint from project_config in {entries:?}"
    );
    Ok(())
}

#[test]
fn verify_text_output_reports_sandbox_unavailable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "verify",
    ]);
    let (_code, output) = run(cli);
    let text = String::from_utf8(output).into_diagnostic()?;

    assert!(text.contains("cargo-test"));
    assert!(text.contains("sandbox execution not available"));
    Ok(())
}

#[test]
fn policy_check_returns_config_error_without_policy_file() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "policy",
        "check",
        "--json",
    ]);
    let (code, _output) = run(cli);

    assert_eq!(
        code,
        std::process::ExitCode::from(ExitCode::ConfigError as u8)
    );
    Ok(())
}

#[test]
fn policy_check_evaluates_policy_and_returns_verdict() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let policy = r#"
version = 1

[defaults]
action = "pass"
non_interactive_prompt_action = "block"
fail_closed_on_unavailable = false

[network]
require_https = false
block_private_networks = false

[network.redirects]
max = 5
allow_https_to_http = false
allow_cross_origin = false
forward_authorization_cross_origin = false

[limits]
max_download_bytes = "100MiB"
max_analysis_time = "60s"

[integrity]
require_digest = false

[provenance]
require_signature_for = []
trusted_sigstore_identities = []

[detectors]
"#;
    let policy_path = temp.path().join("policy.toml");
    fs::write(&policy_path, policy).into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "policy",
        "check",
        "--policy",
        &policy_path.display().to_string(),
        "--json",
    ]);
    let (code, output) = run(cli);
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(json["verdict_str"], "pass");
    Ok(())
}

#[test]
fn run_non_interactive_blocks_by_default() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "run",
        "--non-interactive",
        "--quiet",
        "do something",
    ]);
    let (code, _output) = run(cli);

    assert_eq!(
        code,
        std::process::ExitCode::from(ExitCode::SecurityBlock as u8)
    );
    Ok(())
}

#[test]
fn run_non_interactive_allow_reports_infrastructure_failure() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "run",
        "--non-interactive",
        "--approval",
        "allow",
        "--quiet",
        "do something",
    ]);
    let (code, _output) = run(cli);

    assert_eq!(
        code,
        std::process::ExitCode::from(ExitCode::InfrastructureFailure as u8)
    );
    Ok(())
}

#[test]
fn run_json_output_contains_task_and_mode() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "run",
        "--non-interactive",
        "--approval",
        "allow",
        "--json",
        "fix the bug",
    ]);
    let (_code, output) = run(cli);
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(json["task"], "fix the bug");
    assert_eq!(json["non_interactive"], true);
    assert_eq!(json["approval_mode"], "allow");
    Ok(())
}

#[test]
fn evidence_export_empty_store_succeeds() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--config-dir",
        &temp.path().display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "evidence",
        "export",
        "--json",
    ]);
    let (code, output) = run(cli);
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(json["record_count"], 0);
    assert_eq!(json["mode"], "redacted");
    Ok(())
}

#[test]
fn evidence_export_redacts_sensitive_payloads() -> miette::Result<()> {
    use orchestraitor_events::{
        AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
        InMemoryAuditStore, PrivacyExportMode,
    };
    use orchestraitor_model::OperationId;

    let temp = tempfile::tempdir().into_diagnostic()?;
    let events_dir = temp.path().join("events");
    fs::create_dir_all(&events_dir).into_diagnostic()?;

    let mut store = InMemoryAuditStore::default();
    let event = EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: CURRENT_SCHEMA_VERSION,
        monotonic_seq: 1,
        wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
        correlation_id: OperationId::from_string("op_test".to_string()),
        parent_op_id: None,
        category: EventCategory::ModelRequest,
        payload: serde_json::json!({"prompt": "secret prompt here"}),
        prev_hash: None,
    })
    .into_diagnostic()?;
    store.append(event).into_diagnostic()?;
    let exported = store
        .export(PrivacyExportMode::Redacted)
        .into_diagnostic()?;
    fs::write(events_dir.join("current.jsonl"), exported).into_diagnostic()?;

    let cli = parse(&[
        "orc",
        "--config-dir",
        &temp.path().display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "evidence",
        "export",
    ]);
    let (code, output) = run(cli);
    let text = String::from_utf8(output).into_diagnostic()?;

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert!(!text.contains("secret prompt here"));
    Ok(())
}

#[test]
fn evidence_export_writes_to_file() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let out_path = temp.path().join("export.jsonl");

    let cli = parse(&[
        "orc",
        "--config-dir",
        &temp.path().display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "evidence",
        "export",
        "--output",
        &out_path.display().to_string(),
    ]);
    let (code, _output) = run(cli);

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert!(out_path.exists());
    Ok(())
}

#[test]
fn exit_codes_are_stable_and_distinguishable() {
    assert_eq!(ExitCode::Success.as_i32(), 0);
    assert_eq!(ExitCode::ConfigError.as_i32(), 2);
    assert_eq!(ExitCode::VerificationFailure.as_i32(), 3);
    assert_eq!(ExitCode::SecurityBlock.as_i32(), 4);
    assert_eq!(ExitCode::InfrastructureFailure.as_i32(), 5);
}

#[test]
fn orc_error_preserves_exit_code_through_conversion() {
    let error = OrcError::security_block(miette::miette!("blocked"));
    assert_eq!(error.exit_code(), ExitCode::SecurityBlock);
}

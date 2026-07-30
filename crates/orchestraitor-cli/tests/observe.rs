//! End-to-end behavior for `orc observe`.

use std::fs;

use clap::Parser;
use miette::IntoDiagnostic;
use orchestraitor_cli::cli::Cli;

#[test]
fn observe_requires_harness_command() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "observe",
        "--output",
        &temp.path().display().to_string(),
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);

    assert!(result.is_err());
    let error = match result {
        Ok(()) => return Err(miette::miette!("expected error but observe succeeded")),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("requires a harness command"),
        "error should mention missing harness: {error}"
    );
    Ok(())
}

#[test]
fn observe_records_events_and_shows_non_protective_indicator() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let output_dir = temp.path().join("observe-out");
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "observe",
        "--output",
        &output_dir.display().to_string(),
        "--",
        "true",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    assert!(
        rendered.contains("observation mode: non-protective"),
        "output must show non-protective indicator: {rendered}"
    );
    assert!(
        rendered.contains("no enforcement is claimed or implied"),
        "output must state no enforcement: {rendered}"
    );
    assert!(
        rendered.contains("shadow:"),
        "output must show shadow policy decisions: {rendered}"
    );
    assert!(
        rendered.contains("events recorded:"),
        "output must show event count: {rendered}"
    );

    let events_path = output_dir.join("events.jsonl");
    assert!(events_path.exists(), "events.jsonl must be created");
    let events_content = fs::read_to_string(&events_path).into_diagnostic()?;
    assert!(
        events_content.contains("session_lifecycle"),
        "event stream must contain session_lifecycle events"
    );
    assert!(
        events_content.contains("policy_decision"),
        "event stream must contain policy_decision events"
    );
    assert!(
        events_content.contains("process_execution"),
        "event stream must contain process_execution events"
    );
    assert!(
        events_content.contains("\"shadow\":true"),
        "event stream must mark shadow decisions"
    );
    assert!(
        events_content.contains("\"enforcement\":false"),
        "event stream must mark enforcement as false"
    );
    Ok(())
}

#[test]
fn observe_json_output_is_machine_readable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let output_dir = temp.path().join("observe-json");
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "observe",
        "--output",
        &output_dir.display().to_string(),
        "--json",
        "--",
        "true",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    for line in rendered.lines() {
        let parsed: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            miette::miette!("JSON output line is not valid JSON: {e}\nline: {line}")
        })?;
        assert!(
            parsed.is_object(),
            "each JSON line must be an object: {line}"
        );
    }

    let first: serde_json::Value = serde_json::from_str(rendered.lines().next().unwrap_or("null"))
        .map_err(|e| miette::miette!("first line is not valid JSON: {e}"))?;
    assert_eq!(first["mode"], "observe");
    assert_eq!(first["protective"], false);
    assert_eq!(first["indicator"], "observation mode: non-protective");

    let last: serde_json::Value =
        serde_json::from_str(rendered.lines().last().unwrap_or("null"))
            .map_err(|e| miette::miette!("last line is not valid JSON: {e}"))?;
    assert!(last["summary"].is_object(), "last line must be a summary");
    assert!(
        last["summary"]["events_recorded"].as_u64().unwrap_or(0) > 0,
        "summary must show events recorded"
    );
    assert!(
        last["summary"]["export_path"]
            .as_str()
            .unwrap_or("")
            .ends_with("events.jsonl"),
        "summary must show export path"
    );
    Ok(())
}

#[test]
fn observe_records_harness_exit_code() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let output_dir = temp.path().join("observe-exit");
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "observe",
        "--output",
        &output_dir.display().to_string(),
        "--",
        "sh",
        "-c",
        "exit 42",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    assert!(
        rendered.contains("harness exited: code=42"),
        "output must show exit code 42: {rendered}"
    );

    let events_content = fs::read_to_string(output_dir.join("events.jsonl")).into_diagnostic()?;
    assert!(
        events_content.contains("\"exit_code\":42"),
        "event stream must record exit_code 42"
    );
    Ok(())
}

#[test]
fn observe_handles_nonexistent_harness_gracefully() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let output_dir = temp.path().join("observe-missing");
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "observe",
        "--output",
        &output_dir.display().to_string(),
        "--",
        "this-command-does-not-exist-12345",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    assert!(
        rendered.contains("signal or spawn failure"),
        "output must show spawn failure: {rendered}"
    );
    assert!(
        rendered.contains("events recorded:"),
        "output must still show event count: {rendered}"
    );

    let events_content = fs::read_to_string(output_dir.join("events.jsonl")).into_diagnostic()?;
    assert!(
        events_content.contains("failed to spawn harness"),
        "event stream must record the spawn error"
    );
    Ok(())
}

#[test]
fn observe_shadow_decisions_include_all_outcome_variants() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let output_dir = temp.path().join("observe-outcomes");
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "observe",
        "--output",
        &output_dir.display().to_string(),
        "--json",
        "--",
        "true",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    let shadow_lines: Vec<&str> = rendered
        .lines()
        .filter(|line| line.contains("shadow_decision"))
        .collect();
    assert!(!shadow_lines.is_empty(), "must have shadow decision lines");

    let operation_kinds: Vec<String> = shadow_lines
        .iter()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            value["operation_kind"].as_str().map(String::from)
        })
        .collect();
    assert!(
        operation_kinds.contains(&"filesystem_mutation".to_string()),
        "must include filesystem_mutation shadow decision"
    );
    assert!(
        operation_kinds.contains(&"process_execution".to_string()),
        "must include process_execution shadow decision"
    );
    assert!(
        operation_kinds.contains(&"network_request".to_string()),
        "must include network_request shadow decision"
    );
    assert!(
        operation_kinds.contains(&"mcp_tool_call".to_string()),
        "must include mcp_tool_call shadow decision"
    );

    for line in &shadow_lines {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| miette::miette!("shadow line is not valid JSON: {e}\nline: {line}"))?;
        let outcome = value["outcome"]
            .as_str()
            .ok_or_else(|| miette::miette!("outcome must be a string: {line}"))?;
        assert!(
            matches!(
                outcome,
                "pass"
                    | "pass_with_constraints"
                    | "prompt"
                    | "block"
                    | "unsupported"
                    | "defer_to_stronger_sandbox"
            ),
            "outcome must be a valid spec variant: {outcome}"
        );
        assert!(
            value["trace"].is_string(),
            "shadow decision must include a trace"
        );
    }
    Ok(())
}

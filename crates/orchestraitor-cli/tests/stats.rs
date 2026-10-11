//! End-to-end behavior for `orc stats efficiency` (spec §13.5.1).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;

use clap::Parser;
use miette::IntoDiagnostic;
use orchestraitor_cli::cli::Cli;
use orchestraitor_cost_ledger::{CostEntry, CostLedger, MonetaryCostBasis, ReceiptRecord};
use orchestraitor_model::{AgentId, ModelId, ProviderId, RepositoryId, SessionId};

/// Seeds a ledger with one session that has cost entries plus a receipt,
/// and one session with cost entries only.
fn seed_ledger(path: &std::path::Path) {
    let ledger = CostLedger::open(path).unwrap();
    let entry = |session: &str, request_id: &str, input: u64| CostEntry {
        model: ModelId::from_string(String::from("glm-5.2")),
        provider: ProviderId::from_string(String::from("neuralwatt")),
        agent_domain_id: AgentId::from_string(String::from("backend")),
        role: String::from("implement"),
        project: String::from("project-a"),
        session: SessionId::from_string(String::from(session)),
        repository: RepositoryId::from_string(String::from("repo-a")),
        input_tokens: input,
        output_tokens: 1,
        reasoning_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: String::from(request_id),
        parent_request_id: None,
        started_at: chrono::Utc::now(),
        completed_at: chrono::Utc::now(),
        wall_ms: 1,
        monetary_cost_measured: None,
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::UtilizationOnly,
        subscription_attribution_id: None,
        routing_decision: String::from("bootstrap-default"),
        profile: None,
    };
    ledger
        .api_spend()
        .insert_cost_entry(&entry("sess-measured", "req-1", 1_000))
        .unwrap();
    ledger
        .insert_context_receipt(&ReceiptRecord {
            request_id: String::from("ctx-1"),
            session: SessionId::from_string(String::from("sess-measured")),
            task_class: String::from("backend"),
            budget_tokens: 100_000,
            candidate_tokens: 100_000,
            selected_tokens: 60_000,
            omitted_count: 5,
            raw_tool_output_tokens: 0,
            compacted_tool_output_tokens: 0,
            repeated_tokens_avoided: 0,
            prompt_cache_eligible_tokens: 0,
        })
        .unwrap();
    ledger
        .api_spend()
        .insert_cost_entry(&entry("sess-unmeasured", "req-2", 2_000))
        .unwrap();
}

/// Runs `orc stats efficiency` (markdown output) against `config_dir`.
fn run_efficiency(config_dir: &std::path::Path) -> miette::Result<String> {
    let mut output = Vec::new();
    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "stats",
        "efficiency",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    String::from_utf8(output).into_diagnostic()
}

#[test]
fn stats_efficiency_json_reports_savings_and_unmeasured() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    seed_ledger(&config_dir.join("cost.db"));
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "stats",
        "efficiency",
        "--json",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    let document: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;
    let rollups = document
        .as_array()
        .unwrap_or_else(|| panic!("stats efficiency --json must emit an array: {document}"));
    let measured = rollups
        .iter()
        .find(|rollup| rollup["session"] == "sess-measured")
        .unwrap_or_else(|| panic!("measured session must be reported"));
    assert_eq!(measured["input_tokens"], 1_000);
    assert_eq!(measured["candidate_tokens"], 100_000);
    assert_eq!(measured["selected_tokens"], 60_000);
    let savings = measured["savings_ratio"]
        .as_f64()
        .unwrap_or_else(|| panic!("savings must be measured when a receipt exists"));
    assert!((savings - 0.4).abs() < 1e-12, "1 - 60_000/100_000 = 0.4");
    // The median serializes as null under session grouping (it is a
    // profile-grouping-only stat: one session needs no median).
    assert!(
        measured["median_savings_ratio"].is_null(),
        "session grouping carries no median: {}",
        measured["median_savings_ratio"]
    );

    let unmeasured = rollups
        .iter()
        .find(|rollup| rollup["session"] == "sess-unmeasured")
        .unwrap_or_else(|| panic!("unmeasured session must still report cost"));
    assert_eq!(unmeasured["input_tokens"], 2_000);
    assert!(
        unmeasured["savings_ratio"].is_null(),
        "no receipt → no fabricated savings number"
    );
    Ok(())
}

#[test]
fn stats_efficiency_markdown_uses_dash_for_unmeasured() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    seed_ledger(&config_dir.join("cost.db"));
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "stats",
        "efficiency",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    let text = String::from_utf8(output).into_diagnostic()?;
    assert!(
        text.contains("| sess-measured |"),
        "per-session rows: {text}"
    );
    assert!(
        text.contains("40.0%"),
        "measured savings render as a percentage: {text}"
    );
    assert!(
        text.contains("| sess-unmeasured |"),
        "unmeasured session still appears: {text}"
    );
    Ok(())
}

#[test]
fn stats_efficiency_group_by_profile_is_json_parseable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    seed_ledger(&config_dir.join("cost.db"));
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "stats",
        "efficiency",
        "--group-by-profile",
        "--json",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    let document: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;
    let rollups = document
        .as_array()
        .unwrap_or_else(|| panic!("stats efficiency --json must emit an array: {document}"));
    // Both seeded sessions carry profile = NULL: they group into ONE
    // unprofiled rollup.
    assert_eq!(rollups.len(), 1, "one unprofiled group: {document}");
    assert!(rollups[0]["session"].is_null());
    assert!(rollups[0]["profile"].is_null());
    assert_eq!(rollups[0]["input_tokens"], 3_000);
    Ok(())
}

#[test]
fn stats_efficiency_on_missing_ledger_fails_typed() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "stats",
        "efficiency",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);

    // A missing ledger is a typed error, never a silently created empty
    // one: a report command must not write to disk or pretend "no data".
    let error = result.unwrap_err();
    let message = format!("{error:?}");
    assert!(
        message.contains("no cost ledger"),
        "error must name the missing ledger: {message}"
    );
    assert!(
        !config_dir.join("cost.db").exists(),
        "reporting must not create the ledger file"
    );
    Ok(())
}

#[test]
fn stats_efficiency_with_only_unmeasured_sessions_renders_dashes() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    // A ledger that EXISTS but has no receipt-backed sessions: the
    // markdown table renders with dashes, not a fabricated 0%.
    let ledger = CostLedger::open(&config_dir.join("cost.db")).unwrap();
    let entry = |session: &str, request_id: &str| CostEntry {
        model: ModelId::from_string(String::from("glm-5.2")),
        provider: ProviderId::from_string(String::from("neuralwatt")),
        agent_domain_id: AgentId::from_string(String::from("backend")),
        role: String::from("implement"),
        project: String::from("project-a"),
        session: SessionId::from_string(String::from(session)),
        repository: RepositoryId::from_string(String::from("repo-a")),
        input_tokens: 500,
        output_tokens: 1,
        reasoning_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: String::from(request_id),
        parent_request_id: None,
        started_at: chrono::Utc::now(),
        completed_at: chrono::Utc::now(),
        wall_ms: 1,
        monetary_cost_measured: None,
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::UtilizationOnly,
        subscription_attribution_id: None,
        routing_decision: String::from("bootstrap-default"),
        profile: None,
    };
    ledger
        .api_spend()
        .insert_cost_entry(&entry("sess-x", "req-x"))
        .unwrap();
    drop(ledger);
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "stats",
        "efficiency",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    let text = String::from_utf8(output).into_diagnostic()?;
    assert!(text.contains("| sess-x |"), "session row renders: {text}");
    assert!(text.contains("| — | — | — |"), "unmeasured savings dashes");
    Ok(())
}

/// `orc stats efficiency` never writes: on a legacy ledger that predates
/// the reporting schema, the read-only open fails with a typed
/// migration-required error and the FILE IS UNCHANGED (forbidden effect
/// asserted absent: an in-place migration by a reporting command — the
/// exact defect the PR #565 review found, where `CostLedger::open` ran
/// `BEGIN IMMEDIATE` + `CREATE`/`ALTER` under a reporting read).
#[test]
fn stats_efficiency_on_legacy_ledger_fails_typed_without_writing() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    let ledger_path = config_dir.join("cost.db");

    // Build a LEGACY ledger: the pre-profile schema (no `profile`
    // column, no `context_receipts` table).
    let legacy_sql = "
        CREATE TABLE cost_entries (
            id INTEGER PRIMARY KEY,
            session TEXT NOT NULL,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            cache_read_tokens INTEGER NOT NULL,
            cache_write_tokens INTEGER NOT NULL,
            request_count INTEGER NOT NULL
        );
        INSERT INTO cost_entries (session, input_tokens, output_tokens,
            cache_read_tokens, cache_write_tokens, request_count)
        VALUES ('legacy-session', 100, 10, 0, 0, 1);
    ";
    {
        let conn = rusqlite::Connection::open(&ledger_path).into_diagnostic()?;
        conn.execute_batch(legacy_sql).into_diagnostic()?;
    }
    let before = fs::read(&ledger_path).into_diagnostic()?;

    let result = run_efficiency(&config_dir);
    let error = format!("{}", result.unwrap_err());
    assert!(
        error.contains("needs migration"),
        "the error must name the migration requirement, got: {error}"
    );

    // Forbidden effect asserted absent: the legacy ledger was NOT
    // migrated (no schema writes, no row changes) by the reporting read.
    let after = fs::read(&ledger_path).into_diagnostic()?;
    assert_eq!(
        before, after,
        "a reporting command must never write to the ledger"
    );
    Ok(())
}

/// The read-only open takes no write lock: the report succeeds while a
/// writer holds a `BEGIN IMMEDIATE` transaction on the same database
/// (the contention failure the PR #565 review identified — a migrating
/// open could hit `SQLITE_BUSY` and fail the report).
#[test]
fn stats_efficiency_reads_while_a_writer_holds_the_write_lock() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    seed_ledger(&config_dir.join("cost.db"));

    // A writer holding an open IMMEDIATE transaction (not committed).
    let writer = rusqlite::Connection::open(config_dir.join("cost.db")).into_diagnostic()?;
    writer.execute_batch("BEGIN IMMEDIATE;").into_diagnostic()?;

    // The report must NOT contend: read-only readers never block on a
    // WAL/rollback writer's reserved lock for schema-complete databases.
    let result = run_efficiency(&config_dir);
    writer.execute_batch("ROLLBACK;").into_diagnostic()?;
    let output = result?;
    assert!(
        output.contains("sess-measured"),
        "the report read the ledger while a writer held the write lock"
    );
    Ok(())
}

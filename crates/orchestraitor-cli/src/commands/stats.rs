//! `orc stats` — token-efficiency reporting over the cost ledger (spec
//! §13.5.1).

use std::io::Write;

use miette::{IntoDiagnostic, Result};
use orchestraitor_cost_ledger::{CostLedger, EfficiencyGrouping, TokenEfficiencyRollup};

use crate::cli::{ConfigPaths, StatsCommand, StatsEfficiencyArgs};

/// Implements the `orc stats` subcommands.
///
/// # Errors
///
/// Returns a diagnostic when the cost ledger cannot be opened or queried.
pub fn run(paths: &ConfigPaths, command: StatsCommand, writer: &mut dyn Write) -> Result<()> {
    match command {
        StatsCommand::Efficiency(args) => efficiency(paths, &args, writer),
    }
}

/// `orc stats efficiency`: opens the EXISTING ledger and prints
/// per-session or per-profile rollups. A missing ledger file is a typed
/// error, never a silently created empty one — a reporting command must
/// not write to disk (schema creation / migration) or fabricate an empty
/// report for a mistyped config dir.
fn efficiency(
    paths: &ConfigPaths,
    args: &StatsEfficiencyArgs,
    writer: &mut dyn Write,
) -> Result<()> {
    let ledger_path = paths.config_dir.join("cost.db");
    if !ledger_path.is_file() {
        return Err(miette::miette!(
            "no cost ledger at {} — run `orc loop` first (per-call cost tracking creates it)",
            ledger_path.display()
        ));
    }
    let ledger = CostLedger::open(&ledger_path).into_diagnostic()?;
    let grouping = if args.group_by_profile {
        EfficiencyGrouping::Profile
    } else {
        EfficiencyGrouping::Session
    };
    let rollups = ledger
        .token_efficiency_rollups(grouping)
        .into_diagnostic()?;
    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &rollups).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
    } else {
        render_markdown(writer, &rollups).map_err(|error| miette::miette!(error))?;
    }
    Ok(())
}

/// Renders the rollups as a markdown table. Savings shows `—` when no
/// receipt exists for the group (spec §13.5.1: not measured is not zero);
/// under profile grouping the median column carries the spec-required
/// median of per-session ratios (the group-sum `savings` weights
/// sessions by their baseline).
///
/// # Errors
///
/// Returns a diagnostic when writing to the output writer fails (a
/// broken pipe must not report success for an incomplete report).
fn render_markdown(
    writer: &mut dyn Write,
    rollups: &[TokenEfficiencyRollup],
) -> std::result::Result<(), std::io::Error> {
    writeln!(
        writer,
        "| group | input | output | cached (read) | candidate | selected | savings | median savings |"
    )?;
    writeln!(
        writer,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )?;
    for rollup in rollups {
        writeln!(
            writer,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            group_label(rollup),
            rollup.input_tokens,
            rollup.output_tokens,
            rollup.cache_read_tokens,
            rollup
                .candidate_tokens
                .map_or_else(|| "—".to_owned(), |tokens| tokens.to_string()),
            rollup
                .selected_tokens
                .map_or_else(|| "—".to_owned(), |tokens| tokens.to_string()),
            rollup
                .savings_ratio
                .map_or_else(|| "—".to_owned(), |ratio| format!("{:.1}%", ratio * 100.0),),
            rollup
                .median_session_savings_ratio()
                .map_or_else(|| "—".to_owned(), |ratio| format!("{:.1}%", ratio * 100.0),),
        )?;
    }
    Ok(())
}

/// The grouping key's display label.
fn group_label(rollup: &TokenEfficiencyRollup) -> String {
    if let Some(session) = &rollup.session {
        return session.clone();
    }
    if let Some(agent) = &rollup.agent_domain_id {
        return agent.clone();
    }
    rollup
        .profile
        .clone()
        .unwrap_or_else(|| "(unprofiled)".to_owned())
}

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

/// `orc stats efficiency`: opens the ledger read-only-shaped (an open
/// failure is a typed error — reporting must never silently fabricate an
/// empty report) and prints per-session or per-profile rollups.
fn efficiency(
    paths: &ConfigPaths,
    args: &StatsEfficiencyArgs,
    writer: &mut dyn Write,
) -> Result<()> {
    let ledger = CostLedger::open(&paths.config_dir.join("cost.db")).into_diagnostic()?;
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
        render_markdown(writer, &rollups);
    }
    Ok(())
}

/// Renders the rollups as a markdown table. Savings shows `—` when no
/// receipt exists for the group (spec §13.5.1: not measured is not zero).
fn render_markdown(writer: &mut dyn Write, rollups: &[TokenEfficiencyRollup]) {
    if rollups.is_empty() {
        let _ignore = writeln!(writer, "no cost entries recorded");
        return;
    }
    let _ignore = writeln!(
        writer,
        "| group | input | output | cached (read) | candidate | selected | savings |"
    );
    let _ignore = writeln!(writer, "| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for rollup in rollups {
        let _ignore = writeln!(
            writer,
            "| {} | {} | {} | {} | {} | {} | {} |",
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
        );
    }
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

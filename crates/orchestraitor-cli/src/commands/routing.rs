//! `orc routing` implementation.

use std::io::Write;

use miette::{IntoDiagnostic, Result, miette};
use orchestraitor_agent_catalog::{RoleRouter, RoleRoutingDecisionStore};
use serde::Serialize;

use crate::cli::{ConfigPaths, ResolveArgs, RoutingCommand};
use crate::commands::config::layers::load_layers;

/// Runs an `orc routing` subcommand.
///
/// # Errors
/// Returns a diagnostic when resolution, persistence, or output fails.
pub fn run<W: Write>(paths: &ConfigPaths, command: RoutingCommand, writer: &mut W) -> Result<()> {
    match command {
        RoutingCommand::Resolve(args) => resolve(paths, &args, writer),
    }
}

#[derive(Debug, Serialize)]
struct ResolveOutput<'a> {
    record_id: i64,
    role: &'a str,
    provider: &'a str,
    model: &'a str,
    precedence_path: &'a str,
    fallback_reason: Option<&'a str>,
    persisted_at: &'a str,
}

fn resolve<W: Write>(paths: &ConfigPaths, args: &ResolveArgs, writer: &mut W) -> Result<()> {
    let layers = load_layers(paths)?;
    let router = RoleRouter::new(&layers.resolver);
    let decision = router
        .resolve(&args.role)
        .map_err(|error| miette!("{error}"))?;
    let store_path = paths.config_dir.join("routing.db");
    let store = RoleRoutingDecisionStore::open(&store_path)
        .map_err(|error| miette!("cannot open routing decision store: {error}"))?;
    let stored = store
        .record(&decision)
        .map_err(|error| miette!("cannot persist routing decision record: {error}"))?;
    if args.json {
        let output = ResolveOutput {
            record_id: stored.id,
            role: &stored.role,
            provider: &stored.provider,
            model: &stored.model,
            precedence_path: &stored.precedence_path,
            fallback_reason: stored.fallback_reason.as_deref(),
            persisted_at: &stored.created_at,
        };
        serde_json::to_writer_pretty(&mut *writer, &output).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
        return Ok(());
    }
    writeln!(writer, "role = {}", stored.role).into_diagnostic()?;
    writeln!(writer, "provider = {}", stored.provider).into_diagnostic()?;
    writeln!(writer, "model = {}", stored.model).into_diagnostic()?;
    writeln!(writer, "precedence_path = {}", stored.precedence_path).into_diagnostic()?;
    writeln!(
        writer,
        "fallback_reason = {}",
        stored.fallback_reason.as_deref().unwrap_or("none")
    )
    .into_diagnostic()?;
    writeln!(writer, "record_id = {}", stored.id).into_diagnostic()
}

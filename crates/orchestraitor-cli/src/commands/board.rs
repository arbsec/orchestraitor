//! `orc board` implementation (spec §9.43 bootstrap slice).

use std::io::Write;
use std::sync::Arc;

use miette::{IntoDiagnostic, Result, miette};
use orchestraitor_board::{BoardClient, BoardProjectConfig, SecretUriAuth, SkipWarning};

use crate::cli::{BoardCommand, BoardMoveArgs, BoardReadyArgs, ConfigPaths};

/// Runs an `orc board` subcommand.
///
/// # Errors
/// Returns a diagnostic when board config, auth, GraphQL, or output fails.
pub fn run<W: Write>(paths: &ConfigPaths, command: BoardCommand, writer: &mut W) -> Result<()> {
    let (config, _path) = BoardProjectConfig::load(&paths.project_dir).into_diagnostic()?;
    let token_uri = config
        .token_uri
        .clone()
        .ok_or(orchestraitor_board::BoardError::AuthNotConfigured)
        .into_diagnostic()?;
    let auth = Arc::new(SecretUriAuth::new(token_uri));
    let client = match &paths.github_graphql_endpoint {
        Some(endpoint) => BoardClient::with_endpoint(endpoint.clone(), auth),
        None => BoardClient::new(auth),
    }
    .into_diagnostic()?;
    // `None` keeps the XDG default the crate computes; an explicit path
    // overrides it so tests never touch the host cache.
    let client = match &paths.board_cache_path {
        Some(path) => client.with_cache_path(Some(path.clone())),
        None => client,
    };
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    match command {
        BoardCommand::Ready(args) => ready(&runtime, &client, &config, args, writer),
        BoardCommand::Move(args) => move_item(&runtime, &client, &config, &args, writer),
    }
}

fn ready<W: Write>(
    runtime: &tokio::runtime::Runtime,
    client: &BoardClient,
    config: &BoardProjectConfig,
    args: BoardReadyArgs,
    writer: &mut W,
) -> Result<()> {
    let (items, warnings) = runtime
        .block_on(client.ready_items(config))
        .into_diagnostic()?;
    report_warnings(&warnings)?;
    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &items).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
        return Ok(());
    }
    writeln!(writer, "Ready queue ({} eligible issue(s)):", items.len()).into_diagnostic()?;
    for item in items {
        writeln!(writer, "  #{}: {}", item.number, item.title).into_diagnostic()?;
    }
    Ok(())
}

fn move_item<W: Write>(
    runtime: &tokio::runtime::Runtime,
    client: &BoardClient,
    config: &BoardProjectConfig,
    args: &BoardMoveArgs,
    writer: &mut W,
) -> Result<()> {
    if args.status.trim().is_empty() {
        return Err(miette!("--status must be a non-empty Status option name"));
    }
    let outcome = runtime
        .block_on(client.move_item(config, args.item, args.status.trim()))
        .into_diagnostic()?;
    writeln!(
        writer,
        "moved #{} to \"{}\" (verified by read-back)",
        outcome.number, outcome.status
    )
    .into_diagnostic()
}

/// Warnings go to stderr so `--json` stdout stays machine-readable.
fn report_warnings(warnings: &[SkipWarning]) -> Result<()> {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    for warning in warnings {
        match warning.number {
            Some(number) => writeln!(lock, "warning: skipped item #{number}: {}", warning.reason),
            None => writeln!(lock, "warning: skipped one item: {}", warning.reason),
        }
        .into_diagnostic()?;
    }
    Ok(())
}

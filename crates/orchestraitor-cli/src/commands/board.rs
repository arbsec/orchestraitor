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
    // The board.query decision tool reads the deterministic fixture board in
    // this slice (spec §9.39, issue #332): no board auth, no network — the
    // sqlite provider is a #318 follow-up and the GitHub provider lands
    // separately. The fixture keeps the tool surface, typed results, and
    // event recording testable end to end today.
    if let BoardCommand::Query(args) = command {
        return query(&args, writer);
    }
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
        // Typed early return above keeps this arm exhaustive only.
        BoardCommand::Query(_) => Ok(()),
    }
}

/// Builds the shared `board.query` fixture board (same shape as the crate
/// tests): a small typed board with a blocked chain, a cycle branch, and
/// filterable items.
fn fixture_board() -> orchestraitor_board_contract::InMemoryBoardProvider {
    use orchestraitor_board_contract::{BoardFieldKind, BoardFieldValue, BoardItemType};
    orchestraitor_board_contract::InMemoryBoardProvider::new(|setup| {
        setup
            .status("Ready")
            .status("In Progress")
            .status("Blocked")
            .status("Done")
            .field("Priority", BoardFieldKind::SingleSelect)
            .field("Target", BoardFieldKind::SingleSelect)
            .field("Points", BoardFieldKind::Number)
            .item(
                "root",
                BoardItemType::Task,
                "Root task",
                "body",
                "Blocked",
                &[(
                    "Priority",
                    BoardFieldValue::SingleSelect {
                        option: "P0".into(),
                    },
                )],
            )
            .item(
                "deep-1",
                BoardItemType::Task,
                "Deep blocker one",
                "body",
                "In Progress",
                &[(
                    "Priority",
                    BoardFieldValue::SingleSelect {
                        option: "P1".into(),
                    },
                )],
            )
            .item(
                "deep-2",
                BoardItemType::Task,
                "Deep blocker two",
                "body",
                "In Progress",
                &[],
            )
            .item(
                "deep-3",
                BoardItemType::Task,
                "Deep blocker three",
                "body",
                "In Progress",
                &[],
            )
            .edge("root", "deep-1")
            .edge("deep-1", "deep-2")
            .edge("deep-2", "deep-3")
            .item(
                "cyc-a",
                BoardItemType::Bug,
                "Cycle A",
                "body",
                "In Progress",
                &[],
            )
            .item(
                "cyc-b",
                BoardItemType::Bug,
                "Cycle B",
                "body",
                "In Progress",
                &[],
            )
            .edge("cyc-a", "cyc-b")
            .edge("cyc-b", "cyc-a")
            .item(
                "ready-epic",
                BoardItemType::Epic,
                "Ready epic",
                "body",
                "Ready",
                &[(
                    "Target",
                    BoardFieldValue::SingleSelect {
                        option: "MVP".into(),
                    },
                )],
            )
            .item(
                "done-task",
                BoardItemType::Task,
                "Done task",
                "body",
                "Done",
                &[("Points", BoardFieldValue::Number { value: 3 })],
            );
    })
}

/// Parses CLI filter arguments into the typed query mode.
fn query_mode(
    args: &crate::cli::BoardQueryArgs,
) -> miette::Result<orchestraitor_mcp::board_query::BoardQueryMode> {
    use orchestraitor_mcp::board_query::{
        BoardQueryField, BoardQueryFieldValue, BoardQueryFilter, BoardQueryItemType, BoardQueryMode,
    };
    if let Some(item) = &args.blocked_by {
        return Ok(BoardQueryMode::BlockedBy { item: item.clone() });
    }
    let item_type = match args.item_type.as_deref() {
        None => None,
        Some("task") => Some(BoardQueryItemType::Task),
        Some("bug") => Some(BoardQueryItemType::Bug),
        Some("epic") => Some(BoardQueryItemType::Epic),
        Some("feature") => Some(BoardQueryItemType::Feature),
        Some(other) => {
            return Err(miette!(
                "--item-type must be one of task|bug|epic|feature, got {other:?}"
            ));
        }
    };
    let mut fields = Vec::new();
    for field in &args.fields {
        let (name, value) = field.split_once('=').ok_or_else(|| {
            miette!("--field must be name=value (single-select option), got {field:?}")
        })?;
        fields.push(BoardQueryField {
            name: name.to_string(),
            value: BoardQueryFieldValue::SingleSelect {
                option: value.to_string(),
            },
        });
    }
    Ok(BoardQueryMode::Search {
        filter: BoardQueryFilter {
            item_type,
            status: args.status.clone(),
            fields,
        },
    })
}

/// Renders one typed result item line.
fn write_item_line<W: Write>(
    writer: &mut W,
    item: &orchestraitor_mcp::board_query::BoardQueryItem,
) -> Result<()> {
    writeln!(
        writer,
        "  [{}] {}: {} (status: {})",
        item.item_type, item.id, item.title, item.status
    )
    .into_diagnostic()
}

fn query<W: Write>(args: &crate::cli::BoardQueryArgs, writer: &mut W) -> Result<()> {
    use orchestraitor_events::InMemoryAuditStore;
    use orchestraitor_mcp::board_query::{BoardQueryResultKind, DelegationChain, board_query};
    use orchestraitor_model::OperationId;

    let mode = query_mode(args)?;
    let board = fixture_board();
    let chain = DelegationChain {
        correlation_id: OperationId::new(),
        parent_op_id: None,
        principals: vec![
            String::from("user:cli"),
            String::from("session:orc-board-query"),
        ],
    };
    // §9.25.1 recording: one-shot CLI invocation — the event is written,
    // hash-chain-validated, and summarized here; the in-memory store lives
    // only for this process (per-invocation-volatile). Durable persistence
    // lands with the daemon event-store wiring (§9.17), not in this slice.
    let mut store = InMemoryAuditStore::default();
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    let result = runtime
        .block_on(board_query(&board, &mode, &chain, &mut store))
        .map_err(|error| miette!("{error}"))?;
    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &result).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
        return Ok(());
    }
    match result {
        BoardQueryResultKind::Search(result) => {
            writeln!(
                writer,
                "Matched {} item(s){}:",
                result.items.len(),
                if result.truncated { " (truncated)" } else { "" }
            )
            .into_diagnostic()?;
            for item in &result.items {
                write_item_line(writer, item)?;
                if !item.blocked_by.is_empty() {
                    writeln!(writer, "      blockedBy: {}", item.blocked_by.join(", "))
                        .into_diagnostic()?;
                }
            }
        }
        BoardQueryResultKind::BlockedBy(graph) => {
            if !graph.root_exists {
                writeln!(writer, "Item {} not found on the board.", graph.root)
                    .into_diagnostic()?;
                return Ok(());
            }
            writeln!(
                writer,
                "Transitive blocked set of {} ({} item(s)):",
                graph.root,
                graph.blocking.len()
            )
            .into_diagnostic()?;
            for item in &graph.blocking {
                write_item_line(writer, item)?;
            }
            if let Some(cycle) = &graph.cycle {
                writeln!(
                    writer,
                    "CYCLE (board-data corruption, spec §9.40): {} revisited via {}",
                    cycle.item,
                    cycle.path.join(" -> ")
                )
                .into_diagnostic()?;
            }
            if graph.depth_capped {
                writeln!(
                    writer,
                    "DEPTH CAP: walk stopped at the maximum depth; result may be incomplete."
                )
                .into_diagnostic()?;
            }
        }
    }
    Ok(())
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

//! `orc chat` — the operator's conversational surface (spec
//! `10-orchestrator.md` §9.44).
//!
//! Progress reads are READ-ONLY views over durable state, stamped with the
//! §9.44 last-synced caveat: an answer about state names its snapshot
//! instant, never a live feed, and carries no execution authority. The
//! surface shares the §9.48 aggregation engine with `orc pr status` (one
//! engine, three surfaces) — same typed output, same reason codes, never a
//! second interpretation.

use std::io::Write;

use miette::{IntoDiagnostic, Result};

use crate::cli::{ChatCommand, ConfigPaths};
use crate::commands::pr_status::{build_engine, repo_root, typed_report};

/// Runs an `orc chat` subcommand.
///
/// # Errors
/// Returns the shared engine's typed errors — `PrStatusError::Unrecoverable`
/// (exit 1), [`PrStatusError::Config`] (exit 2), and
/// [`PrStatusError::ConcurrentEdit`] (exit 4) map to the §9.48 exit-code
/// contract exactly like `orc pr status`.
pub fn run<W: Write>(paths: &ConfigPaths, command: ChatCommand, writer: &mut W) -> Result<()> {
    match command {
        ChatCommand::PrStatus(args) => pr_status_view(paths, &args, writer),
    }
}

/// Renders the §9.44-stamped read-only `pr.status` progress view.
///
/// The view is a FRESH aggregation on every invocation (a progress read
/// reads from durable state at the moment of the answer — there is no chat
/// memory and no cache to go stale). The §9.44 caveat is the LAST-SYNCED
/// stamp: the rendered verdict names its snapshot instant and states that
/// answers describe that instant, never a live feed.
fn pr_status_view<W: Write>(
    paths: &ConfigPaths,
    args: &crate::cli::ChatPrStatusArgs,
    writer: &mut W,
) -> Result<()> {
    let outcome = super::pr_status::resolve_repo(args.repo.as_deref()).and_then(|(owner, repo)| {
        repo_root().and_then(|repo_root| {
            build_engine(paths, repo_root).and_then(|engine| {
                engine
                    .aggregate(&owner, &repo, args.number)
                    .map_err(typed_report)
            })
        })
    });
    match outcome {
        Ok(verdict) => {
            let convergence = &verdict.convergence;
            let headline = if convergence.mergeable {
                "MERGEABLE"
            } else {
                "BLOCKED"
            };
            // §9.44: verdict-first prose, then the reason codes, then the
            // last-synced caveat — the snapshot instant, never a live feed.
            writeln!(
                writer,
                "{headline}: PR #{} ({}) is {}right now (last synced {}).",
                verdict.number,
                verdict.repository,
                if convergence.mergeable {
                    "mergeable "
                } else {
                    "NOT mergeable "
                },
                verdict.fetched_at
            )
            .into_diagnostic()?;
            if convergence.reasons.is_empty() {
                writeln!(writer, "reasons: (none — all gates pass)").into_diagnostic()?;
            } else {
                writeln!(writer, "reasons: {}", convergence.reasons.join(", "))
                    .into_diagnostic()?;
            }
            if let Some(checks) = &verdict.checks {
                for check in checks {
                    if matches!(check.classification.as_str(), "fail" | "pending") {
                        writeln!(
                            writer,
                            "check [{}] {}: {}",
                            check.classification, check.name, check.state
                        )
                        .into_diagnostic()?;
                    }
                }
            }
            if let Some(threads) = &verdict.threads {
                for thread in &threads.items {
                    if thread.actionable {
                        writeln!(
                            writer,
                            "thread {}: {}:{} — {}",
                            thread.id,
                            thread.path.as_deref().unwrap_or("(no path)"),
                            thread
                                .line
                                .map_or_else(|| "-".to_string(), |line| line.to_string()),
                            thread
                                .last_comment_excerpt
                                .as_deref()
                                .unwrap_or("(no comments)")
                        )
                        .into_diagnostic()?;
                    }
                }
            }
            if let Some(checklist) = &verdict.checklist
                && !checklist.all_checked
            {
                writeln!(
                    writer,
                    "checklist: {} unchecked item(s)",
                    checklist.unchecked_count
                )
                .into_diagnostic()?;
                for item in &checklist.items {
                    if !item.checked {
                        writeln!(writer, "  [ ] {}: {}", item.marker, item.description)
                            .into_diagnostic()?;
                    }
                }
            }
            // The §9.44 caveat, verbatim: this read describes the snapshot
            // instant above. Chat answers are summaries of durable state at
            // their sync time, never a live feed; re-read for the current
            // instant.
            writeln!(
                writer,
                "last-synced caveat: this view is a read-only snapshot taken at {} — it \
                 describes durable state AT that instant, not a live feed; re-run to sync.",
                verdict.fetched_at
            )
            .into_diagnostic()?;
            Ok(())
        }
        // The typed diagnostic surfaces exactly like `orc pr status`: the
        // exit-code mapping in main.rs matches the same wrapper.
        Err(report) => Err(report),
    }
}

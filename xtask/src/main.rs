//! xtask — development automation for Orchestraitor.

mod docs_check;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Orchestraitor development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check documentation invariants: spec compatibility-index integrity,
    /// legacy section-reference resolution, and intra-spec markdown links.
    DocsCheck {
        /// Repository root to validate; defaults to the workspace root that
        /// contains this crate.
        root: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = match cli.command {
        Commands::DocsCheck { root } => root.unwrap_or_else(default_root),
    };
    let report = docs_check::run(&root);
    let printed = print_report(&report);
    if printed.is_ok() && report.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Repository root used when no `root` argument is given: the parent of the
/// `xtask` crate directory baked in at compile time.
fn default_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Print the human-readable report to stdout.
fn print_report(report: &docs_check::Report) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for failure in &report.failures {
        match failure.line {
            Some(line) => writeln!(
                out,
                "{}:{line}: {}",
                failure.path.display(),
                failure.message
            )?,
            None => writeln!(out, "{}: {}", failure.path.display(), failure.message)?,
        }
    }
    let failures = report.failures.len();
    if failures == 0 {
        writeln!(
            out,
            "docs-check: OK ({} index rows, {} references, {} links checked)",
            report.index_rows, report.references_checked, report.links_checked
        )?;
    } else {
        writeln!(
            out,
            "docs-check: FAILED ({failures} failures; {} index rows, {} references, {} links checked)",
            report.index_rows, report.references_checked, report.links_checked
        )?;
    }
    Ok(())
}

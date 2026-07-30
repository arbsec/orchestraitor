//! Typed `orc` command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Orchestraitor command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "orc",
    author,
    version,
    about = "Orchestraitor local control-plane CLI",
    long_about = "Orchestraitor - an agent harness with trust issues.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Command to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level `orc` subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Detect the local project and propose `.orchestraitor/orchestraitor.toml`.
    Init(InitArgs),
}

/// Arguments for `orc init`.
#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Show the proposed configuration without writing any files.
    #[arg(long)]
    pub dry_run: bool,

    /// Project root to inspect.
    #[arg(long, default_value = ".")]
    pub project: PathBuf,
}

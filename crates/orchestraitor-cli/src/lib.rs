//! Command-line entry points for Orchestraitor.

#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
mod detection;
pub mod init;
mod render;
mod scanner;

use std::io::{self, Write};

use clap::Parser;

pub use cli::{Cli, Commands, ObserveArgs};

/// Runs the CLI against process arguments and standard streams.
///
/// # Errors
/// Returns a diagnostic when arguments, configuration, or provider metadata fail.
pub fn run() -> miette::Result<()> {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    run_with_writer(cli, &mut lock)
}

/// Runs a parsed command against a supplied writer.
///
/// # Errors
/// Returns a diagnostic when the selected command fails.
pub fn run_with_writer<W: Write>(cli: Cli, writer: &mut W) -> miette::Result<()> {
    match cli.command {
        Commands::Init(args) => init::run(&args, writer),
        Commands::Config(command) => commands::config::run(&cli.paths, command, writer),
        Commands::Models(command) => commands::models::run(&cli.paths, command, writer),
        Commands::Observe(args) => commands::observe::run(&args, writer),
    }
}

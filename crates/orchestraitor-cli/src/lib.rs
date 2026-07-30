//! Command-line entry points for Orchestraitor.

#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
mod detection;
pub mod exit_code;
pub mod init;
mod render;
mod scanner;

use std::io::{self, Write};

use clap::Parser;

pub use cli::{Cli, Commands};
pub use exit_code::{ExitCode, OrcError, OrcResult};

/// Runs the CLI against process arguments and standard streams.
///
/// # Errors
///
/// Returns an [`OrcError`] carrying a stable [`ExitCode`] when arguments,
/// configuration, or command execution fails.
pub fn run() -> OrcResult<ExitCode> {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    run_with_writer(cli, &mut lock)
}

/// Runs a parsed command against a supplied writer.
///
/// # Errors
///
/// Returns an [`OrcError`] carrying a stable [`ExitCode`] when the selected
/// command fails.
pub fn run_with_writer<W: Write>(cli: Cli, writer: &mut W) -> OrcResult<ExitCode> {
    match cli.command {
        Commands::Init(args) => {
            init::run(&args, writer)?;
            Ok(ExitCode::Success)
        }
        Commands::Config(command) => {
            commands::config::run(&cli.paths, command, writer)?;
            Ok(ExitCode::Success)
        }
        Commands::Models(command) => {
            commands::models::run(&cli.paths, command, writer)?;
            Ok(ExitCode::Success)
        }
        Commands::Verify(args) => commands::verify::run(&cli.paths, &args, writer),
        Commands::Policy(command) => match command {
            cli::PolicyCommand::Check(args) => {
                commands::policy_check::run(&cli.paths, &args, writer)
            }
        },
        Commands::Run(args) => commands::run::run(&cli.paths, &args, writer),
        Commands::Evidence(command) => match command {
            cli::EvidenceCommand::Export(args) => {
                commands::evidence::run(&cli.paths, &args, writer)
            }
        },
    }
}

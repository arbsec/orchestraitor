//! xtask — development automation for Orchestraitor.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Orchestraitor development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check documentation invariants (ADR index, spec refs, conceptual-name guard).
    DocsCheck {
        #[arg(long)]
        check: bool,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::DocsCheck { check: _ } => {
            std::process::exit(1);
        }
    }
}

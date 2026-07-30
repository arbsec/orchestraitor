//! `orc` command-line binary.

use std::io;

use clap::Parser;
use miette::Result;
use orchestraitor_cli::cli::{Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut stdout = io::stdout().lock();
    match cli.command {
        Commands::Init(args) => orchestraitor_cli::init::run(&args, &mut stdout),
    }
}

//! `orc` command-line binary.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use orchestraitor_cli::commands::simplify::PedanticCheckFailed;

fn main() -> miette::Result<ExitCode> {
    match orchestraitor_cli::run() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) if error.is::<PedanticCheckFailed>() => Ok(ExitCode::FAILURE),
        Err(error) => Err(error),
    }
}

//! `orc` command-line binary.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use orchestraitor_cli::commands::pr_status::{
    PrStatusConcurrentEdit, PrStatusConfigError, PrStatusUnrecoverable,
};
use orchestraitor_cli::commands::simplify::PedanticCheckFailed;

fn main() -> miette::Result<ExitCode> {
    match orchestraitor_cli::run() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) if error.is::<PedanticCheckFailed>() => Ok(ExitCode::FAILURE),
        // §9.48 exit-code contract for `orc pr status`: 1 unrecoverable,
        // 2 config/state, 4 concurrent edit. The diagnostic (and any partial
        // typed JSON) is printed by the command itself before the typed
        // error propagates — these arms only map the exit code.
        Err(error) if error.is::<PrStatusUnrecoverable>() => Ok(ExitCode::from(1)),
        Err(error) if error.is::<PrStatusConfigError>() => Ok(ExitCode::from(2)),
        Err(error) if error.is::<PrStatusConcurrentEdit>() => Ok(ExitCode::from(4)),
        Err(error) => Err(error),
    }
}

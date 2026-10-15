//! `orc` command-line binary.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use orchestraitor_cli::commands::pr_status::PrStatusDiagnostic;
use orchestraitor_cli::commands::simplify::PedanticCheckFailed;
use orchestraitor_mcp::pr_status::PrStatusError;

fn main() -> miette::Result<ExitCode> {
    match orchestraitor_cli::run() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) if error.is::<PedanticCheckFailed>() => Ok(ExitCode::FAILURE),
        // §9.48 exit-code contract for `orc pr status`: 1 unrecoverable,
        // 2 config/state, 4 concurrent edit. The diagnostic (and any partial
        // typed JSON) is printed by the command itself before the typed
        // error propagates — these arms only map the exit code. The typed
        // error rides the CLI's `PrStatusDiagnostic` wrapper (the shared
        // mcp crate has no miette dependency; see the wrapper's docs).
        Err(error) if error.is::<PrStatusDiagnostic>() => {
            match error
                .downcast_ref::<PrStatusDiagnostic>()
                .map(PrStatusDiagnostic::error)
            {
                Some(PrStatusError::Unrecoverable { .. }) => Ok(ExitCode::from(1)),
                Some(PrStatusError::Config { .. }) => Ok(ExitCode::from(2)),
                Some(PrStatusError::ConcurrentEdit { .. }) => Ok(ExitCode::from(4)),
                None => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

//! `orcd` daemon binary.

use std::{path::PathBuf, process::ExitCode};

use miette::{IntoDiagnostic, Result};
use orchestraitor_daemon::{DaemonConfig, DaemonError, run_until_signal};

/// Starts the `orcd` JSON-RPC daemon on a Unix-domain socket.
fn main() -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::BuildRuntime)
        .into_diagnostic()?;
    runtime.block_on(async {
        run_until_signal(DaemonConfig::new(socket_path()))
            .await
            .into_diagnostic()?;
        Ok(ExitCode::SUCCESS)
    })
}

fn socket_path() -> PathBuf {
    std::env::args_os()
        .skip(1)
        .find_map(|arg| arg.into_string().ok())
        .or_else(|| std::env::var("ORCHESTRAITOR_DAEMON_SOCKET").ok())
        .map_or_else(default_socket_path, PathBuf::from)
}

fn default_socket_path() -> PathBuf {
    std::env::temp_dir().join("orchestraitor").join("orcd.sock")
}

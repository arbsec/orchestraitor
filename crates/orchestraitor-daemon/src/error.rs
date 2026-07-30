//! Error types for the Orchestraitor daemon transport.

use std::{io, path::PathBuf};

/// Errors returned by the daemon JSON-RPC server.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The daemon could not bind its Unix-domain socket.
    #[error("failed to bind daemon socket at {path}: {source}")]
    BindSocket {
        /// Socket path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },

    /// The daemon could not remove a stale socket path before binding.
    #[error("failed to remove stale daemon socket at {path}: {source}")]
    RemoveStaleSocket {
        /// Socket path that failed cleanup.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },

    /// The daemon accepted a connection but jsonrpsee failed to serve it.
    #[error("failed to serve daemon JSON-RPC connection: {0}")]
    ServeConnection(String),

    /// The daemon failed to accept a Unix-domain socket connection.
    #[error("failed to accept daemon socket connection: {0}")]
    Accept(io::Error),

    /// The daemon could not install a SIGTERM handler.
    #[error("failed to install SIGTERM handler: {0}")]
    InstallSigterm(io::Error),

    /// The daemon failed to register a JSON-RPC method.
    #[error("failed to register JSON-RPC method: {0}")]
    RegisterMethod(String),

    /// The daemon runtime could not start.
    #[error("failed to build daemon runtime: {0}")]
    BuildRuntime(io::Error),
}

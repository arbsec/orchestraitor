//! JSON-RPC method registration for the daemon API.

use jsonrpsee::{RpcModule, types::ErrorObjectOwned};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::DaemonError;

/// Current daemon JSON-RPC protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Request payload accepted by `initialize`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InitializeRequest {
    /// Client protocol version, if known.
    #[serde(default)]
    pub protocol_version: Option<u16>,
}

/// Response returned by `initialize`.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct InitializeResponse {
    /// Negotiated protocol version.
    pub protocol_version: u16,
    /// Stable daemon program name.
    pub server_name: &'static str,
    /// Daemon crate version.
    pub server_version: &'static str,
}

/// Response returned by `health`.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct HealthResponse {
    /// Health state for the daemon process.
    pub status: &'static str,
    /// Whether this daemon crate implements security enforcement itself.
    pub security_authority: &'static str,
}

/// Response returned by `shutdown`.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ShutdownResponse {
    /// Whether shutdown was requested successfully.
    pub accepted: bool,
}

/// Builds the daemon JSON-RPC method table.
///
/// # Errors
///
/// Returns [`DaemonError::RegisterMethod`] if jsonrpsee rejects a method name.
pub fn build_rpc_module(shutdown_tx: watch::Sender<bool>) -> Result<RpcModule<()>, DaemonError> {
    let mut module = RpcModule::new(());

    module
        .register_method("initialize", |params, (), _| {
            let request = params.parse::<InitializeRequest>()?;
            let protocol_version = request.protocol_version.unwrap_or(PROTOCOL_VERSION);
            Ok::<InitializeResponse, ErrorObjectOwned>(InitializeResponse {
                protocol_version: protocol_version.min(PROTOCOL_VERSION),
                server_name: "orcd",
                server_version: env!("CARGO_PKG_VERSION"),
            })
        })
        .map_err(|error| DaemonError::RegisterMethod(error.to_string()))?;

    module
        .register_method("health", |_, (), _| {
            Ok::<HealthResponse, ErrorObjectOwned>(HealthResponse {
                status: "ok",
                security_authority: "arbitraitor",
            })
        })
        .map_err(|error| DaemonError::RegisterMethod(error.to_string()))?;

    module
        .register_method("shutdown", move |_, (), _| {
            let accepted = shutdown_tx.send(true).is_ok();
            Ok::<ShutdownResponse, ErrorObjectOwned>(ShutdownResponse { accepted })
        })
        .map_err(|error| DaemonError::RegisterMethod(error.to_string()))?;

    Ok(module)
}

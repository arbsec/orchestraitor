//! Unix-domain socket JSON-RPC serving for `orcd`.

use std::{future::Future, path::PathBuf, time::Duration};

use jsonrpsee::server::{Server, serve_with_graceful_shutdown};
use orchestraitor_arbitraitor_client::ArbitraitorClient;
use tokio::{
    net::UnixListener,
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinSet,
    time::timeout,
};
use tracing::{debug, info, warn};

use crate::{CapabilityReport, DaemonError, build_rpc_module, probe_capabilities};

/// Maximum time allowed for daemon graceful shutdown.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Runtime configuration for the daemon JSON-RPC server.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Unix-domain socket path used for local IPC.
    pub socket_path: PathBuf,
    /// Grace period used to drain accepted connections during shutdown.
    pub shutdown_timeout: Duration,
}

impl DaemonConfig {
    /// Creates a daemon configuration for a Unix-domain socket path.
    #[must_use]
    pub const fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }
}

/// Runs the daemon until SIGTERM is received or the `shutdown` RPC method is called.
///
/// # Errors
///
/// Returns [`DaemonError`] when socket setup, signal registration, or serving fails.
pub async fn run_until_signal(config: DaemonConfig) -> Result<(), DaemonError> {
    serve_until(config, sigterm()).await
}

/// Serves the daemon until an external shutdown future completes or RPC shutdown is requested.
///
/// At startup, probes Arbitraitor effective controls for the current platform
/// (spec §6.7, §9.6, §16.7). When any required control is unavailable, the
/// daemon logs the missing controls and refuses to start protected services
/// (fail-closed per §6.7) but continues serving so the health RPC can report
/// the degraded posture.
///
/// # Errors
///
/// Returns [`DaemonError`] when socket setup, method registration, or serving fails.
pub async fn serve_until<F>(config: DaemonConfig, external_shutdown: F) -> Result<(), DaemonError>
where
    F: Future<Output = Result<(), DaemonError>>,
{
    let capability = probe_startup_capabilities();

    let listener = bind_socket(config.socket_path.clone())?;
    let _socket_file = SocketFile::new(config.socket_path.clone());
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let module = build_rpc_module(shutdown_tx, capability)?;
    let service_builder = Server::builder().to_service_builder();
    let (stop_handle, server_handle) = jsonrpsee::server::stop_channel();
    let service = service_builder.build(module, stop_handle);
    let mut connections = JoinSet::new();

    info!(socket = %config.socket_path.display(), "orcd JSON-RPC socket ready");

    tokio::pin!(external_shutdown);
    loop {
        tokio::select! {
            signal_result = &mut external_shutdown => {
                signal_result?;
                break;
            }
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(DaemonError::Accept)?;
                let connection_service = service.clone();
                let mut connection_shutdown = shutdown_rx.clone();
                connections.spawn(async move {
                    serve_with_graceful_shutdown(stream, connection_service, async move {
                        let _changed = connection_shutdown.changed().await;
                    })
                    .await
                    .map_err(|error| DaemonError::ServeConnection(error.to_string()))
                });
            }
        }
    }

    server_handle
        .stop()
        .map_err(|error| DaemonError::ServeConnection(error.to_string()))?;
    drain_connections(connections, config.shutdown_timeout).await;
    Ok(())
}

/// Probes Arbitraitor capabilities for the current platform at startup.
///
/// Logs the result so operators can see which controls are effective and which
/// are missing. The returned [`CapabilityReport`] drives the fail-closed
/// decision (spec §6.7) and is forwarded to the health RPC.
fn probe_startup_capabilities() -> CapabilityReport {
    let platform = std::env::consts::OS;
    let client = ArbitraitorClient::default();
    let report = probe_capabilities(&client, platform);

    if !report.protected_services_allowed {
        warn!(
            platform = %report.platform,
            missing_controls = ?report.missing_controls,
            "fail-closed: required Arbitraitor controls unavailable; \
             protected services refused (spec §6.7)"
        );
    } else if report.degraded_mode {
        warn!(
            platform = %report.platform,
            "Arbitraitor capability probe reports degraded controls; \
             protected services allowed with reduced assurance (spec §6.7)"
        );
    } else {
        info!(
            platform = %report.platform,
            "Arbitraitor capability probe passed; all required controls available"
        );
    }

    report
}

fn bind_socket(socket_path: PathBuf) -> Result<UnixListener, DaemonError> {
    match std::fs::remove_file(&socket_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(DaemonError::RemoveStaleSocket {
                path: socket_path,
                source,
            });
        }
    }

    UnixListener::bind(&socket_path).map_err(|source| DaemonError::BindSocket {
        path: socket_path,
        source,
    })
}

async fn sigterm() -> Result<(), DaemonError> {
    let mut term = signal(SignalKind::terminate()).map_err(DaemonError::InstallSigterm)?;
    let _received = term.recv().await;
    Ok(())
}

async fn drain_connections(mut connections: JoinSet<Result<(), DaemonError>>, grace: Duration) {
    let drain = async {
        while let Some(joined) = connections.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(error)) => debug!(%error, "daemon connection closed with error"),
                Err(error) => debug!(%error, "daemon connection task aborted"),
            }
        }
    };
    if timeout(grace, drain).await.is_err() {
        connections.abort_all();
    }
}

#[derive(Debug)]
struct SocketFile {
    path: PathBuf,
}

impl SocketFile {
    const fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_file(&self.path);
    }
}

//! Process monitoring: waits for exit, emits events, restarts up to `max_restarts`.
//!
//! Cancellation via `cancel_rx` kills the child immediately without restart
//! (§17.2.9 invariant 2 — worker failure is isolated).

use std::process::Stdio;

use orchestraitor_model::SessionId;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};
use tracing::warn;

use super::types::{CommandSpec, ProcessStatus, SupervisionEvent};

/// Identifies whether a supervised process is a worker or the MCP gateway.
enum ProcessKind {
    /// Worker process carrying its session identifier.
    Worker {
        /// Session identifier of the worker.
        session: SessionId,
    },
    /// MCP gateway process.
    Gateway,
}

/// Communication channels shared between the supervisor and a monitoring task.
pub(super) struct Channels {
    cancel_rx: watch::Receiver<bool>,
    status_tx: watch::Sender<ProcessStatus>,
    event_tx: mpsc::Sender<SupervisionEvent>,
}

/// Spawns the monitoring task for a worker process.
pub(super) fn spawn_worker_monitor(
    session: SessionId,
    command: CommandSpec,
    max_restarts: u32,
    child: Child,
    channels: Channels,
) {
    tokio::spawn(monitor_process(
        ProcessKind::Worker { session },
        command,
        max_restarts,
        child,
        channels,
    ));
}

/// Spawns the monitoring task for the MCP gateway process.
pub(super) fn spawn_gateway_monitor(
    command: CommandSpec,
    max_restarts: u32,
    child: Child,
    channels: Channels,
) {
    tokio::spawn(monitor_process(
        ProcessKind::Gateway,
        command,
        max_restarts,
        child,
        channels,
    ));
}

/// Creates a [`Channels`] bundle from the supervisor's watch/mpsc senders.
pub(super) fn channels(
    cancel_rx: watch::Receiver<bool>,
    status_tx: watch::Sender<ProcessStatus>,
    event_tx: mpsc::Sender<SupervisionEvent>,
) -> Channels {
    Channels {
        cancel_rx,
        status_tx,
        event_tx,
    }
}

/// Monitors a supervised process: waits for exit, emits events, and restarts
/// up to `max_restarts` times. Cancellation kills the child immediately without
/// restart (§17.2.9 invariant 2).
async fn monitor_process(
    kind: ProcessKind,
    command: CommandSpec,
    max_restarts: u32,
    mut child: Child,
    mut ctx: Channels,
) {
    let mut restarts = 0u32;
    loop {
        let exit_code = tokio::select! {
            result = child.wait() => result.ok().and_then(|status| status.code()),
            _ = ctx.cancel_rx.changed() => {
                let _ = child.kill().await;
                let _ = ctx.status_tx.send(ProcessStatus::Exited { code: None });
                emit_exit(&ctx.event_tx, &kind, None);
                return;
            }
        };

        let _ = ctx
            .status_tx
            .send(ProcessStatus::Exited { code: exit_code });
        emit_exit(&ctx.event_tx, &kind, exit_code);

        if *ctx.cancel_rx.borrow() {
            return;
        }

        restarts += 1;
        if restarts > max_restarts {
            let _ = ctx.status_tx.send(ProcessStatus::Failed);
            emit_failed(&ctx.event_tx, &kind);
            return;
        }

        let _ = ctx
            .status_tx
            .send(ProcessStatus::Restarting { attempt: restarts });
        emit_restarting(&ctx.event_tx, &kind, restarts);

        child = match build_command(&command).spawn() {
            Ok(child) => child,
            Err(error) => {
                warn!(%error, "supervised process restart spawn failed");
                let _ = ctx.status_tx.send(ProcessStatus::Failed);
                emit_failed(&ctx.event_tx, &kind);
                return;
            }
        };
        let pid = child.id().unwrap_or(0);
        let _ = ctx.status_tx.send(ProcessStatus::Running { pid });
        emit_started(&ctx.event_tx, &kind, pid);
    }
}

fn emit_started(tx: &mpsc::Sender<SupervisionEvent>, kind: &ProcessKind, pid: u32) {
    let event = match kind {
        ProcessKind::Worker { session } => SupervisionEvent::WorkerStarted {
            session: session.clone(),
            pid,
        },
        ProcessKind::Gateway => SupervisionEvent::GatewayStarted { pid },
    };
    let _ = tx.try_send(event);
}

fn emit_exit(tx: &mpsc::Sender<SupervisionEvent>, kind: &ProcessKind, code: Option<i32>) {
    let event = match kind {
        ProcessKind::Worker { session } => SupervisionEvent::WorkerExited {
            session: session.clone(),
            code,
        },
        ProcessKind::Gateway => SupervisionEvent::GatewayExited { code },
    };
    let _ = tx.try_send(event);
}

fn emit_restarting(tx: &mpsc::Sender<SupervisionEvent>, kind: &ProcessKind, attempt: u32) {
    let event = match kind {
        ProcessKind::Worker { session } => SupervisionEvent::WorkerRestarting {
            session: session.clone(),
            attempt,
        },
        ProcessKind::Gateway => SupervisionEvent::GatewayRestarting { attempt },
    };
    let _ = tx.try_send(event);
}

fn emit_failed(tx: &mpsc::Sender<SupervisionEvent>, kind: &ProcessKind) {
    let event = match kind {
        ProcessKind::Worker { session } => SupervisionEvent::WorkerFailed {
            session: session.clone(),
        },
        ProcessKind::Gateway => SupervisionEvent::GatewayFailed,
    };
    let _ = tx.try_send(event);
}

/// Builds a [`Command`] from a [`CommandSpec`], redirecting stdio and enabling
/// `kill_on_drop` so the child is reaped if the handle is dropped without `wait`.
pub(super) fn build_command(spec: &CommandSpec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd.kill_on_drop(true);
    cmd
}

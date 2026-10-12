//! Process supervision for worker processes and the MCP gateway (spec §17.2).
//!
//! The supervisor spawns and monitors separate processes for each active agent
//! attempt (worker) and the MCP gateway. Worker failure is isolated: a crash
//! terminates only the failed worker, never sibling work (§17.2.9 invariant 2).
//! Project isolation is enforced at the process boundary: each worker is bound
//! to exactly one [`ProjectId`] and cross-project tool access is refused
//! (§17.2.9 invariant 3).
//!
//! This module owns process lifecycle only. It does not implement sandboxing,
//! filesystem projection, policy, or enforcement — those are Arbitraitor's
//! exclusive domain (§2.2, §16). The complementary adapter-level supervisor
//! ([`orchestraitor_adapter_host::AdapterSupervisor`]) delegates side-effect
//! boundaries to Arbitraitor; this module supervises the OS processes that host
//! those adapter sessions.
//!
//! All channels are bounded: the event stream uses a bounded `mpsc` channel and
//! per-worker status/cancel signals use `watch` channels (§17.2.9 — no
//! unbounded queues).

use std::collections::{HashMap, HashSet};

use orchestraitor_mcp::ProjectId;
use orchestraitor_model::SessionId;
use tokio::sync::{mpsc, watch};

mod monitor;
mod types;

pub use types::{
    CommandSpec, GatewaySpec, ProcessStatus, SupervisionError, SupervisionEvent, WorkerSpec,
};

use monitor::{build_command, channels, spawn_gateway_monitor, spawn_worker_monitor};

/// Bounded capacity for the supervision event channel.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Internal metadata for a supervised worker process.
struct WorkerEntry {
    /// Project this worker is bound to (isolation boundary).
    project: ProjectId,
    /// Receiver for the worker's current status (updated by the monitoring task).
    status_rx: watch::Receiver<ProcessStatus>,
    /// Sender for cancellation signals (kill the worker).
    cancel: watch::Sender<bool>,
}

/// Internal metadata for the supervised MCP gateway process.
struct GatewayEntry {
    /// Receiver for the gateway's current status (updated by the monitoring task).
    status_rx: watch::Receiver<ProcessStatus>,
    /// Sender for cancellation signals (kill the gateway).
    cancel: watch::Sender<bool>,
}

/// Supervises worker processes and the MCP gateway (spec §17.2.1).
///
/// Each worker is a separate OS process bound to exactly one [`ProjectId`].
/// Worker failure is isolated: a crash terminates only the failed worker,
/// never sibling work (§17.2.9 invariant 2). Project isolation is enforced
/// at the process boundary: cross-project tool access is refused
/// (§17.2.9 invariant 3).
///
/// All channels are bounded — the event stream uses a bounded `mpsc` channel
/// and per-worker status/cancel signals use `watch` channels.
pub struct Supervisor {
    /// Worker processes keyed by session id.
    workers: HashMap<SessionId, WorkerEntry>,
    /// MCP gateway process.
    gateway: Option<GatewayEntry>,
    /// Registered tools per project (project isolation registry).
    project_tools: HashMap<ProjectId, HashSet<String>>,
    /// Bounded event channel sender.
    event_tx: mpsc::Sender<SupervisionEvent>,
}

impl Supervisor {
    /// Creates a new supervisor with the default bounded event channel capacity.
    #[must_use]
    pub fn new() -> (Self, mpsc::Receiver<SupervisionEvent>) {
        Self::with_capacity(EVENT_CHANNEL_CAPACITY)
    }

    /// Creates a new supervisor with a custom bounded event channel capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> (Self, mpsc::Receiver<SupervisionEvent>) {
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let supervisor = Self {
            workers: HashMap::new(),
            gateway: None,
            project_tools: HashMap::new(),
            event_tx,
        };
        (supervisor, event_rx)
    }

    /// Spawns a worker process bound to a project.
    ///
    /// The worker is monitored independently — its failure does not affect
    /// sibling workers (§17.2.9 invariant 2).
    ///
    /// # Errors
    ///
    /// Returns [`SupervisionError::SpawnWorker`] if the process cannot be spawned.
    pub fn spawn_worker(&mut self, spec: WorkerSpec) -> Result<SessionId, SupervisionError> {
        let child = build_command(&spec.command)
            .spawn()
            .map_err(|e| SupervisionError::SpawnWorker(e.to_string()))?;
        let pid = child.id().unwrap_or(0);
        let (status_tx, status_rx) = watch::channel(ProcessStatus::Running { pid });
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let session = spec.session_id.clone();
        let project = spec.project.clone();

        spawn_worker_monitor(
            session.clone(),
            spec.command,
            spec.max_restarts,
            child,
            channels(cancel_rx, status_tx, self.event_tx.clone()),
        );

        self.workers.insert(
            session.clone(),
            WorkerEntry {
                project,
                status_rx,
                cancel: cancel_tx,
            },
        );

        let _ = self.event_tx.try_send(SupervisionEvent::WorkerStarted {
            session: session.clone(),
            pid,
        });
        Ok(session)
    }

    /// Spawns the MCP gateway process.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisionError::SpawnGateway`] if the process cannot be spawned.
    pub fn spawn_gateway(&mut self, spec: GatewaySpec) -> Result<(), SupervisionError> {
        let child = build_command(&spec.command)
            .spawn()
            .map_err(|e| SupervisionError::SpawnGateway(e.to_string()))?;
        let pid = child.id().unwrap_or(0);
        let (status_tx, status_rx) = watch::channel(ProcessStatus::Running { pid });
        let (cancel_tx, cancel_rx) = watch::channel(false);

        spawn_gateway_monitor(
            spec.command,
            spec.max_restarts,
            child,
            channels(cancel_rx, status_tx, self.event_tx.clone()),
        );

        self.gateway = Some(GatewayEntry {
            status_rx,
            cancel: cancel_tx,
        });

        let _ = self
            .event_tx
            .try_send(SupervisionEvent::GatewayStarted { pid });
        Ok(())
    }

    /// Registers tools for a project (project isolation registry).
    ///
    /// Tools registered here are the only tools accessible to workers bound
    /// to this project. Cross-project tool access is refused
    /// (§17.2.9 invariant 3).
    pub fn register_project_tools(&mut self, project: &ProjectId, tools: Vec<String>) {
        self.project_tools
            .entry(project.clone())
            .or_default()
            .extend(tools);
    }

    /// Checks whether a worker may access a tool from the given project.
    ///
    /// Returns `Ok(())` if the worker's bound project matches the requested
    /// project and the tool is registered for that project. Returns an error
    /// otherwise (§17.2.9 invariant 3).
    ///
    /// # Errors
    ///
    /// Returns [`SupervisionError::WorkerNotFound`] if the worker is not registered,
    /// [`SupervisionError::CrossProjectAccess`] if the worker's project does not
    /// match the requested project, or [`SupervisionError::ToolNotRegistered`] if
    /// the tool is not registered for the worker's project.
    pub fn check_tool_access(
        &self,
        session: &SessionId,
        tool: &str,
        requested_project: &ProjectId,
    ) -> Result<(), SupervisionError> {
        let entry = self
            .workers
            .get(session)
            .ok_or_else(|| SupervisionError::WorkerNotFound(session.clone()))?;
        if entry.project != *requested_project {
            let _ = self
                .event_tx
                .try_send(SupervisionEvent::CrossProjectRefused {
                    session: session.clone(),
                    tool: tool.to_string(),
                    bound_project: entry.project.clone(),
                    requested_project: requested_project.clone(),
                });
            return Err(SupervisionError::CrossProjectAccess {
                bound_project: entry.project.as_str().to_string(),
                requested_project: requested_project.as_str().to_string(),
                tool: tool.to_string(),
            });
        }
        let registered = self
            .project_tools
            .get(&entry.project)
            .is_some_and(|tools| tools.contains(tool));
        if !registered {
            return Err(SupervisionError::ToolNotRegistered {
                tool: tool.to_string(),
                project: entry.project.as_str().to_string(),
            });
        }
        Ok(())
    }

    /// Returns the status of a worker process.
    #[must_use]
    pub fn worker_status(&self, session: &SessionId) -> Option<ProcessStatus> {
        self.workers
            .get(session)
            .map(|entry| entry.status_rx.borrow().clone())
    }

    /// Returns the status of the MCP gateway process.
    #[must_use]
    pub fn gateway_status(&self) -> Option<ProcessStatus> {
        self.gateway
            .as_ref()
            .map(|entry| entry.status_rx.borrow().clone())
    }

    /// Returns the project a worker is bound to.
    #[must_use]
    pub fn worker_project(&self, session: &SessionId) -> Option<&ProjectId> {
        self.workers.get(session).map(|entry| &entry.project)
    }

    /// Requests shutdown of a specific worker process.
    ///
    /// The worker is killed asynchronously; check [`Supervisor::worker_status`]
    /// to observe the exit.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisionError::WorkerNotFound`] if the worker is not registered.
    pub fn shutdown_worker(&self, session: &SessionId) -> Result<(), SupervisionError> {
        let entry = self
            .workers
            .get(session)
            .ok_or_else(|| SupervisionError::WorkerNotFound(session.clone()))?;
        let _ = entry.cancel.send(true);
        Ok(())
    }

    /// Requests shutdown of the MCP gateway process.
    pub fn shutdown_gateway(&self) {
        if let Some(entry) = &self.gateway {
            let _ = entry.cancel.send(true);
        }
    }

    /// Requests shutdown of all worker processes and the MCP gateway.
    pub fn shutdown_all(&self) {
        for entry in self.workers.values() {
            let _ = entry.cancel.send(true);
        }
        self.shutdown_gateway();
    }
}

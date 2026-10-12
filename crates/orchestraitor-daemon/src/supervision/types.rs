//! Public types for process supervision (spec §17.2).

use std::path::PathBuf;

use orchestraitor_mcp::ProjectId;
use orchestraitor_model::SessionId;

/// Executable command for a supervised process.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Program path to execute.
    pub program: PathBuf,
    /// Arguments passed to the program.
    pub args: Vec<String>,
    /// Environment variables set for the child process.
    pub env: Vec<(String, String)>,
}

/// Specification for spawning a worker process bound to a project.
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    /// Session this worker executes (one worker per active agent attempt).
    pub session_id: SessionId,
    /// Project isolation boundary — the worker may only access this project's tools.
    pub project: ProjectId,
    /// Command to execute.
    pub command: CommandSpec,
    /// Maximum restart attempts before the worker is marked failed.
    pub max_restarts: u32,
}

/// Specification for spawning the MCP gateway process.
#[derive(Debug, Clone)]
pub struct GatewaySpec {
    /// Command to execute.
    pub command: CommandSpec,
    /// Maximum restart attempts before the gateway is marked failed.
    pub max_restarts: u32,
}

/// Status of a supervised process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Process is starting.
    Starting,
    /// Process is running with the given PID.
    Running {
        /// Operating-system process identifier.
        pid: u32,
    },
    /// Process exited with the given code (`None` if killed by a signal).
    Exited {
        /// Exit code, if available.
        code: Option<i32>,
    },
    /// Process is being restarted (nth attempt).
    Restarting {
        /// Current restart attempt number (1-based).
        attempt: u32,
    },
    /// Process exceeded max restarts and is permanently failed.
    Failed,
}

/// Events emitted by the supervisor on the bounded event channel.
#[derive(Debug, Clone)]
pub enum SupervisionEvent {
    /// A worker process started.
    WorkerStarted {
        /// Session identifier of the worker.
        session: SessionId,
        /// Operating-system process identifier.
        pid: u32,
    },
    /// A worker process exited.
    WorkerExited {
        /// Session identifier of the worker.
        session: SessionId,
        /// Exit code, if available (`None` if killed by a signal).
        code: Option<i32>,
    },
    /// A worker is being restarted after failure.
    WorkerRestarting {
        /// Session identifier of the worker.
        session: SessionId,
        /// Current restart attempt number (1-based).
        attempt: u32,
    },
    /// A worker exceeded max restarts and is permanently failed.
    WorkerFailed {
        /// Session identifier of the worker.
        session: SessionId,
    },
    /// The MCP gateway process started.
    GatewayStarted {
        /// Operating-system process identifier.
        pid: u32,
    },
    /// The MCP gateway process exited.
    GatewayExited {
        /// Exit code, if available (`None` if killed by a signal).
        code: Option<i32>,
    },
    /// The MCP gateway is being restarted after failure.
    GatewayRestarting {
        /// Current restart attempt number (1-based).
        attempt: u32,
    },
    /// The MCP gateway exceeded max restarts and is permanently failed.
    GatewayFailed,
    /// Cross-project tool access was refused (§17.2.9 invariant 3).
    CrossProjectRefused {
        /// Session identifier of the worker that attempted access.
        session: SessionId,
        /// Tool name that was requested.
        tool: String,
        /// Project the worker is bound to.
        bound_project: ProjectId,
        /// Project the tool was requested from.
        requested_project: ProjectId,
    },
}

/// Errors returned by the supervisor.
#[derive(Debug, thiserror::Error)]
pub enum SupervisionError {
    /// Worker process spawn failed.
    #[error("worker process spawn failed: {0}")]
    SpawnWorker(String),
    /// MCP gateway process spawn failed.
    #[error("MCP gateway process spawn failed: {0}")]
    SpawnGateway(String),
    /// Worker is not registered.
    #[error("worker `{0}` is not registered")]
    WorkerNotFound(SessionId),
    /// Cross-project tool access was refused (§17.2.9 invariant 3).
    #[error(
        "cross-project tool access refused: worker bound to `{bound_project}` cannot access `{requested_project}`"
    )]
    CrossProjectAccess {
        /// Project the worker is bound to.
        bound_project: String,
        /// Project the tool was requested from.
        requested_project: String,
        /// Tool name that was requested.
        tool: String,
    },
    /// Tool is not registered for the worker's project.
    #[error("tool `{tool}` is not registered for project `{project}`")]
    ToolNotRegistered {
        /// Tool name that was requested.
        tool: String,
        /// Project identifier.
        project: String,
    },
}

//! Durable Orchestraitor daemon: JSON-RPC server, capability probing, supervision, governance, and store.
//!
//! This crate owns the local daemon transport, method registration, and
//! persistence for the `orcd` process. It does not implement security policy
//! or containment; those decisions remain Arbitraitor-owned per spec §2.2
//! and §16.

#![forbid(unsafe_code)]

mod error;
mod rpc;
mod server;
mod watch;

/// The §9.50 GitHub event ingress (issue #504): the poll-only fallback
/// feeding the §9.36 kick-off evaluation — conditional-request poller,
/// durable dedup, bounded queue, and the production `KickoffEventSource`.
pub mod ingress;

/// Startup capability probing and Arbitraitor version negotiation (spec §6.7, §9.6, §16.7).
pub mod capability;

/// Data-governance routing and retry scheduling (spec §9.28, §9.26).
pub mod governance;

/// Process supervision for worker processes and the MCP gateway (spec §17.2).
pub mod supervision;

/// SQLite WAL store and filesystem content-addressed storage.
pub mod store;

pub use capability::{CapabilityReport, ControlStatus, probe_capabilities};
pub use error::DaemonError;
pub use governance::{
    ArbitraitorEnforcer, CircuitBreaker, ClassifiedContent, DataClassification, DataEgressPreview,
    DataReleaseEnforcer, GovernanceError, GovernanceEventSink, GovernanceRouter, ReleaseVerdict,
    RetryPolicy, RetryScheduler, RoutingDecision,
};
pub use ingress::ci_lane as ci_lane_module;
pub use ingress::intake as intake_module;
pub use ingress::{
    KickoffIngressSource, build_ingress,
    ci_lane::{CHECK_RUN_COMPLETED, CiLane, CiRepoTarget, WORKFLOW_RUN_COMPLETED, ci_targets},
    intake::{
        AuthorClass, INTAKE_CONFIG_KEY_PREFIX, IntakeAttacher, IntakeBoard, IntakeCursor,
        IntakeOutcome, SqliteIntakeCursor, classify_author, intake_attachment,
    },
    poller::{DEFAULT_POLL_AFTER_SECS, EventPoller, PollTarget, poll_targets},
    queue::{DEFAULT_QUEUE_CAPACITY, DEGRADED_QUIET_WINDOW, IngressQueue, PushOutcome},
    store::{IngressEvent, IngressStore, REDELIVERY_WINDOW_SECS},
};
pub use rpc::{HealthResponse, InitializeResponse, ShutdownResponse, build_rpc_module};
pub use server::{DEFAULT_SHUTDOWN_TIMEOUT, DaemonConfig, run_until_signal, serve_until};
pub use store::{
    BacklogStateRecord, CasDirectory, CostLedgerRecord, DaemonStore, DelegationRecord,
    LATEST_SCHEMA_VERSION, ReceiptRecord, StoreError, StorePaths, StoreResult,
};
pub use supervision::{
    CommandSpec, GatewaySpec, ProcessStatus, SupervisionError, SupervisionEvent, Supervisor,
    WorkerSpec,
};
pub use watch::{
    BoardSnapshotPoller, DEFAULT_POLL_INTERVAL_SECS, DirectWatchStarter, IngressSettings,
    InstanceLock, IntakeSettings, JournallessSink, KICKOFF_CONFIG_KEY_PREFIX,
    POLL_INTERVAL_CONFIG_KEY, ReconcilePoller, ReconcileSink, SetWatchInvocation,
    WATCH_BUILT_IN_DEFAULTS_TOML, WatchConfig, acquire_instance_lock, append_reasoning_events,
    ingress_kickoff_warnings, intake_ingress_warnings, run_watch,
};

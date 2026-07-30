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

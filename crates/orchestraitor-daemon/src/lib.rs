//! Durable Orchestraitor daemon: JSON-RPC server, capability probing, and store.
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

/// SQLite WAL store and filesystem content-addressed storage.
pub mod store;

pub use capability::{CapabilityReport, ControlStatus, probe_capabilities};
pub use error::DaemonError;
pub use rpc::{HealthResponse, InitializeResponse, ShutdownResponse, build_rpc_module};
pub use server::{DEFAULT_SHUTDOWN_TIMEOUT, DaemonConfig, run_until_signal, serve_until};

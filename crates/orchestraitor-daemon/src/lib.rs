<<<<<<< HEAD
//! Durable Orchestraitor daemon JSON-RPC server.
//!
//! This crate owns the local daemon transport and method registration for the
//! `orcd` process. It does not implement security policy or containment; those
//! decisions remain Arbitraitor-owned per spec §2.2 and §16.

#![forbid(unsafe_code)]

mod error;
mod rpc;
mod server;

pub use error::DaemonError;
pub use rpc::{HealthResponse, InitializeResponse, ShutdownResponse, build_rpc_module};
pub use server::{DEFAULT_SHUTDOWN_TIMEOUT, DaemonConfig, run_until_signal, serve_until};
||||||| parent of 13d059b (feat(daemon): add SQLite WAL store + CAS directory + hash-chained event persistence)
=======
//! Durable daemon primitives for `orcd`.
//!
//! This crate currently exposes the spec §9.17/§11 daemon store. It owns
//! persistence only; security receipts and enforcement decisions remain
//! Arbitraitor-owned data recorded by Orchestraitor.

#![forbid(unsafe_code)]

/// SQLite WAL store and filesystem content-addressed storage.
pub mod store;
>>>>>>> 13d059b (feat(daemon): add SQLite WAL store + CAS directory + hash-chained event persistence)

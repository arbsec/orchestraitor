//! Normalized events and append-only audit store for Orchestraitor.
//!
//! This crate implements spec §9.17/§9.17.1 event records, canonical bytes, redaction, and
//! tamper-evident hash-chain validation. It contains no security decision logic.

#![forbid(unsafe_code)]

mod error;
mod hash;
mod redaction;
mod schema;
mod store;
#[cfg(test)]
mod tests;
mod tracing_layer;

pub use error::EventError;
pub use hash::{HashDigest, hash_envelope, validate_hash_chain};
pub use redaction::{PrivacyExportMode, redact_event};
pub use schema::{
    CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput, SchemaInterpretation,
};
pub use store::{AuditRecord, AuditStore, EventQuery, InMemoryAuditStore};
pub use tracing_layer::TracingAuditLayer;

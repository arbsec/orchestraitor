//! Event envelope schema and event category vocabulary.

use orchestraitor_core::is_redacted_field;
use orchestraitor_model::OperationId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EventError, HashDigest};

/// Current normalized event schema version.
pub const CURRENT_SCHEMA_VERSION: u16 = 1;

/// Replay interpretation status for a versioned event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaInterpretation {
    /// The event schema is known to this crate.
    Interpreted,
    /// The event uses a future schema and must be preserved without semantic replay.
    Uninterpreted,
}

/// Normalized event category names from spec §9.17.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    /// Session lifecycle event.
    SessionLifecycle,
    /// Adapter lifecycle event.
    AdapterLifecycle,
    /// Provider model request event.
    ModelRequest,
    /// Provider model response metadata event.
    ModelResponseMetadata,
    /// Context selection event.
    ContextSelection,
    /// Tool request event.
    ToolRequest,
    /// Action plan event.
    ActionPlan,
    /// Arbitraitor policy decision receipt reference.
    PolicyDecision,
    /// Trusted UI approval event.
    Approval,
    /// Process execution event.
    ProcessExecution,
    /// Network request event.
    NetworkRequest,
    /// Secret-use event, with secret material redacted.
    SecretUse,
    /// File observation event.
    FileObservation,
    /// Git operation event.
    GitOperation,
    /// Output promotion event.
    OutputPromotion,
    /// Sandbox capability event.
    SandboxCapability,
    /// Resource usage event.
    ResourceUsage,
    /// Error event.
    Error,
    /// Security finding event.
    SecurityFinding,
}

/// Normalized append-only event envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Version of the serialized event schema.
    pub schema_version: u16,
    /// Per-store monotonic sequence number.
    pub monotonic_seq: u64,
    /// Wall-clock timestamp captured at source, encoded as RFC 3339 or another stable string.
    pub wall_clock_ts: String,
    /// Correlation operation identifier for cross-event reconstruction.
    pub correlation_id: OperationId,
    /// Parent operation identifier when this event is nested under another operation.
    pub parent_op_id: Option<OperationId>,
    /// Normalized event category.
    pub category: EventCategory,
    /// Category-specific payload. Sensitive values must be redacted before construction.
    pub payload: Value,
    /// SHA-256 digest of the previous record's canonical envelope bytes.
    pub prev_hash: Option<HashDigest>,
}

impl EventEnvelope {
    /// Creates a validated event envelope.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::SensitiveField`] when a payload key is credential-like.
    pub fn try_new(input: EventEnvelopeInput) -> Result<Self, EventError> {
        reject_sensitive_fields(&input.payload)?;
        Ok(Self {
            schema_version: input.schema_version,
            monotonic_seq: input.monotonic_seq,
            wall_clock_ts: input.wall_clock_ts,
            correlation_id: input.correlation_id,
            parent_op_id: input.parent_op_id,
            category: input.category,
            payload: input.payload,
            prev_hash: input.prev_hash,
        })
    }

    /// Returns whether this event can be semantically interpreted by this crate.
    #[must_use]
    pub const fn schema_interpretation(&self) -> SchemaInterpretation {
        if self.schema_version <= CURRENT_SCHEMA_VERSION {
            SchemaInterpretation::Interpreted
        } else {
            SchemaInterpretation::Uninterpreted
        }
    }
}

/// Input object used to construct [`EventEnvelope`] without parameter bloat.
#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelopeInput {
    /// Version of the serialized event schema.
    pub schema_version: u16,
    /// Per-store monotonic sequence number.
    pub monotonic_seq: u64,
    /// Wall-clock timestamp captured at source.
    pub wall_clock_ts: String,
    /// Correlation operation identifier for cross-event reconstruction.
    pub correlation_id: OperationId,
    /// Parent operation identifier when this event is nested under another operation.
    pub parent_op_id: Option<OperationId>,
    /// Normalized event category.
    pub category: EventCategory,
    /// Category-specific payload.
    pub payload: Value,
    /// SHA-256 digest of the previous record's canonical envelope bytes.
    pub prev_hash: Option<HashDigest>,
}

fn reject_sensitive_fields(value: &Value) -> Result<(), EventError> {
    match value {
        Value::Object(object) => {
            for (key, nested) in object {
                if is_redacted_field(key) {
                    return Err(EventError::SensitiveField { field: key.clone() });
                }
                reject_sensitive_fields(nested)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_sensitive_fields(item)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}

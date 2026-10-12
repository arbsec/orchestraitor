//! Partial-result preservation in the normalized event store.

use std::time::{SystemTime, UNIX_EPOCH};

use orchestraitor_events::{
    AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
    HashDigest,
};
use orchestraitor_model::OperationId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{IdempotencyKey, LifecycleError};

/// Partial result categories preserved on failure or cancellation (spec §9.24.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialResultKind {
    /// Partial patch captured before terminal interruption.
    PartialPatch,
    /// Completed tool call that must not be re-run on replay.
    CompletedToolCall,
    /// Model response or partial stream chunk.
    ModelResponse,
}

/// Partial result to persist in the audit event store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartialResult {
    /// Event operation id for this preserved result.
    pub operation_id: OperationId,
    /// Idempotency key proving replay-safe append intent.
    pub idempotency_key: IdempotencyKey,
    /// Kind of partial result.
    pub kind: PartialResultKind,
    /// Whether the payload is incomplete.
    pub partial: bool,
    /// Redacted category-specific data.
    pub payload: Value,
}

/// Cursor for constructing hash-chained event envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventCursor {
    /// Next sequence number expected by the store.
    pub next_sequence: u64,
    /// Previous event hash, if any.
    pub previous_hash: Option<HashDigest>,
}

impl EventCursor {
    /// Creates a cursor for an empty event store.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            next_sequence: 1,
            previous_hash: None,
        }
    }
}

/// Appends partial-result events and advances the caller's event cursor.
#[derive(Debug)]
pub struct PartialResultStore<'a, S: AuditStore> {
    store: &'a mut S,
    cursor: EventCursor,
}

impl<'a, S: AuditStore> PartialResultStore<'a, S> {
    /// Wraps an audit store and cursor.
    pub const fn new(store: &'a mut S, cursor: EventCursor) -> Self {
        Self { store, cursor }
    }

    /// Returns the current append cursor.
    #[must_use]
    pub const fn cursor(&self) -> &EventCursor {
        &self.cursor
    }

    /// Preserves a partial result in the event store.
    ///
    /// # Errors
    /// Returns [`LifecycleError::EventStore`] if the event envelope or hash chain is rejected.
    pub fn preserve(&mut self, result: PartialResult) -> Result<OperationId, LifecycleError> {
        let operation_id = result.operation_id.clone();
        let envelope = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: self.cursor.next_sequence,
            wall_clock_ts: unix_timestamp_string()?,
            correlation_id: result.operation_id,
            parent_op_id: None,
            category: event_category(result.kind),
            payload: json!({
                "idempotency": result.idempotency_key.as_str(),
                "kind": result.kind,
                "partial": result.partial,
                "result": result.payload,
            }),
            prev_hash: self.cursor.previous_hash.clone(),
        })?;
        let record = self.store.append(envelope)?;
        self.cursor.next_sequence = self.cursor.next_sequence.saturating_add(1);
        self.cursor.previous_hash = Some(record.hash);
        Ok(operation_id)
    }
}

fn event_category(kind: PartialResultKind) -> EventCategory {
    match kind {
        PartialResultKind::PartialPatch => EventCategory::OutputPromotion,
        PartialResultKind::CompletedToolCall => EventCategory::ToolRequest,
        PartialResultKind::ModelResponse => EventCategory::ModelResponseMetadata,
    }
}

fn unix_timestamp_string() -> Result<String, LifecycleError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| LifecycleError::ClockBeforeUnixEpoch)?;
    Ok(timestamp.as_secs().to_string())
}

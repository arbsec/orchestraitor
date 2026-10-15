//! §9.48.8 `agent reasoning` event appender.
//!
//! ONE shared implementation for both operating modes (daemon `orcd watch`
//! and foreground `orc loop`): every receipted `think` call — accepted step
//! or typed refusal — appends one hash-chained `agent reasoning` event to
//! the §9.17 event store. The audit metadata always rides; `thought_text`
//! rides ONLY under payload-recording consent (spec 50 §20.4.2); refused
//! steps never carry text under any consent setting (§9.48.4).

use std::sync::Mutex;

use crate::{AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput};

/// Appends one hash-chained `agent reasoning` event per receipted `think`
/// call — accepted step OR typed refusal (§9.48.1, §9.48.8) — to the given
/// §9.17 event store. The payload always carries the audit metadata
/// (`thought_number`, `total_thoughts`, `next_thought_needed`, `is_revision`,
/// `revises_thought`, `thought_chars`, `sequence_id`, `task_id`,
/// `accepted`, `refusal_reason`); `thought_text` rides ONLY under
/// payload-recording consent (spec 50 §20.4.2) — the second defense layer
/// after the §9.48.4 source refusal, with export redaction (the
/// `PrivacyExportMode::Redacted` class the category is wired into) as the
/// third. Refused steps carry no text under any consent setting.
///
/// Per-event atomicity (the reconcile recorder's convention): a mid-batch
/// failure leaves the store consistent with the recorded prefix, so a retry
/// records only the unrecorded suffix and never duplicates.
///
/// # Errors
///
/// Returns the store error when the head read or any append fails.
pub fn append_reasoning_events<S: AuditStore, R: ReasoningEvent>(
    events: &Mutex<S>,
    task_id: &str,
    records: &[R],
    record_payloads: bool,
) -> Result<(), crate::EventError> {
    let mut store = events.lock().map_err(|_| {
        crate::EventError::Sqlite(rusqlite::Error::InvalidParameterName(
            "reasoning event store lock poisoned".into(),
        ))
    })?;
    for record in records {
        // Accepted steps carry text only under consent; refusals never do.
        let thought_text = if record_payloads {
            record.thought_text().map(str::to_owned)
        } else {
            None
        };
        let payload = serde_json::json!({
            "event": "agent-reasoning",
            "task_id": task_id,
            "accepted": record.accepted(),
            "refusal_reason": record.refusal_reason(),
            "thought_number": record.thought_number(),
            "total_thoughts": record.total_thoughts(),
            "next_thought_needed": record.next_thought_needed(),
            "is_revision": record.is_revision(),
            "revises_thought": record.revises_thought(),
            "thought_chars": record.thought_chars(),
            "sequence_id": record.sequence_id(),
            "thought_text": thought_text,
        });
        let head = store.head()?;
        let envelope = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: u64::try_from(head.seq_base)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            wall_clock_ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            correlation_id: orchestraitor_model::OperationId::new(),
            parent_op_id: None,
            category: EventCategory::AgentReasoning,
            payload,
            prev_hash: head.prev_hash,
        })?;
        store.append(envelope)?;
    }
    Ok(())
}

/// The audit-metadata accessors of one receipted reasoning step (§9.48.8).
/// The worker crate's [`crate::ReasoningRecord`]-shaped type implements
/// this, so both consumers share ONE event appender without the events
/// crate depending on the worker crate.
pub trait ReasoningEvent {
    /// Whether the call was accepted (`false` = typed refusal).
    fn accepted(&self) -> bool;
    /// The static refusal reason code; `None` on an accepted step.
    fn refusal_reason(&self) -> Option<&'static str>;
    /// Position in the sequence (1-based).
    fn thought_number(&self) -> u32;
    /// Current step-count estimate.
    fn total_thoughts(&self) -> u32;
    /// Whether more steps are coming.
    fn next_thought_needed(&self) -> bool;
    /// Whether this call revises earlier thinking.
    fn is_revision(&self) -> bool;
    /// Which thought is reconsidered; `None` when not a revision.
    fn revises_thought(&self) -> Option<u32>;
    /// Char count of the thought text at validation time.
    fn thought_chars(&self) -> usize;
    /// Sequence this step belongs to (1-based).
    fn sequence_id(&self) -> u32;
    /// The consent-gated deliberation text (`None` for refusals and for
    /// consent-off accepted steps).
    fn thought_text(&self) -> Option<&str>;
}

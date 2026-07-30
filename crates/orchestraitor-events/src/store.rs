//! Audit store trait and in-memory append-only implementation.

use serde::{Deserialize, Serialize};

use crate::{
    EventCategory, EventEnvelope, EventError, HashDigest, PrivacyExportMode, SchemaInterpretation,
    hash_envelope, redact_event, validate_hash_chain,
};

/// Stored audit record containing an envelope and its canonical hash.
///
/// Interpretation status is *not* stored as a separate field — it is always
/// derived from [`Self::envelope`]`.schema_version`, so any tampering with the
/// interpretation flips the envelope bytes and therefore the hash chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Event envelope stored in append-only order.
    pub envelope: EventEnvelope,
    /// SHA-256 hash of canonical envelope bytes.
    pub hash: HashDigest,
}

impl AuditRecord {
    /// Creates a hash-chained record from an event envelope.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::CanonicalJson`] when canonical hashing fails.
    pub fn try_from_envelope(envelope: EventEnvelope) -> Result<Self, EventError> {
        let hash = hash_envelope(&envelope)?;
        Ok(Self { envelope, hash })
    }

    /// Returns the replay interpretation status derived from the envelope's
    /// `schema_version`. Recomputed on demand so the value cannot be tampered
    /// with independently of the hash-protected envelope.
    #[must_use]
    pub const fn schema_interpretation(&self) -> SchemaInterpretation {
        self.envelope.schema_interpretation()
    }
}

/// Query filter for audit event retrieval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventQuery {
    /// Optional category filter.
    pub category: Option<EventCategory>,
    /// Inclusive lower sequence bound.
    pub since_seq: Option<u64>,
    /// Inclusive upper sequence bound.
    pub until_seq: Option<u64>,
    /// Whether future-schema events should be returned.
    pub include_uninterpreted: bool,
}

/// Append-only audit store abstraction.
pub trait AuditStore {
    /// Appends one validated event envelope and returns the stored record.
    ///
    /// # Errors
    ///
    /// Returns an [`EventError`] when hash-chain continuity or canonical hashing fails.
    fn append(&mut self, envelope: EventEnvelope) -> Result<AuditRecord, EventError>;

    /// Queries stored records by category, sequence range, and schema interpretation.
    ///
    /// # Errors
    ///
    /// Implementations may return validation or backend errors.
    fn query(&self, query: &EventQuery) -> Result<Vec<AuditRecord>, EventError>;

    /// Exports records as canonical JSON Lines, optionally redacted.
    ///
    /// # Errors
    ///
    /// Returns an [`EventError`] when serialization fails.
    fn export(&self, mode: PrivacyExportMode) -> Result<Vec<u8>, EventError>;

    /// Imports records from canonical or ordinary JSON Lines and validates the hash chain.
    ///
    /// # Errors
    ///
    /// Returns an [`EventError`] when parsing or chain validation fails.
    fn r#import(&mut self, bytes: &[u8]) -> Result<Vec<AuditRecord>, EventError>;
}

/// In-memory audit store for tests and embedded callers.
#[derive(Debug, Clone, Default)]
pub struct InMemoryAuditStore {
    records: Vec<AuditRecord>,
}

impl InMemoryAuditStore {
    /// Returns all records in append order.
    #[must_use]
    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }
}

impl AuditStore for InMemoryAuditStore {
    fn append(&mut self, envelope: EventEnvelope) -> Result<AuditRecord, EventError> {
        validate_next_envelope(&self.records, &envelope)?;
        let record = AuditRecord::try_from_envelope(envelope)?;
        self.records.push(record.clone());
        Ok(record)
    }

    fn query(&self, query: &EventQuery) -> Result<Vec<AuditRecord>, EventError> {
        Ok(self
            .records
            .iter()
            .filter(|record| matches_query(record, query))
            .cloned()
            .collect())
    }

    fn export(&self, mode: PrivacyExportMode) -> Result<Vec<u8>, EventError> {
        let mut output = Vec::new();
        let mut previous_hash = None;
        for record in &self.records {
            let mut envelope = redact_event(record, mode);
            envelope.prev_hash = previous_hash;
            let exported = AuditRecord::try_from_envelope(envelope)?;
            previous_hash = Some(exported.hash.clone());
            serde_json_canonicalizer::to_writer(&exported, &mut output)
                .map_err(EventError::CanonicalJson)?;
            output.push(b'\n');
        }
        Ok(output)
    }

    fn r#import(&mut self, bytes: &[u8]) -> Result<Vec<AuditRecord>, EventError> {
        let mut imported = Vec::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            imported.push(serde_json::from_slice::<AuditRecord>(line)?);
        }
        validate_hash_chain(&imported)?;
        self.records = imported;
        Ok(self.records.clone())
    }
}

fn validate_next_envelope(
    records: &[AuditRecord],
    envelope: &EventEnvelope,
) -> Result<(), EventError> {
    let expected_sequence = u64::try_from(records.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if envelope.monotonic_seq != expected_sequence {
        return Err(EventError::SequenceGap {
            expected: expected_sequence,
            observed: envelope.monotonic_seq,
        });
    }
    let expected_previous = records.last().map(|record| record.hash.clone());
    if envelope.prev_hash != expected_previous {
        return Err(EventError::PreviousHashMismatch {
            sequence: envelope.monotonic_seq,
        });
    }
    Ok(())
}

fn matches_query(record: &AuditRecord, query: &EventQuery) -> bool {
    category_matches(record, query)
        && lower_bound_matches(record, query)
        && upper_bound_matches(record, query)
        && schema_matches(record, query)
}

fn category_matches(record: &AuditRecord, query: &EventQuery) -> bool {
    match query.category {
        Some(category) => record.envelope.category == category,
        None => true,
    }
}

fn lower_bound_matches(record: &AuditRecord, query: &EventQuery) -> bool {
    match query.since_seq {
        Some(since_seq) => record.envelope.monotonic_seq >= since_seq,
        None => true,
    }
}

fn upper_bound_matches(record: &AuditRecord, query: &EventQuery) -> bool {
    match query.until_seq {
        Some(until_seq) => record.envelope.monotonic_seq <= until_seq,
        None => true,
    }
}

fn schema_matches(record: &AuditRecord, query: &EventQuery) -> bool {
    query.include_uninterpreted
        || record.schema_interpretation() == SchemaInterpretation::Interpreted
}

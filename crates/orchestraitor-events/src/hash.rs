//! Canonical hashing and hash-chain validation.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{EventEnvelope, EventError, store::AuditRecord};

/// Hex-encoded SHA-256 digest over canonical event bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HashDigest(
    /// Lowercase hexadecimal SHA-256 digest.
    pub String,
);

impl HashDigest {
    /// Returns the digest as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Computes the canonical SHA-256 digest for an event envelope.
///
/// # Errors
///
/// Returns [`EventError::CanonicalJson`] when canonical JSON serialization fails.
pub fn hash_envelope(envelope: &EventEnvelope) -> Result<HashDigest, EventError> {
    let canonical =
        serde_json_canonicalizer::to_vec(envelope).map_err(EventError::CanonicalJson)?;
    let digest = Sha256::digest(&canonical);
    Ok(HashDigest(hex::encode(digest)))
}

/// Validates contiguous sequence numbers, previous-hash pointers, and record hashes.
///
/// # Errors
///
/// Returns an [`EventError`] when a gap, pointer mismatch, or tamper mismatch is detected.
pub fn validate_hash_chain(records: &[AuditRecord]) -> Result<(), EventError> {
    let mut expected_sequence = 1_u64;
    let mut previous_hash: Option<HashDigest> = None;
    for record in records {
        let sequence = record.envelope.monotonic_seq;
        if sequence != expected_sequence {
            return Err(EventError::SequenceGap {
                expected: expected_sequence,
                observed: sequence,
            });
        }
        if record.envelope.prev_hash != previous_hash {
            return Err(EventError::PreviousHashMismatch { sequence });
        }
        let computed = hash_envelope(&record.envelope)?;
        if computed != record.hash {
            return Err(EventError::RecordHashMismatch { sequence });
        }
        previous_hash = Some(computed);
        expected_sequence = expected_sequence.saturating_add(1);
    }
    Ok(())
}

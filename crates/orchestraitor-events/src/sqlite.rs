//! SQLite-backed append-only audit store with hash-chain validation.
//!
//! Mirrors the in-memory store semantics: records are validated on append and
//! re-validated on read, so stored metadata (category, hash, schema version) is
//! never trusted — everything is recomputed from the canonical envelope bytes.
//! The unkeyed chain detects inconsistencies and incidental corruption but is
//! not a defense against a database-level rewriter, who can rebuild it.

use crate::store::validate_next_envelope;
use crate::{
    AuditRecord, AuditStore, EventEnvelope, EventError, EventQuery, PrivacyExportMode,
    SchemaInterpretation, hash_envelope, redact_event, validate_hash_chain,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// SQLite-backed audit store persisting canonical envelope bytes in WAL mode.
///
/// The schema stores only the canonical envelope JSON plus derived index
/// metadata; every read recomputes the hash from the envelope bytes, so any
/// tampering with the stored blob is detected exactly like the in-memory store
/// detects mutated imports (a database-level rewriter who recomputes the
/// unkeyed chain is out of scope — see the changelog entry for the exact
/// guarantees).
pub struct SqliteAuditStore {
    conn: Connection,
}

impl std::fmt::Debug for SqliteAuditStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteAuditStore")
            .finish_non_exhaustive()
    }
}

impl SqliteAuditStore {
    /// Opens (or creates) a file-backed audit store with WAL journaling.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::Sqlite`] when the database cannot be opened or
    /// the schema cannot be initialized.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, EventError> {
        let conn = Connection::open(path)?;
        Self::initialize(conn)
    }

    /// Opens an in-memory audit store (for tests and ephemeral callers).
    ///
    /// # Errors
    ///
    /// Returns [`EventError::Sqlite`] when the schema cannot be initialized.
    pub fn open_in_memory() -> Result<Self, EventError> {
        let conn = Connection::open_in_memory()?;
        Self::initialize(conn)
    }

    /// Exposes the underlying connection for test instrumentation (e.g.
    /// installing a rusqlite authorizer hook to interleave concurrent writers).
    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    fn initialize(mut conn: Connection) -> Result<Self, EventError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // All writers (append and import) must take the RESERVED lock up
        // front: a deferred BEGIN could interleave with another connection's
        // history replacement and persist a hash chain broken at the seam.
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_records (
                monotonic_seq INTEGER PRIMARY KEY,
                category TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                hash TEXT NOT NULL UNIQUE,
                envelope_json BLOB NOT NULL
            )",
            [],
        )?;
        Ok(Self { conn })
    }

    /// Loads and re-validates the record with the highest sequence number.
    ///
    /// When called from `append`/`import`, this must be invoked on a
    /// connection already inside a transaction so the head snapshot cannot
    /// diverge from the write that follows it.
    fn head_record(conn: &Connection) -> Result<Option<AuditRecord>, EventError> {
        Ok(Self::load_verified_records(conn)?.pop())
    }

    /// Decodes and re-validates one stored envelope against its recomputed hash.
    ///
    /// The stored `hash` column is treated as untrusted metadata: the digest is
    /// recomputed from the envelope bytes and compared against it, so any byte
    /// flip in the stored blob is quarantined as a [`EventError::RecordHashMismatch`].
    fn decode_envelope(envelope_json: &[u8], stored_hash: &str) -> Result<AuditRecord, EventError> {
        let envelope = serde_json::from_slice::<EventEnvelope>(envelope_json)?;
        let computed = hash_envelope(&envelope)?;
        let sequence = envelope.monotonic_seq;
        if computed.as_str() != stored_hash {
            return Err(EventError::RecordHashMismatch { sequence });
        }
        Ok(AuditRecord {
            envelope,
            hash: computed,
        })
    }

    /// Loads all records with full untrusted-metadata verification and chain
    /// validation over the decoded set.
    ///
    /// Beyond the per-record hash re-computation in [`Self::decode_envelope`],
    /// this enforces on read that the stored `category`, `schema_version`, and
    /// `monotonic_seq` columns match the envelope bytes, and that the whole
    /// set forms a valid hash chain — a database-level edit of either the
    /// envelope or any metadata column is quarantined, never silently skipped.
    ///
    /// Takes the connection as a parameter so `append`/`import` can call it
    /// on their transaction connection: the head record then comes from the
    /// last entry of the fully-verified chain, so a corrupted or deleted
    /// middle row fails the append instead of leaving a valid-looking head
    /// on top of a broken chain.
    fn load_verified_records(conn: &Connection) -> Result<Vec<AuditRecord>, EventError> {
        let mut statement = conn.prepare(
            "SELECT monotonic_seq, category, schema_version, envelope_json, hash
             FROM audit_records ORDER BY monotonic_seq ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let records = rows
            .iter()
            .map(|(seq, category, schema_version, bytes, hash)| {
                let record = Self::decode_envelope(bytes, hash)?;
                let sequence = record.envelope.monotonic_seq;
                if i64::try_from(sequence) != Ok(*seq) {
                    return Err(EventError::RecordHashMismatch { sequence });
                }
                if serde_json::to_string(&record.envelope.category)? != *category
                    || i64::from(record.envelope.schema_version) != *schema_version
                {
                    return Err(EventError::RecordHashMismatch { sequence });
                }
                Ok(record)
            })
            .collect::<Result<Vec<_>, EventError>>()?;
        validate_hash_chain(&records)?;
        Ok(records)
    }

    fn insert(conn: &Connection, record: &AuditRecord) -> Result<(), EventError> {
        let sequence =
            i64::try_from(record.envelope.monotonic_seq).map_err(|_| EventError::SequenceGap {
                expected: 1,
                observed: record.envelope.monotonic_seq,
            })?;
        let envelope_json = serde_json_canonicalizer::to_vec(&record.envelope)
            .map_err(EventError::CanonicalJson)?;
        let category = serde_json::to_string(&record.envelope.category)?;
        let result = conn.execute(
            "INSERT INTO audit_records
                (monotonic_seq, category, schema_version, hash, envelope_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                sequence,
                category,
                i64::from(record.envelope.schema_version),
                record.hash.as_str(),
                envelope_json,
            ],
        );
        if let Err(error) = result {
            if is_primary_key_violation(&error) {
                return Err(EventError::SequenceGap {
                    expected: Self::next_sequence(conn)?,
                    observed: record.envelope.monotonic_seq,
                });
            }
            return Err(EventError::Sqlite(error));
        }
        Ok(())
    }

    fn next_sequence(conn: &Connection) -> Result<u64, EventError> {
        let last: Option<i64> = conn
            .query_row("SELECT MAX(monotonic_seq) FROM audit_records", [], |row| {
                row.get(0)
            })
            .optional()?
            .flatten();
        let last = last.unwrap_or(0);
        u64::try_from(last).map_or(Ok(1), |last| Ok(last.saturating_add(1)))
    }

    /// Reads the count and head record inside the caller's snapshot.
    fn head_snapshot(&self) -> Result<crate::AuditHead, EventError> {
        let head = Self::head_record(&self.conn)?;
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM audit_records", [], |row| row.get(0))?;
        Ok(crate::AuditHead {
            seq_base: usize::try_from(count).unwrap_or(usize::MAX),
            prev_hash: head.map(|record| record.hash),
        })
    }
}

fn is_primary_key_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    ) && error.sqlite_error().is_some_and(|database_error| {
        database_error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
    })
}

impl AuditStore for SqliteAuditStore {
    fn append(&mut self, envelope: EventEnvelope) -> Result<AuditRecord, EventError> {
        // Validate-then-insert must be atomic: a concurrent connection may
        // `import` (replace the whole history) between our head read and our
        // insert, which would persist a record whose `prev_hash` points at a
        // predecessor that no longer exists. An IMMEDIATE transaction takes
        // the write lock before the head snapshot, so the validated head is
        // still the head at commit time.
        let transaction = self.conn.unchecked_transaction()?;
        let head = Self::head_record(&transaction)?;
        if let Some(record) = &head {
            validate_next_envelope(std::slice::from_ref(record), &envelope)?;
        } else if envelope.monotonic_seq != 1 || envelope.prev_hash.is_some() {
            return Err(EventError::SequenceGap {
                expected: 1,
                observed: envelope.monotonic_seq,
            });
        }
        let record = AuditRecord::try_from_envelope(envelope)?;
        Self::insert(&transaction, &record)?;
        transaction.commit()?;
        Ok(record)
    }

    fn query(&self, query: &EventQuery) -> Result<Vec<AuditRecord>, EventError> {
        // Full scan + full chain validation on every read: stored metadata
        // columns (category, schema_version, monotonic_seq) are untrusted, so
        // filtering must happen in memory on decoded envelopes, never on the
        // SQL columns — a database-level column edit must never hide a row.
        // Audit stores are append-mostly with small per-workspace record
        // counts, so the O(n) scan is acceptable at MVP scale.
        let records = Self::load_verified_records(&self.conn)?;
        Ok(records
            .into_iter()
            .filter(|record| {
                if let Some(category) = query.category
                    && record.envelope.category != category
                {
                    return false;
                }
                if let Some(since_seq) = query.since_seq
                    && record.envelope.monotonic_seq < since_seq
                {
                    return false;
                }
                if let Some(until_seq) = query.until_seq
                    && record.envelope.monotonic_seq > until_seq
                {
                    return false;
                }
                if !query.include_uninterpreted
                    && record.schema_interpretation() != SchemaInterpretation::Interpreted
                {
                    return false;
                }
                true
            })
            .collect())
    }

    fn head(&self) -> Result<crate::AuditHead, EventError> {
        // Count and head record must come from one snapshot: reading them in
        // separate autocommit statements could pair a count from one write
        // generation with a head hash from another. A deferred read
        // transaction pins one WAL snapshot without taking the write lock.
        self.conn.execute_batch("BEGIN DEFERRED")?;
        match self.head_snapshot() {
            Ok(head) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(head)
            }
            Err(error) => {
                // Best-effort rollback; the transaction holds no writes, so
                // the connection returns to autocommit either way.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn export(&self, mode: PrivacyExportMode) -> Result<Vec<u8>, EventError> {
        let mut output = Vec::new();
        let mut previous_hash = None;
        for record in &Self::load_verified_records(&self.conn)? {
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
        // IMMEDIATE (connection-wide behavior): history replacement must hold
        // the write lock from the delete through the re-insert so a concurrent
        // appender cannot interleave between the DELETE and the new rows.
        let transaction = self.conn.transaction()?;
        transaction.execute("DELETE FROM audit_records", [])?;
        for record in &imported {
            Self::insert(&transaction, record)?;
        }
        transaction.commit()?;
        Ok(imported)
    }
}

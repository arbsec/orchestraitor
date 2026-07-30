//! `SQLite` metadata store with append-only event records and migrations.

use std::fs;
use std::path::{Path, PathBuf};

use orchestraitor_events::{
    AuditRecord, EventEnvelope, EventError, EventQuery, HashDigest, validate_hash_chain,
};
use orchestraitor_model::{Session, SessionId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::store::cas::CasDirectory;
use crate::store::records::{
    BacklogStateRecord, CostLedgerRecord, DelegationRecord, ReceiptRecord, category_text,
};
use crate::store::schema::SCHEMA_V1;
use crate::store::{StoreError, StoreResult};

/// Latest daemon-store schema version.
pub const LATEST_SCHEMA_VERSION: u32 = 1;

/// Filesystem paths for the daemon store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePaths {
    /// `SQLite` database path.
    pub database_path: PathBuf,
    /// CAS root directory path.
    pub cas_root: PathBuf,
}

/// `SQLite` WAL metadata store plus filesystem CAS handle.
pub struct DaemonStore {
    conn: Connection,
    cas: CasDirectory,
}

impl DaemonStore {
    /// Opens the daemon store, initializes WAL mode, and runs migrations.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the database, CAS, or schema initialization fails.
    pub fn open(paths: &StorePaths) -> StoreResult<Self> {
        create_parent(&paths.database_path)?;
        let mut conn = Connection::open(&paths.database_path)?;
        configure_connection(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn,
            cas: CasDirectory::open(&paths.cas_root)?,
        })
    }

    /// Opens an in-memory `SQLite` store with an on-disk CAS root for tests.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the CAS or schema initialization fails.
    pub fn open_in_memory(cas_root: impl AsRef<Path>) -> StoreResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn,
            cas: CasDirectory::open(cas_root)?,
        })
    }

    /// Returns the CAS directory handle.
    #[must_use]
    pub const fn cas(&self) -> &CasDirectory {
        &self.cas
    }

    /// Returns `SQLite`'s active journal mode.
    ///
    /// # Errors
    /// Returns [`StoreError`] when querying `SQLite` fails.
    pub fn journal_mode(&self) -> StoreResult<String> {
        self.conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Persists session metadata.
    ///
    /// # Errors
    /// Returns [`StoreError`] when serialization or insertion fails.
    pub fn upsert_session(&self, session: &Session) -> StoreResult<()> {
        let payload = serde_json::to_string(session)?;
        self.conn.execute(
            SESSION_SQL,
            params![
                session.id.as_str(),
                session.repository_id.as_str(),
                session.adapter_id.as_str(),
                session.workspace_id.as_str(),
                session.security_mode.to_string(),
                session.policy_digest.as_str(),
                session.state.to_string(),
                session.created_at.to_rfc3339(),
                payload
            ],
        )?;
        Ok(())
    }

    /// Persists one cost-ledger row.
    ///
    /// # Errors
    /// Returns [`StoreError`] when serialization or insertion fails.
    pub fn insert_cost_ledger(&self, record: &CostLedgerRecord) -> StoreResult<()> {
        let payload = serde_json::to_string(&record.payload)?;
        self.conn.execute("INSERT OR REPLACE INTO cost_ledger (request_id, session_id, provider_id, model_id, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)", params![record.request_id, record.session_id.as_str(), record.provider_id.as_str(), record.model_id.as_str(), payload])?;
        Ok(())
    }

    /// Persists one backlog state row.
    ///
    /// # Errors
    /// Returns [`StoreError`] when serialization or insertion fails.
    pub fn upsert_backlog_state(&self, record: &BacklogStateRecord) -> StoreResult<()> {
        let payload = serde_json::to_string(&record.payload)?;
        self.conn.execute("INSERT OR REPLACE INTO backlog_state (item_id, session_id, state, payload_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![record.item_id, opt_session(record.session_id.as_ref()), record.state, payload, record.updated_at])?;
        Ok(())
    }

    /// Persists one Arbitraitor receipt payload.
    ///
    /// # Errors
    /// Returns [`StoreError`] when serialization or insertion fails.
    pub fn insert_receipt(&self, record: &ReceiptRecord) -> StoreResult<()> {
        let payload = serde_json::to_string(&record.payload)?;
        self.conn.execute("INSERT OR REPLACE INTO arbitraitor_receipts (receipt_id, session_id, receipt_kind, digest, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)", params![record.receipt_id, opt_session(record.session_id.as_ref()), record.receipt_kind, record.digest.as_ref().map(ToString::to_string), payload])?;
        Ok(())
    }

    /// Persists one delegation-chain edge.
    ///
    /// # Errors
    /// Returns [`StoreError`] when serialization or insertion fails.
    pub fn insert_delegation(&self, record: &DelegationRecord) -> StoreResult<()> {
        let payload = serde_json::to_string(&record.payload)?;
        self.conn.execute("INSERT OR REPLACE INTO delegation_chains (chain_id, session_id, parent_op_id, child_op_id, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)", params![record.chain_id, opt_session(record.session_id.as_ref()), record.parent_op_id.as_str(), record.child_op_id.as_str(), payload])?;
        Ok(())
    }

    /// Appends one hash-chained event record.
    ///
    /// # Errors
    /// Returns [`StoreError`] when chain continuity, serialization, or insertion fails.
    pub fn append_event(&self, envelope: EventEnvelope) -> StoreResult<AuditRecord> {
        let last_event_hash = self.last_event_hash()?;
        validate_next_event(
            self.next_event_sequence()?,
            last_event_hash.as_ref(),
            &envelope,
        )?;
        let record = AuditRecord::try_from_envelope(envelope)?;
        let payload = serde_json::to_string(&record)?;
        self.conn.execute(
            EVENT_SQL,
            params![
                u64_to_i64(record.envelope.monotonic_seq)?,
                u64::from(record.envelope.schema_version),
                category_text(record.envelope.category),
                record.envelope.correlation_id.as_str(),
                record
                    .envelope
                    .parent_op_id
                    .as_ref()
                    .map(orchestraitor_model::OperationId::as_str),
                record.envelope.prev_hash.as_ref().map(HashDigest::as_str),
                record.hash.as_str(),
                payload
            ],
        )?;
        Ok(record)
    }

    /// Loads all event records and validates the persisted hash chain.
    ///
    /// # Errors
    /// Returns [`StoreError`] when loading, parsing, or validation fails.
    pub fn load_event_records(&self) -> StoreResult<Vec<AuditRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT record_json FROM event_records ORDER BY monotonic_seq")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        validate_hash_chain(&records)?;
        Ok(records)
    }

    /// Queries event records using the shared event-store filter type.
    ///
    /// # Errors
    /// Returns [`StoreError`] when loading or validating persisted events fails.
    pub fn query_events(&self, query: &EventQuery) -> StoreResult<Vec<AuditRecord>> {
        Ok(self
            .load_event_records()?
            .into_iter()
            .filter(|record| event_matches(record, query))
            .collect())
    }

    /// Returns the next event sequence number.
    ///
    /// # Errors
    /// Returns [`StoreError`] when `SQLite` returns an invalid integer range.
    pub fn next_event_sequence(&self) -> StoreResult<u64> {
        let last =
            self.conn
                .query_row("SELECT MAX(monotonic_seq) FROM event_records", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })?;
        Ok(match last {
            Some(value) => i64_to_u64(value)?.saturating_add(1),
            None => 1,
        })
    }

    /// Returns the current tail hash for the event chain.
    ///
    /// # Errors
    /// Returns [`StoreError`] when `SQLite` query fails.
    pub fn last_event_hash(&self) -> StoreResult<Option<HashDigest>> {
        self.conn
            .query_row(
                "SELECT record_hash FROM event_records ORDER BY monotonic_seq DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|value| value.map(HashDigest))
            .map_err(Into::into)
    }

    /// Executes a raw `SQL` statement against the metadata database.
    ///
    /// Only available to crate-internal tests; production code must go through
    /// the typed CRUD helpers on `DaemonStore` so that invariants (hash-chain
    /// continuity, JSON validation, parameter binding) are preserved.
    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) -> StoreResult<()> {
        self.conn.execute(sql, [])?;
        Ok(())
    }
}

const SESSION_SQL: &str = "INSERT OR REPLACE INTO session_metadata (session_id, repository_id, adapter_id, workspace_id, security_mode, policy_digest, state, created_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
const EVENT_SQL: &str = "INSERT INTO event_records (monotonic_seq, schema_version, category, correlation_id, parent_op_id, prev_hash, record_hash, record_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

fn configure_connection(conn: &Connection) -> StoreResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5_000_u32)?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> StoreResult<()> {
    conn.execute("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')))", [])?;
    let applied = migration_applied(conn, LATEST_SCHEMA_VERSION)?;
    if !applied {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
            params![i64::from(LATEST_SCHEMA_VERSION), "initial-daemon-store"],
        )?;
        tx.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)?;
        tx.commit()?;
    }
    Ok(())
}

fn migration_applied(conn: &Connection, version: u32) -> StoreResult<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        params![i64::from(version)],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn create_parent(path: &Path) -> StoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn validate_next_event(
    expected_seq: u64,
    expected_prev: Option<&HashDigest>,
    envelope: &EventEnvelope,
) -> StoreResult<()> {
    if envelope.monotonic_seq != expected_seq {
        return Err(EventError::SequenceGap {
            expected: expected_seq,
            observed: envelope.monotonic_seq,
        }
        .into());
    }
    if envelope.prev_hash.as_ref() != expected_prev {
        return Err(EventError::PreviousHashMismatch {
            sequence: envelope.monotonic_seq,
        }
        .into());
    }
    Ok(())
}

fn event_matches(record: &AuditRecord, query: &EventQuery) -> bool {
    query
        .category
        .is_none_or(|category| record.envelope.category == category)
        && query
            .since_seq
            .is_none_or(|since| record.envelope.monotonic_seq >= since)
        && query
            .until_seq
            .is_none_or(|until| record.envelope.monotonic_seq <= until)
        && (query.include_uninterpreted
            || record.schema_interpretation()
                == orchestraitor_events::SchemaInterpretation::Interpreted)
}

fn opt_session(session: Option<&SessionId>) -> Option<&str> {
    session.map(SessionId::as_str)
}

fn u64_to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| StoreError::IntegerRange)
}

fn i64_to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| StoreError::IntegerRange)
}

//! Durable state for the §9.47 event ingress (issue #504): the dedup
//! table and the per-endpoint conditional-request bookkeeping.
//!
//! The store is a dedicated `rusqlite` connection (the same pattern the
//! campaign run-state store uses: one owner, synchronous calls, WAL) at
//! `<config-dir>/ingress-events.db` — a separate file from the loop's
//! stores, so ingress bookkeeping never contends with runner writes.
//!
//! **Dedup key.** Delivery is at-least-once on every ingress path (spec
//! §9.47 idempotency): the durable delivery key is the platform delivery
//! identifier (for the poll path, the REST event's `id`) plus repository,
//! event type, and head SHA where present. A redelivery inside the
//! 30-day window (the platform's redelivery/retention horizon) never
//! produces a second accepted event — and therefore never a second
//! kick-off for the same item (§9.26.3). Older rows prune on insert.

use std::path::Path;

use rusqlite::Connection;

use crate::error::DaemonError;

/// The §9.47 redelivery window in seconds: the platform's redelivery and
/// retention horizon (30 days). A delivery key seen within this window
/// deduplicates; outside it, the key may fire again (a genuinely new
/// platform delivery of the same identifier).
pub const REDELIVERY_WINDOW_SECS: u64 = 30 * 24 * 60 * 60;

/// One accepted external event after dedup: identifiers only — never
/// board or PR content (spec §9.47: the queue feeds kick-off evaluation,
/// not planning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressEvent {
    /// The dedup delivery key (platform delivery id + repo + event type
    /// + head SHA where present).
    pub delivery_key: String,
    /// Repository the event occurred on (`owner/name`).
    pub repo: String,
    /// Normalized event type (the operator-filtered set).
    pub event_type: String,
    /// Head SHA for PR events, when the platform supplied one.
    pub head_sha: Option<String>,
    /// Issue or PR number the event is about.
    pub number: u64,
    /// Unix seconds the poller accepted the event.
    pub accepted_at_secs: u64,
}

/// Durable ingress state: the dedup table and the per-endpoint `ETag` /
/// `X-Poll-Interval` bookkeeping for the conditional-request poller.
pub struct IngressStore {
    conn: Connection,
    path_label: String,
}

impl IngressStore {
    /// Opens (creating if absent) the ingress store at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] when the file cannot be
    /// created/opened or the schema cannot initialize.
    pub fn open(path: &Path) -> Result<Self, DaemonError> {
        let label = path.display().to_string();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| DaemonError::IngressStore {
                path: label.clone(),
                reason: format!("parent dir creation failed: {error}"),
            })?;
        }
        let conn = Connection::open(path).map_err(|error| DaemonError::IngressStore {
            path: label.clone(),
            reason: format!("sqlite open failed: {error}"),
        })?;
        Self::init(conn, label)
    }

    /// Opens an in-memory store for tests.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] when initialization fails.
    pub fn open_in_memory() -> Result<Self, DaemonError> {
        let conn = Connection::open_in_memory().map_err(|error| DaemonError::IngressStore {
            path: ":memory:".to_string(),
            reason: format!("sqlite open failed: {error}"),
        })?;
        Self::init(conn, ":memory:".to_string())
    }

    /// Configures the connection and creates the schema.
    fn init(conn: Connection, path_label: String) -> Result<Self, DaemonError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS ingress_deliveries (
                 delivery_key     TEXT PRIMARY KEY,
                 first_seen_secs  INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_ingress_deliveries_seen
                 ON ingress_deliveries(first_seen_secs);
             CREATE TABLE IF NOT EXISTS ingress_etags (
                 endpoint        TEXT PRIMARY KEY,
                 etag            TEXT,
                 poll_after_secs INTEGER NOT NULL DEFAULT 0,
                 not_modified    INTEGER NOT NULL DEFAULT 0
             );",
        )
        .map_err(|error| DaemonError::IngressStore {
            path: path_label.clone(),
            reason: format!("schema init failed: {error}"),
        })?;
        Ok(Self { conn, path_label })
    }

    /// The durable delivery key for one platform delivery (spec §9.47):
    /// platform delivery identifier + repository + event type + head SHA
    /// where present.
    #[must_use]
    pub fn delivery_key(
        delivery_id: &str,
        repo: &str,
        event_type: &str,
        head_sha: Option<&str>,
    ) -> String {
        format!(
            "{delivery_id}|{repo}|{event_type}|{}",
            head_sha.unwrap_or_default()
        )
    }

    /// Dedups one delivery: `Ok(true)` when the key is new inside the
    /// redelivery window (the event must be accepted), `Ok(false)` when
    /// the key was already seen inside the window (a redelivery — never
    /// a second kick-off). The table prunes on every insert, so it never
    /// grows unboundedly.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on sqlite failure.
    pub fn record_delivery(
        &mut self,
        delivery_key: &str,
        now_secs: u64,
    ) -> Result<bool, DaemonError> {
        let now = i64::try_from(now_secs).map_err(|_| DaemonError::IngressStore {
            path: self.path_label.clone(),
            reason: format!("unix seconds out of range: {now_secs}"),
        })?;
        let window =
            i64::try_from(REDELIVERY_WINDOW_SECS).map_err(|_| DaemonError::IngressStore {
                path: self.path_label.clone(),
                reason: "redelivery window out of range".to_string(),
            })?;
        let prune_before = now.saturating_sub(window);
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|error| DaemonError::IngressStore {
                path: self.path_label.clone(),
                reason: format!("transaction begin failed: {error}"),
            })?;
        tx.execute(
            "DELETE FROM ingress_deliveries WHERE first_seen_secs < ?1",
            [prune_before],
        )
        .map_err(|error| DaemonError::IngressStore {
            path: self.path_label.clone(),
            reason: format!("dedup prune failed: {error}"),
        })?;
        let mut stmt = tx
            .prepare(
                "INSERT OR IGNORE INTO ingress_deliveries
                 (delivery_key, first_seen_secs) VALUES (?1, ?2)",
            )
            .map_err(|error| DaemonError::IngressStore {
                path: self.path_label.clone(),
                reason: format!("dedup insert prepare failed: {error}"),
            })?;
        let inserted = stmt
            .execute(rusqlite::params![delivery_key, now])
            .map_err(|error| DaemonError::IngressStore {
                path: self.path_label.clone(),
                reason: format!("dedup insert failed: {error}"),
            })?;
        let accepted = inserted == 1;
        drop(stmt);
        tx.commit().map_err(|error| DaemonError::IngressStore {
            path: self.path_label.clone(),
            reason: format!("dedup commit failed: {error}"),
        })?;
        Ok(accepted)
    }

    /// Whether this delivery key was already recorded INSIDE the
    /// redelivery window (a read-only check that does NOT insert — used
    /// by the poller's pagination stop so a page of already-judged
    /// history ends the chain without re-inserting anything).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on sqlite failure.
    pub fn delivery_seen_within_window(
        &self,
        delivery_key: &str,
        now_secs: u64,
    ) -> Result<bool, DaemonError> {
        let now = i64::try_from(now_secs).map_err(|_| DaemonError::IngressStore {
            path: self.path_label.clone(),
            reason: format!("unix seconds out of range: {now_secs}"),
        })?;
        let window =
            i64::try_from(REDELIVERY_WINDOW_SECS).map_err(|_| DaemonError::IngressStore {
                path: self.path_label.clone(),
                reason: "redelivery window out of range".to_string(),
            })?;
        let cutoff = now.saturating_sub(window);
        let seen: Option<i64> = self
            .conn
            .query_row(
                "SELECT first_seen_secs FROM ingress_deliveries
                 WHERE delivery_key = ?1 AND first_seen_secs >= ?2",
                rusqlite::params![delivery_key, cutoff],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(DaemonError::IngressStore {
                    path: self.path_label.clone(),
                    reason: format!("delivery lookup failed: {other}"),
                }),
            })?;
        Ok(seen.is_some())
    }

    /// Stores the conditional-request bookkeeping for one endpoint: the
    /// last `ETag` and the server-advised minimum wait (`X-Poll-Interval`,
    /// seconds) for the NEXT request. `not_modified` records whether the
    /// last response was a 304 (informational; the `ETag` itself drives
    /// the conditional request).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on sqlite failure.
    pub fn record_conditional(
        &self,
        endpoint: &str,
        etag: Option<&str>,
        poll_after_secs: u64,
        not_modified: bool,
    ) -> Result<(), DaemonError> {
        let poll_after = i64::try_from(poll_after_secs).map_err(|_| DaemonError::IngressStore {
            path: self.path_label.clone(),
            reason: format!("poll interval out of range: {poll_after_secs}"),
        })?;
        self.conn
            .execute(
                "INSERT INTO ingress_etags (endpoint, etag, poll_after_secs, not_modified)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(endpoint) DO UPDATE SET
                     etag = excluded.etag,
                     poll_after_secs = excluded.poll_after_secs,
                     not_modified = excluded.not_modified",
                rusqlite::params![endpoint, etag, poll_after, i64::from(not_modified)],
            )
            .map_err(|error| DaemonError::IngressStore {
                path: self.path_label.clone(),
                reason: format!("etag record failed: {error}"),
            })?;
        Ok(())
    }

    /// Reads the stored `ETag` for one endpoint (`None` = no conditional
    /// bookkeeping yet — the first request carries no `If-None-Match`).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on sqlite failure.
    pub fn etag_for(&self, endpoint: &str) -> Result<Option<String>, DaemonError> {
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT etag FROM ingress_etags WHERE endpoint = ?1",
                [endpoint],
                |row| row.get::<usize, Option<String>>(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(DaemonError::IngressStore {
                    path: self.path_label.clone(),
                    reason: format!("etag read failed: {other}"),
                }),
            })?;
        Ok(stored.flatten())
    }

    /// Reads the server-advised minimum wait (seconds) before the next
    /// poll of one endpoint. Zero when no response advised one yet.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on sqlite failure.
    pub fn poll_after_secs(&self, endpoint: &str) -> Result<u64, DaemonError> {
        let stored: Option<i64> = self
            .conn
            .query_row(
                "SELECT poll_after_secs FROM ingress_etags WHERE endpoint = ?1",
                [endpoint],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(DaemonError::IngressStore {
                    path: self.path_label.clone(),
                    reason: format!("poll-after read failed: {other}"),
                }),
            })?;
        Ok(stored
            .and_then(|secs| u64::try_from(secs).ok())
            .unwrap_or(0))
    }
}

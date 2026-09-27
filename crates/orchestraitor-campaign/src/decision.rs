//! Decision records: the typed outcome of one campaign pass and the
//! append-only `SQLite` store that persists them (one row per pass, never
//! mutated after write).
//!
//! Mirrors the `RoleRoutingDecisionStore` pattern in `orchestraitor-agent-catalog`:
//! `schema_migrations`-versioned, WAL-mode defaults, an in-memory variant for
//! tests. The full decision payload is stored as JSON for replay; the common
//! columns are queryable directly.

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::CampaignError;

/// One open board item that is not eligible under the ready predicate,
/// attached to `AllBlocked` no-op records (the "blocked graph").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedNode {
    /// `org/name` of the issue's repository.
    pub repo: String,
    /// Issue number within `repo`.
    pub number: u64,
    /// Issue title (untrusted text; carried as data, never executed).
    pub title: String,
    /// Number of unresolved `blockedBy` edges on the issue.
    pub open_blockers: u64,
    /// Configured target-field value, when set.
    pub target: Option<String>,
    /// Configured status-field value, when set.
    pub status: Option<String>,
}

/// The board item a `Selected` pass dispatched the worker against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedTask {
    /// `org/name` of the issue's repository.
    pub repo: String,
    /// Issue number within `repo`.
    pub number: u64,
    /// Issue title (untrusted text; carried as data, never executed).
    pub title: String,
    /// Issue URL.
    pub url: String,
    /// `ProjectV2Item` node id for follow-up board operations.
    pub item_node_id: String,
    /// Deterministic worker task id derived from the item.
    pub task_id: String,
}

/// Why a pass did not select anything. The vocabulary is closed (issue #313).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoOpReason {
    /// No open board items exist in the configured repositories.
    EmptyQueue,
    /// Open items exist but none satisfy the ready predicate; the blocked
    /// graph is attached.
    AllBlocked,
    /// Every tracked item is closed — the epic has nothing open left.
    EpicExhausted,
}

/// Whether a pass selected a task or was a typed no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionKind {
    /// A worker was selected and dispatched.
    Selected,
    /// Nothing was runnable; `no_op_reason` carries the typed cause.
    NoOp,
}

/// The full, typed outcome of one campaign pass. Exactly one record per pass
/// is persisted, append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignDecision {
    /// Selected or typed no-op.
    pub kind: DecisionKind,
    /// Set iff `kind` is [`DecisionKind::NoOp`].
    pub no_op_reason: Option<NoOpReason>,
    /// The dispatched item; `None` on no-op passes.
    pub selected: Option<SelectedTask>,
    /// Orchestration role the worker runs as (the implement role in the
    /// bootstrap slice).
    pub role: String,
    /// Resolved provider for the role.
    pub provider: String,
    /// Resolved model for the role.
    pub model: String,
    /// Precedence path that produced the routing resolution.
    pub precedence_path: String,
    /// Documented fallback reason, when the routing fell back.
    pub fallback_reason: Option<String>,
    /// The concrete worker argv the pass planned (or used).
    pub worker_args: Vec<String>,
    /// Deterministic human-readable rationale for the selection or no-op.
    pub rationale: String,
    /// Ready items that were not selected, in P0-first order.
    pub alternatives: Vec<BlockedNode>,
    /// Open items that were not eligible (attached on `AllBlocked`).
    pub blocked_graph: Vec<BlockedNode>,
}

/// A persisted decision row: the payload plus store-assigned identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCampaignDecision {
    /// Monotonic row id (insertion order).
    pub id: i64,
    /// Store-assigned creation timestamp (`SQLite`, `UTC`).
    pub created_at: String,
    /// The decision payload.
    pub decision: CampaignDecision,
}

/// Append-only `SQLite` store for campaign decisions.
#[derive(Debug)]
pub struct CampaignDecisionStore {
    conn: Connection,
    path_label: String,
}

impl CampaignDecisionStore {
    /// Opens (creating parent directories if needed) and migrates the store.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the file cannot be opened or
    /// migrated.
    pub fn open(path: &Path) -> Result<Self, CampaignError> {
        let label = path.display().to_string();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| CampaignError::Store {
                path: label.clone(),
                source: rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(error.to_string()),
                ),
            })?;
        }
        let conn = Connection::open(path).map_err(|source| CampaignError::Store {
            path: label.clone(),
            source,
        })?;
        Self::init(conn, label)
    }

    /// Opens an in-memory store for tests and short-lived callers.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when initialization fails.
    pub fn open_in_memory() -> Result<Self, CampaignError> {
        let conn = Connection::open_in_memory().map_err(|source| CampaignError::Store {
            path: ":memory:".to_string(),
            source,
        })?;
        Self::init(conn, ":memory:".to_string())
    }

    fn init(conn: Connection, path_label: String) -> Result<Self, CampaignError> {
        configure(&conn).map_err(|source| CampaignError::Store {
            path: path_label.clone(),
            source,
        })?;
        migrate(&conn).map_err(|source| CampaignError::Store {
            path: path_label.clone(),
            source,
        })?;
        Ok(Self { conn, path_label })
    }

    /// Appends one decision row and returns the stored record (read-back).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when serialization, insert, or
    /// read-back fails.
    pub fn record(
        &self,
        decision: &CampaignDecision,
    ) -> Result<StoredCampaignDecision, CampaignError> {
        let payload = serialize(decision, &self.path_label)?;
        let kind = serialize(&decision.kind, &self.path_label)?;
        let no_op_reason = decision
            .no_op_reason
            .map(|reason| serialize(&reason, &self.path_label))
            .transpose()?;
        let selected_ref = decision.selected.as_ref();
        let selected_repo = selected_ref.map(|task| task.repo.as_str());
        let selected_number = match selected_ref {
            Some(task) => Some(number_to_i64(task.number, &self.path_label)?),
            None => None,
        };
        self.conn
            .execute(
                "INSERT INTO campaign_decisions
                 (kind, no_op_reason, selected_repo, selected_number, role,
                  provider, model, rationale, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    kind,
                    no_op_reason,
                    selected_repo,
                    selected_number,
                    decision.role,
                    decision.provider,
                    decision.model,
                    decision.rationale,
                    payload,
                ],
            )
            .map_err(|source| CampaignError::Store {
                path: self.path_label.clone(),
                source,
            })?;
        self.by_id(self.conn.last_insert_rowid())
    }

    /// Reads one stored record by row id.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails or the id is
    /// unknown.
    pub fn by_id(&self, id: i64) -> Result<StoredCampaignDecision, CampaignError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, created_at, payload FROM campaign_decisions WHERE id = ?1")
            .map_err(|source| self.err(source))?;
        let mut rows = statement
            .query(rusqlite::params![id])
            .map_err(|source| self.err(source))?;
        let row = rows.next().map_err(|source| self.err(source))?;
        decode_row(
            row.ok_or_else(|| CampaignError::Store {
                path: self.path_label.clone(),
                source: rusqlite::Error::QueryReturnedNoRows,
            })?,
            &self.path_label,
        )
    }

    /// Lists every stored decision in insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn list(&self) -> Result<Vec<StoredCampaignDecision>, CampaignError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, created_at, payload FROM campaign_decisions ORDER BY id")
            .map_err(|source| self.err(source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|source| self.err(source))?;
        let mut records = Vec::new();
        for row in rows {
            let (id, created_at, payload) = row.map_err(|source| self.err(source))?;
            records.push(decode_payload(id, created_at, &payload, &self.path_label)?);
        }
        Ok(records)
    }

    fn err(&self, source: rusqlite::Error) -> CampaignError {
        CampaignError::Store {
            path: self.path_label.clone(),
            source,
        }
    }
}

fn serialize<T: serde::Serialize>(value: &T, path_label: &str) -> Result<String, CampaignError> {
    serde_json::to_string(value).map_err(|source| CampaignError::Store {
        path: path_label.to_string(),
        source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
    })
}

/// GitHub issue numbers fit far inside `i64`; the conversion exists so the
/// `SQLite` column stays typed and a pathological value fails loudly.
fn number_to_i64(number: u64, path_label: &str) -> Result<i64, CampaignError> {
    i64::try_from(number).map_err(|source| CampaignError::Store {
        path: path_label.to_string(),
        source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
    })
}

fn decode_row(
    row: &rusqlite::Row<'_>,
    path_label: &str,
) -> Result<StoredCampaignDecision, CampaignError> {
    let id: i64 = row.get(0).map_err(|source| CampaignError::Store {
        path: path_label.to_string(),
        source,
    })?;
    let created_at: String = row.get(1).map_err(|source| CampaignError::Store {
        path: path_label.to_string(),
        source,
    })?;
    let payload: String = row.get(2).map_err(|source| CampaignError::Store {
        path: path_label.to_string(),
        source,
    })?;
    decode_payload(id, created_at, &payload, path_label)
}

fn decode_payload(
    id: i64,
    created_at: String,
    payload: &str,
    path_label: &str,
) -> Result<StoredCampaignDecision, CampaignError> {
    let decision: CampaignDecision =
        serde_json::from_str(payload).map_err(|source| CampaignError::Store {
            path: path_label.to_string(),
            source: rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(source),
            ),
        })?;
    Ok(StoredCampaignDecision {
        id,
        created_at,
        decision,
    })
}

fn configure(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
}

fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );
         CREATE TABLE IF NOT EXISTS campaign_decisions (
             id              INTEGER PRIMARY KEY,
             kind            TEXT NOT NULL,
             no_op_reason    TEXT,
             selected_repo   TEXT,
             selected_number INTEGER,
             role            TEXT NOT NULL,
             provider        TEXT NOT NULL,
             model           TEXT NOT NULL,
             rationale       TEXT NOT NULL,
             payload         TEXT NOT NULL,
             created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );
         INSERT OR IGNORE INTO schema_migrations (version, name)
         VALUES (1, 'campaign_decisions_v1');",
    )
}

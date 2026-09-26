//! `SQLite`-backed persistence for role routing decision records
//! (spec `30-model-routing.md` §9.45, "Routing decision records"; storage via
//! `rusqlite` with WAL mode per tech-stack §11). The schema is versioned with
//! the same `schema_migrations` migration pattern as the daemon store so E2
//! additions (alternatives considered, per-alternative skip reasons, decision
//! provider confidence) land as `SCHEMA_V2+` migrations, never table rewrites.

use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::role_routing::RoleRoutingDecision;
use crate::{AgentCatalogError, AgentCatalogResult};

/// Latest decision-store schema version.
pub const LATEST_SCHEMA_VERSION: u32 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS role_routing_decisions (
  id INTEGER PRIMARY KEY,
  role TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  precedence_path TEXT NOT NULL,
  fallback_reason TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_role_routing_decisions_role ON role_routing_decisions(role);
";

/// A decision record persisted in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRoleRoutingDecision {
    /// Store-assigned monotonically increasing row id.
    pub id: i64,
    /// Orchestration role id that was resolved.
    pub role: String,
    /// Resolved provider identifier.
    pub provider: String,
    /// Resolved model identifier.
    pub model: String,
    /// Precedence path that produced the resolution.
    pub precedence_path: String,
    /// Documented fallback reason; `None` when a table entry matched directly.
    pub fallback_reason: Option<String>,
    /// RFC 3339 UTC timestamp assigned at insert time.
    pub created_at: String,
}

/// `SQLite` store for role routing decision records.
pub struct RoleRoutingDecisionStore {
    conn: Connection,
}

impl RoleRoutingDecisionStore {
    /// Opens the store at `path`, initializes WAL mode, and runs migrations.
    ///
    /// # Errors
    /// Returns [`AgentCatalogError`] when the database cannot be created,
    /// opened, configured, or migrated.
    pub fn open(path: &Path) -> AgentCatalogResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        configure_connection(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Opens an in-memory store for tests and short-lived callers.
    ///
    /// # Errors
    /// Returns [`AgentCatalogError`] when the in-memory database cannot be
    /// initialized or migrated.
    pub fn open_in_memory() -> AgentCatalogResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Persists one decision record and returns the stored row.
    ///
    /// # Errors
    /// Returns [`AgentCatalogError`] when insertion or read-back fails.
    pub fn record(
        &self,
        decision: &RoleRoutingDecision,
    ) -> AgentCatalogResult<StoredRoleRoutingDecision> {
        self.conn.execute(
            "INSERT INTO role_routing_decisions (role, provider, model, precedence_path, fallback_reason) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                decision.role,
                decision.provider,
                decision.model,
                decision.precedence_path,
                decision.fallback_reason,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.by_id(id)?
            .ok_or_else(|| AgentCatalogError::DecisionStoreReadback { id })
    }

    /// Lists every stored decision record in insertion order.
    ///
    /// # Errors
    /// Returns [`AgentCatalogError`] when the query fails.
    pub fn list(&self) -> AgentCatalogResult<Vec<StoredRoleRoutingDecision>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, provider, model, precedence_path, fallback_reason, created_at FROM role_routing_decisions ORDER BY id",
        )?;
        let rows = stmt.query_map([], row_to_decision)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Lists stored decision records for one role in insertion order.
    ///
    /// # Errors
    /// Returns [`AgentCatalogError`] when the query fails.
    pub fn for_role(&self, role: &str) -> AgentCatalogResult<Vec<StoredRoleRoutingDecision>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, provider, model, precedence_path, fallback_reason, created_at FROM role_routing_decisions WHERE role = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![role], row_to_decision)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn by_id(&self, id: i64) -> AgentCatalogResult<Option<StoredRoleRoutingDecision>> {
        self.conn
            .query_row(
                "SELECT id, role, provider, model, precedence_path, fallback_reason, created_at FROM role_routing_decisions WHERE id = ?1",
                params![id],
                row_to_decision,
            )
            .optional()
            .map_err(Into::into)
    }
}

fn row_to_decision(row: &rusqlite::Row<'_>) -> Result<StoredRoleRoutingDecision, rusqlite::Error> {
    Ok(StoredRoleRoutingDecision {
        id: row.get(0)?,
        role: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        precedence_path: row.get(4)?,
        fallback_reason: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn configure_connection(conn: &Connection) -> AgentCatalogResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5_000_u32)?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> AgentCatalogResult<()> {
    if schema_version_applied(conn, LATEST_SCHEMA_VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute_batch(SCHEMA_V1)?;
    record_schema_version(&tx, LATEST_SCHEMA_VERSION, "initial-role-routing-decisions")?;
    tx.commit()?;
    Ok(())
}

fn migrations_table(conn: &Connection) -> AgentCatalogResult<()> {
    conn.execute("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')))", [])?;
    Ok(())
}

fn schema_version_applied(conn: &Connection, version: u32) -> AgentCatalogResult<bool> {
    migrations_table(conn)?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        params![i64::from(version)],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn record_schema_version(conn: &Connection, version: u32, name: &str) -> AgentCatalogResult<()> {
    conn.execute(
        "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
        params![i64::from(version), name],
    )?;
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

//! The loop run-state store: one row per worker run a loop invocation
//! started, with mutable supervision columns (heartbeat, terminal status).
//!
//! This is the durable surface the `orc loop` guards assert against
//! (issue #314): the concurrency cap reads `active()`, the never-silent-retry
//! exclusion reads `runs_for_task()`, and the daily spend soft cap reads
//! `daily_spend()`. A crashed loop leaves `running` rows behind; the next
//! invocation reconciles them to `aborted-crash` via [`LoopRunStore::reconcile_stale`]
//! before enforcing any guard (a stale row would otherwise consume a
//! concurrency slot forever).
//!
//! Rows reference the append-only campaign decision store by plain integer
//! id (`decision_id`); the two stores live in separate files, so there is no
//! cross-file foreign key — the id is the audit link only. All time inputs
//! are explicit `now_secs: u64` Unix seconds: no clock is read inside this
//! module, keeping every guard deterministic under a virtual clock.

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::CampaignError;

/// Lifecycle status of one supervised worker run. The terminal states are
/// mutually exclusive and always carry a `detail` reason; a row is mutated
/// at most once from `running` to a terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunRowStatus {
    /// Spawned, supervised, not yet terminal.
    Running,
    /// The worker run returned a completed result.
    Completed,
    /// The worker run returned a typed failure.
    Failed,
    /// The supervisor killed the run: no beat within the stall timeout.
    Stalled,
    /// The supervisor killed the run: the worker timeout elapsed (beats
    /// may have kept arriving; the run still overstayed its bound).
    TimedOut,
    /// The supervisor killed the run during a graceful shutdown or
    /// run-budget stop (the abort intent distinguishes the reason in
    /// `detail`; the row status is shared).
    AbortedShutdown,
    /// Startup reconciliation of a `running` row left behind by a crashed
    /// previous invocation.
    AbortedCrash,
}

impl RunRowStatus {
    /// Whether the status is terminal (no further mutation allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// Canonical store spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stalled => "stalled",
            Self::TimedOut => "timed-out",
            Self::AbortedShutdown => "aborted-shutdown",
            Self::AbortedCrash => "aborted-crash",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "stalled" => Some(Self::Stalled),
            "timed-out" => Some(Self::TimedOut),
            "aborted-shutdown" => Some(Self::AbortedShutdown),
            "aborted-crash" => Some(Self::AbortedCrash),
            _ => None,
        }
    }
}

/// One supervised worker run: the durable row the loop guards read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    /// Monotonic row id.
    pub id: i64,
    /// Loop invocation that started the run (the exclusion and reconciliation
    /// scopes are per-invocation).
    pub invocation_id: String,
    /// Campaign decision record id that selected the task.
    pub decision_id: i64,
    /// Deterministic worker task id (see `task_id_for`).
    pub task_id: String,
    /// `org/name` of the selected item's repository.
    pub repo: String,
    /// Issue number within `repo`.
    pub number: u64,
    /// Current lifecycle status.
    pub status: RunRowStatus,
    /// Unix seconds when the run started.
    pub started_at_secs: u64,
    /// Highest beat sequence observed on the progress channel.
    pub heartbeat_turn: u64,
    /// Unix seconds of the last observed beat.
    pub heartbeat_at_secs: u64,
    /// Unix seconds when the run reached a terminal status; `None` while
    /// running.
    pub finished_at_secs: Option<u64>,
    /// Estimated spend recorded at termination (tokens × per-token
    /// estimate; `0.0` accrues nothing — the documented estimate-disabled
    /// default).
    pub spend_usd: f64,
    /// Terminal detail (typed status is in `status`; the free-form reason is
    /// store-assigned, log-safe, and never contains task or board content).
    pub detail: String,
}

/// The inputs to [`LoopRunStore::start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRun {
    /// Loop invocation identity.
    pub invocation_id: String,
    /// Campaign decision record id that selected the task.
    pub decision_id: i64,
    /// Deterministic worker task id.
    pub task_id: String,
    /// `org/name` of the selected item's repository.
    pub repo: String,
    /// Issue number within `repo`.
    pub number: u64,
    /// Unix seconds when the run started.
    pub started_at_secs: u64,
}

/// `SQLite` store for loop worker-run state (`<config-dir>/loop.db`).
#[derive(Debug)]
pub struct LoopRunStore {
    conn: Connection,
    path_label: String,
}

const RUN_COLUMNS: &str = "id, invocation_id, decision_id, task_id, repo, number, status, \
     started_at_secs, heartbeat_turn, heartbeat_at_secs, finished_at_secs, spend_usd, detail";

impl LoopRunStore {
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

    /// Inserts a `running` row for a freshly spawned worker run.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the insert or read-back fails.
    pub fn start(&self, request: &StartRun) -> Result<RunRow, CampaignError> {
        self.conn
            .execute(
                "INSERT INTO loop_worker_runs
                 (invocation_id, decision_id, task_id, repo, number, status,
                  started_at_secs, heartbeat_turn, heartbeat_at_secs)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, 0, ?6)",
                rusqlite::params![
                    request.invocation_id,
                    request.decision_id,
                    request.task_id,
                    request.repo,
                    number_i64(request.number, &self.path_label)?,
                    secs_i64(request.started_at_secs, &self.path_label)?,
                ],
            )
            .map_err(|source| self.err(source))?;
        self.by_id(self.conn.last_insert_rowid())
    }

    /// Records an observed progress beat on a running row. Terminal rows are
    /// rejected (a beat after termination is a supervision bug).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the update fails or the row is
    /// not in the `running` status.
    pub fn heartbeat(&self, run_id: i64, beat: u64, now_secs: u64) -> Result<(), CampaignError> {
        let changed = self
            .conn
            .execute(
                "UPDATE loop_worker_runs
                 SET heartbeat_turn = ?2, heartbeat_at_secs = ?3
                 WHERE id = ?1 AND status = 'running'",
                rusqlite::params![
                    run_id,
                    secs_i64(beat, &self.path_label)?,
                    secs_i64(now_secs, &self.path_label)?,
                ],
            )
            .map_err(|source| self.err(source))?;
        if changed == 0 {
            return Err(self.no_such_running_row());
        }
        Ok(())
    }

    /// Transitions a running row to a terminal status, stamping the finish
    /// time, the recorded spend, and the abort reason. Transitioning an
    /// already-terminal row is a supervision bug and is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the update fails or the row is
    /// not in the `running` status.
    pub fn finish(
        &self,
        run_id: i64,
        status: RunRowStatus,
        now_secs: u64,
        spend_usd: f64,
        detail: &str,
    ) -> Result<(), CampaignError> {
        if !status.is_terminal() {
            return Err(CampaignError::Store {
                path: self.path_label.clone(),
                source: rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "finish requires a terminal status",
                ))),
            });
        }
        let changed = self
            .conn
            .execute(
                "UPDATE loop_worker_runs
                 SET status = ?2, finished_at_secs = ?3, spend_usd = ?4, detail = ?5
                 WHERE id = ?1 AND status = 'running'",
                rusqlite::params![
                    run_id,
                    status.as_str(),
                    secs_i64(now_secs, &self.path_label)?,
                    spend_usd,
                    detail,
                ],
            )
            .map_err(|source| self.err(source))?;
        if changed == 0 {
            return Err(self.no_such_running_row());
        }
        Ok(())
    }

    /// Reads one row by id.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails or the id is
    /// unknown.
    pub fn by_id(&self, run_id: i64) -> Result<RunRow, CampaignError> {
        let sql = format!("SELECT {RUN_COLUMNS} FROM loop_worker_runs WHERE id = ?1");
        let mut statement = self.conn.prepare(&sql).map_err(|source| self.err(source))?;
        let mut rows = statement
            .query(rusqlite::params![run_id])
            .map_err(|source| self.err(source))?;
        let row = rows
            .next()
            .map_err(|source| self.err(source))?
            .ok_or_else(|| CampaignError::Store {
                path: self.path_label.clone(),
                source: rusqlite::Error::QueryReturnedNoRows,
            })?;
        decode_row(row).map_err(|source| self.err(source))
    }

    /// Lists rows currently in the `running` status — the concurrency-slot
    /// holders. Callers must reconcile stale invocations first.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn active(&self) -> Result<Vec<RunRow>, CampaignError> {
        self.list_where("status = 'running'")
    }

    /// Lists every row for a task id (insertion order) — the
    /// never-silent-retry exclusion surface.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn runs_for_task(&self, task_id: &str) -> Result<Vec<RunRow>, CampaignError> {
        self.list_where_task(task_id)
    }

    /// Lists every row of one loop invocation (insertion order) — the
    /// per-invocation exclusion scan.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn runs_for_invocation(&self, invocation_id: &str) -> Result<Vec<RunRow>, CampaignError> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM loop_worker_runs WHERE invocation_id = ?1 ORDER BY id"
        );
        self.query_rows(&sql, rusqlite::params![invocation_id])
    }

    /// Sums the recorded spend of runs that STARTED on the UTC day of
    /// `now_secs` (the spend soft cap is daily; the day boundary is derived
    /// from the explicit clock parameter, never a real clock).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn daily_spend(&self, now_secs: u64) -> Result<f64, CampaignError> {
        let day_start = secs_i64(now_secs - (now_secs % 86_400), &self.path_label)?;
        let day_end = day_start + 86_400;
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(spend_usd), 0.0) FROM loop_worker_runs
                 WHERE started_at_secs >= ?1 AND started_at_secs < ?2",
                rusqlite::params![day_start, day_end],
                |row| row.get::<_, f64>(0),
            )
            .map_err(|source| self.err(source))
    }

    /// Sweeps `running` rows that do not belong to `invocation_id` to
    /// `aborted-crash` and returns how many rows were reconciled. Must be
    /// called at loop start, before any guard reads `active()`: a crashed
    /// previous invocation's rows would otherwise occupy concurrency slots
    /// forever.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the update fails.
    pub fn reconcile_stale(
        &self,
        invocation_id: &str,
        now_secs: u64,
    ) -> Result<u64, CampaignError> {
        let changed = self
            .conn
            .execute(
                "UPDATE loop_worker_runs
                 SET status = 'aborted-crash', finished_at_secs = ?2,
                     detail = 'stale-running-row-from-crashed-invocation'
                 WHERE status = 'running' AND invocation_id != ?1",
                rusqlite::params![invocation_id, secs_i64(now_secs, &self.path_label)?],
            )
            .map_err(|source| self.err(source))?;
        u64::try_from(changed).map_err(|source| bigint(&self.path_label, source))
    }

    fn list_where(&self, predicate: &str) -> Result<Vec<RunRow>, CampaignError> {
        let sql =
            format!("SELECT {RUN_COLUMNS} FROM loop_worker_runs WHERE {predicate} ORDER BY id");
        self.query_rows(&sql, rusqlite::params![])
    }

    fn list_where_task(&self, task_id: &str) -> Result<Vec<RunRow>, CampaignError> {
        let sql =
            format!("SELECT {RUN_COLUMNS} FROM loop_worker_runs WHERE task_id = ?1 ORDER BY id");
        self.query_rows(&sql, rusqlite::params![task_id])
    }

    fn query_rows(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<RunRow>, CampaignError> {
        let mut statement = self.conn.prepare(sql).map_err(|source| self.err(source))?;
        let rows = statement
            .query_map(params, decode_row)
            .map_err(|source| self.err(source))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|source| self.err(source))?);
        }
        Ok(records)
    }

    fn no_such_running_row(&self) -> CampaignError {
        CampaignError::Store {
            path: self.path_label.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        }
    }

    fn err(&self, source: rusqlite::Error) -> CampaignError {
        CampaignError::Store {
            path: self.path_label.clone(),
            source,
        }
    }
}

fn decode_row(row: &rusqlite::Row<'_>) -> Result<RunRow, rusqlite::Error> {
    let status_text: String = row.get(6)?;
    let status = RunRowStatus::parse(&status_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown loop run status `{status_text}`"),
            )),
        )
    })?;
    let number = row_u64(row.get::<_, i64>(5)?, 5)?;
    let started_at_secs = row_u64(row.get::<_, i64>(7)?, 7)?;
    let heartbeat_turn = row_u64(row.get::<_, i64>(8)?, 8)?;
    let heartbeat_at_secs = row_u64(row.get::<_, i64>(9)?, 9)?;
    let finished_at_secs = row
        .get::<_, Option<i64>>(10)?
        .map(|secs| row_u64(secs, 10))
        .transpose()?;
    Ok(RunRow {
        id: row.get(0)?,
        invocation_id: row.get(1)?,
        decision_id: row.get(2)?,
        task_id: row.get(3)?,
        repo: row.get(4)?,
        number,
        status,
        started_at_secs,
        heartbeat_turn,
        heartbeat_at_secs,
        finished_at_secs,
        spend_usd: row.get(11)?,
        detail: row.get(12)?,
    })
}

fn row_u64(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "negative integer in an unsigned column",
            )),
        )
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
         CREATE TABLE IF NOT EXISTS loop_worker_runs (
             id                INTEGER PRIMARY KEY,
             invocation_id     TEXT NOT NULL,
             decision_id       INTEGER NOT NULL,
             task_id           TEXT NOT NULL,
             repo              TEXT NOT NULL,
             number            INTEGER NOT NULL,
             status            TEXT NOT NULL,
             started_at_secs   INTEGER NOT NULL,
             heartbeat_turn    INTEGER NOT NULL DEFAULT 0,
             heartbeat_at_secs INTEGER NOT NULL,
             finished_at_secs  INTEGER,
             spend_usd         REAL NOT NULL DEFAULT 0.0,
             detail            TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX IF NOT EXISTS idx_loop_runs_task ON loop_worker_runs (task_id);
         CREATE INDEX IF NOT EXISTS idx_loop_runs_status ON loop_worker_runs (status);
         INSERT OR IGNORE INTO schema_migrations (version, name)
         VALUES (1, 'loop_worker_runs_v1');",
    )
}

fn secs_i64(value: u64, path_label: &str) -> Result<i64, CampaignError> {
    i64::try_from(value).map_err(|source| bigint(path_label, source))
}

fn number_i64(value: u64, path_label: &str) -> Result<i64, CampaignError> {
    i64::try_from(value).map_err(|source| bigint(path_label, source))
}

fn bigint(path_label: &str, source: std::num::TryFromIntError) -> CampaignError {
    CampaignError::Store {
        path: path_label.to_string(),
        source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
    }
}

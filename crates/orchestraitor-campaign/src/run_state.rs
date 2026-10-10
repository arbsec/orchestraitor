//! The loop run-state store: one row per worker run a loop invocation
//! started, with mutable supervision columns (heartbeat, terminal status).
//!
//! The `orc loop` guards read this store against the in-memory supervision
//! state they enforce (issue #314): the concurrency cap counts supervised
//! slots in memory (`loop_run.rs`), the never-silent-retry exclusion reads
//! `runs_for_invocation()` (this invocation's rows are the audit trail a
//! re-selection would contradict), and the daily spend soft cap reads
//! `daily_spend()`. The heartbeat columns are the durable liveness record:
//! every beat the supervisor observes is persisted via
//! [`LoopRunStore::heartbeat`], so `loop.db` shows how far each run got
//! even after a crash or kill. Rows from earlier invocations are historical
//! records; the watch daemon's startup performs restart recovery (spec
//! §9.24.2, issue #503): every row still `running` transitions to
//! [`RunRowStatus::Orphaned`] — never directly `failed` — and a `paused`
//! row stays paused.
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
    /// The task hit its cross-invocation retry budget or its backoff window
    /// (anti-stuck guard, spec §9.24.1): never silently re-selected — the
    /// typed rows and the skip reason in the decision record are the
    /// needs-human signal.
    Stuck,
    /// The supervisor died (crash, kill -9) with the row still `running`;
    /// restart recovery transitioned it per §9.24.2. Never a direct
    /// `failed`: the next tick may re-select the work fresh.
    Orphaned,
    /// Explicit operator pause (§9.24.1 `paused`; the pause control itself
    /// is a later slice). Non-terminal and lease-protected: restart
    /// recovery leaves a paused row exactly where it is (§9.24.2 —
    /// `paused` stays paused), and it never transitions to `orphaned`.
    Paused,
}

impl RunRowStatus {
    /// Whether the status is terminal (no further mutation allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running | Self::Paused)
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
            Self::Stuck => "stuck",
            Self::Orphaned => "orphaned",
            Self::Paused => "paused",
        }
    }

    /// Decodes a stored lifecycle status, rejecting unknown spellings.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "stalled" => Some(Self::Stalled),
            "timed-out" => Some(Self::TimedOut),
            "aborted-shutdown" => Some(Self::AbortedShutdown),
            "stuck" => Some(Self::Stuck),
            "orphaned" => Some(Self::Orphaned),
            "paused" => Some(Self::Paused),
            _ => None,
        }
    }
}

/// One supervised worker run: the durable row the loop guards read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    /// Monotonic row id.
    pub id: i64,
    /// Loop invocation that started the run (the exclusion scope is per-invocation).
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

/// Durable per-task retry state (the cross-invocation anti-stuck record).
/// One row per task id, mutated as attempts fail and re-selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRetryState {
    /// Deterministic worker task id.
    pub task_id: String,
    /// Total attempts across every invocation.
    pub attempts_total: u64,
    /// The last typed failure class recorded for the task (kebab-case
    /// spelling of the worker `FailureClass`, or a loop-side code).
    pub last_failure_class: Option<String>,
    /// Consecutive no-progress attempts (reset on any progress or success).
    pub consecutive_no_progress: u64,
    /// Unix seconds until which the task must not be re-selected
    /// (`None` = selectable now).
    pub backoff_until_secs: Option<u64>,
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

    /// Configures the connection and creates the run-state schema.
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

    /// Lists every row currently in the `running` status (insertion order) —
    /// the reconcile and recovery scans.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn running_rows(&self) -> Result<Vec<RunRow>, CampaignError> {
        self.query_rows(
            &format!(
                "SELECT {RUN_COLUMNS} FROM loop_worker_runs WHERE status = 'running' ORDER BY id"
            ),
            [],
        )
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
        let day_end = day_start.saturating_add(86_400);
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(spend_usd), 0.0) FROM loop_worker_runs
                 WHERE started_at_secs >= ?1 AND started_at_secs < ?2",
                rusqlite::params![day_start, day_end],
                |row| row.get::<_, f64>(0),
            )
            .map_err(|source| self.err(source))
    }

    /// Restart recovery (spec §9.24.2, issue #503): transitions every row
    /// still in the `running` status to `orphaned` — never directly
    /// `failed` — and returns the recovered rows. A crashed or killed
    /// supervisor leaves `running` rows behind; a fresh daemon startup
    /// reconciles them before the first tick so no row can outlive its
    /// process. A `paused` row stays paused (§9.24.2: `paused` tasks stay
    /// paused across a restart) and is never touched here. Idempotent: a
    /// second call over recovered rows returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the update or the row decode
    /// fails. Each row is transitioned and decoded individually, so the
    /// rows recovered before a failure are already durable.
    pub fn recover_running_rows(&self, now_secs: u64) -> Result<Vec<RunRow>, CampaignError> {
        let mut recovered = Vec::new();
        for row in self.running_rows()? {
            self.finish(
                row.id,
                RunRowStatus::Orphaned,
                now_secs,
                0.0,
                "restart-recovery: supervisor did not reach a terminal status",
            )?;
            recovered.push(self.by_id(row.id)?);
        }
        Ok(recovered)
    }

    /// Reads the durable retry state for one task; `None` when the task has
    /// never been attempted.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn task_retry_state(&self, task_id: &str) -> Result<Option<TaskRetryState>, CampaignError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT task_id, attempts_total, last_failure_class, \
                 consecutive_no_progress, backoff_until_secs \
                 FROM task_retry_state WHERE task_id = ?1",
            )
            .map_err(|source| self.err(source))?;
        let mut rows = statement
            .query(rusqlite::params![task_id])
            .map_err(|source| self.err(source))?;
        let Some(row) = rows.next().map_err(|source| self.err(source))? else {
            return Ok(None);
        };
        let backoff_until_secs = row
            .get::<_, Option<i64>>(4)
            .map_err(|source| self.err(source))?
            .map(|secs| row_u64(secs, 4).map_err(|source| self.err(source)))
            .transpose()?;
        Ok(Some(TaskRetryState {
            task_id: row.get(0).map_err(|source| self.err(source))?,
            attempts_total: row_u64(row.get::<_, i64>(1).map_err(|source| self.err(source))?, 1)
                .map_err(|source| self.err(source))?,
            last_failure_class: row.get(2).map_err(|source| self.err(source))?,
            consecutive_no_progress: row_u64(
                row.get::<_, i64>(3).map_err(|source| self.err(source))?,
                3,
            )
            .map_err(|source| self.err(source))?,
            backoff_until_secs,
        }))
    }

    /// Upserts one task's durable retry state (the failure bookkeeping the
    /// cross-invocation budget reads).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the write fails.
    pub fn record_task_failure(
        &self,
        task_id: &str,
        failure_class: &str,
        no_progress: bool,
        backoff_until_secs: Option<u64>,
    ) -> Result<(), CampaignError> {
        self.conn
            .execute(
                "INSERT INTO task_retry_state
                 (task_id, attempts_total, last_failure_class,
                  consecutive_no_progress, backoff_until_secs)
                 VALUES (?1, 1, ?2, ?3, ?4)
                 ON CONFLICT(task_id) DO UPDATE SET
                   attempts_total = attempts_total + 1,
                   last_failure_class = excluded.last_failure_class,
                   consecutive_no_progress = CASE WHEN excluded.consecutive_no_progress > 0
                       THEN consecutive_no_progress + 1 ELSE 0 END,
                   backoff_until_secs = excluded.backoff_until_secs",
                rusqlite::params![
                    task_id,
                    failure_class,
                    i64::from(no_progress),
                    match backoff_until_secs {
                        Some(secs) => Some(secs_i64(secs, &self.path_label)?),
                        None => None,
                    }
                ],
            )
            .map_err(|source| self.err(source))?;
        Ok(())
    }

    /// Clears a task's failure streak (called on a completed run).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the write fails.
    pub fn record_task_success(&self, task_id: &str) -> Result<(), CampaignError> {
        self.conn
            .execute(
                "UPDATE task_retry_state SET consecutive_no_progress = 0,
                 backoff_until_secs = NULL WHERE task_id = ?1",
                rusqlite::params![task_id],
            )
            .map_err(|source| self.err(source))?;
        Ok(())
    }

    /// Reads every task's durable retry state (the exclusion scan input).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Store`] when the query fails.
    pub fn all_task_retry_states(&self) -> Result<Vec<TaskRetryState>, CampaignError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT task_id, attempts_total, last_failure_class, \
                 consecutive_no_progress, backoff_until_secs \
                 FROM task_retry_state ORDER BY task_id",
            )
            .map_err(|source| self.err(source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(|source| self.err(source))?;
        let mut states = Vec::new();
        for row in rows {
            let (task_id, attempts, class, streak, backoff) =
                row.map_err(|source| self.err(source))?;
            states.push(TaskRetryState {
                task_id,
                attempts_total: row_u64(attempts, 1).map_err(|source| self.err(source))?,
                last_failure_class: class,
                consecutive_no_progress: row_u64(streak, 3).map_err(|source| self.err(source))?,
                backoff_until_secs: backoff
                    .map(|secs| row_u64(secs, 4).map_err(|source| self.err(source)))
                    .transpose()?,
            });
        }
        Ok(states)
    }

    /// Reads run rows with bound parameters and propagates decoding failures.
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

    /// Reports a missing or already-terminal run to reject invalid updates.
    fn no_such_running_row(&self) -> CampaignError {
        CampaignError::Store {
            path: self.path_label.clone(),
            source: rusqlite::Error::QueryReturnedNoRows,
        }
    }

    /// Attaches this store's path to a database error.
    fn err(&self, source: rusqlite::Error) -> CampaignError {
        CampaignError::Store {
            path: self.path_label.clone(),
            source,
        }
    }
}

/// Decodes the canonical column order, validating statuses and unsigned values.
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

/// Converts a stored integer to an unsigned value, rejecting negative data.
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

/// Enables WAL journaling, normal synchronization, and foreign keys.
fn configure(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
}

/// Creates the initial run-state tables and indexes if absent.
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
         CREATE TABLE IF NOT EXISTS task_retry_state (
             task_id                 TEXT PRIMARY KEY,
             attempts_total          INTEGER NOT NULL DEFAULT 0,
             last_failure_class      TEXT,
             consecutive_no_progress INTEGER NOT NULL DEFAULT 0,
             backoff_until_secs      INTEGER
         );
         INSERT OR IGNORE INTO schema_migrations (version, name)
         VALUES (1, 'loop_worker_runs_v1');
         INSERT OR IGNORE INTO schema_migrations (version, name)
         VALUES (2, 'task_retry_state_v1');",
    )
}

/// Converts a clock or beat value to `SQLite`'s signed integer range.
fn secs_i64(value: u64, path_label: &str) -> Result<i64, CampaignError> {
    i64::try_from(value).map_err(|source| bigint(path_label, source))
}

/// Converts an issue number to `SQLite`'s signed integer range.
fn number_i64(value: u64, path_label: &str) -> Result<i64, CampaignError> {
    i64::try_from(value).map_err(|source| bigint(path_label, source))
}

/// Wraps an integer overflow as a store conversion error with its path.
fn bigint(path_label: &str, source: std::num::TryFromIntError) -> CampaignError {
    CampaignError::Store {
        path: path_label.to_string(),
        source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
    }
}

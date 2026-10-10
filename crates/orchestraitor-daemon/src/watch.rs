//! `orcd watch` — the daemon's running mode (spec `10-orchestrator.md`
//! §9.36 thin slice, issue #503): the port of `orc loop`'s poll/supervise
//! cycle into the always-running daemon.
//!
//! One instance: the daemon takes over single-flight ownership of the
//! loop's instance lock (`<config-dir>/loop.lock` — the same advisory lock
//! `orc loop` holds), so a daemon and a foreground loop are mutually
//! exclusive on one config dir. Foreground `orc loop` remains a documented
//! operating mode; the daemon is the ownership successor of the same lock,
//! not a second mechanism.
//!
//! Restart recovery (§9.24.2): before the first tick, every `running` row
//! in `loop.db` transitions to `orphaned` — never directly `failed` — via
//! [`LoopRunStore::recover_running_rows`]; the tick then resumes from
//! durable state. A `paused` row stays paused (§9.24.2); the recovery
//! helper only ever transitions `running` rows.
//!
//! Reconcile (§9.36): every poll tick is a reconcile pass with board-wins
//! semantics (§9.43) — the [`ReconcilePoller`] wraps the plain board
//! poller and records `board-diverged` events plus newly unblocked-task
//! promotions (§9.40) into the daemon event store on every snapshot.
//!
//! Lease/heartbeat orphan detection (§9.36): the loop's beat-staleness
//! machinery IS the lease check — a campaign run whose worker produces no
//! decision progress (no beat) inside the stall window is killed and
//! recorded; a row the supervisor never reached a terminal status for is
//! `orphaned` by restart recovery. A lease-expired row is never `failed`
//! directly.
//!
//! This crate implements no security primitive: killing, capping, and
//! pacing are orchestration limits (spec §9.27.3); the sandbox boundary
//! remains Arbitraitor's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::{BoardClient, BoardProjectConfig};
use orchestraitor_campaign::{
    BoardPoller, BoardSnapshot, CampaignDecisionStore, CampaignError, LoopConfig, LoopRunner,
    LoopSummary, LoopWorkerStarter, ReconcileEvent, ReconcileOutcome, SelectedTask, WorkerProcess,
    reconcile,
};
use orchestraitor_worker::bootstrap::build_bootstrap_transport;
use orchestraitor_worker::{
    MediatedBashMediator, ModelId, PendingDeliverySink, ProviderId, WorkerBudgets, WorkerConfig,
    run_worker,
};

use crate::error::DaemonError;

/// The watch daemon's default poll cadence (§9.36 fixed default;
/// operator-configurable via the §9.22 layered configuration key
/// `watch.poll_interval_secs`).
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// The §9.22 layered configuration key carrying the poll cadence in
/// seconds.
pub const POLL_INTERVAL_CONFIG_KEY: &str = "watch.poll_interval_secs";

/// Operator-configurable watch settings (spec §9.22 via §9.36).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchConfig {
    /// Seconds between poll ticks. Each tick is a full reconcile +
    /// supervise pass; the loop runner's supervision ticks and backoff
    /// stay in force underneath the cadence.
    pub poll_interval_secs: u64,
}

impl WatchConfig {
    /// Builds and validates the watch configuration. Fail-closed: a zero
    /// interval is a runaway poll loop and is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] naming the rejected value.
    pub fn new(poll_interval_secs: u64) -> Result<Self, DaemonError> {
        if poll_interval_secs == 0 {
            return Err(DaemonError::WatchConfig(format!(
                "{POLL_INTERVAL_CONFIG_KEY} must be at least 1 second (got 0)"
            )));
        }
        Ok(Self { poll_interval_secs })
    }

    /// The documented fixed default cadence (§9.36).
    #[must_use]
    pub const fn default_cadence() -> Self {
        Self {
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
        }
    }

    /// Resolves the operator-configured cadence through the §9.22 layered
    /// resolver. An absent key falls back to the fixed default.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] when the configured value is
    /// invalid.
    pub fn from_layers(
        resolver: &orchestraitor_core::config::ConfigResolver,
    ) -> Result<Self, DaemonError> {
        let configured = resolver
            .resolve_value(POLL_INTERVAL_CONFIG_KEY, |config| {
                config
                    .watch
                    .as_ref()
                    .and_then(|watch| watch.poll_interval_secs)
            })
            .map_err(|error| {
                DaemonError::WatchConfig(format!("{POLL_INTERVAL_CONFIG_KEY}: {error}"))
            })?;
        match configured {
            Some(resolved_value) => Self::new(resolved_value.value),
            None => Ok(Self::default_cadence()),
        }
    }
}

/// Holds the loop's advisory instance lock for the daemon's lifetime.
#[derive(Debug)]
pub struct InstanceLock {
    _file: std::fs::File,
}

/// Acquires the loop's advisory instance lock or rejects with the typed
/// single-flight error.
///
/// # Errors
///
/// Returns [`DaemonError::WatchAlreadyRunning`] when another `orc loop`
/// or `orcd watch` instance holds the lock, and
/// [`DaemonError::WatchLock`] when the lock file cannot be created or
/// locked for an OS-level reason (permission, I/O). A fresh config dir is
/// created (the lock file's parent must exist before the open).
pub fn acquire_instance_lock(config_dir: &Path) -> Result<InstanceLock, DaemonError> {
    std::fs::create_dir_all(config_dir).map_err(|source| DaemonError::WatchLock {
        path: config_dir.to_path_buf(),
        reason: format!("config dir creation failed: {source}"),
    })?;
    let path = config_dir.join("loop.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| DaemonError::WatchLock {
            path: path.clone(),
            reason: format!("lock file open failed: {source}"),
        })?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(DaemonError::WatchAlreadyRunning { path }),
        Err(error) => Err(DaemonError::WatchLock {
            path,
            reason: format!("lock acquisition failed: {error}"),
        }),
    }
}

/// Production poll side for the daemon: the same reconciled board read
/// `orc loop` performs (open items + ready queue + blocked candidates +
/// warnings).
pub struct BoardSnapshotPoller {
    client: BoardClient,
    config: BoardProjectConfig,
}

impl BoardSnapshotPoller {
    /// Builds the daemon-side poller over the board client.
    #[must_use]
    pub fn new(client: BoardClient, config: BoardProjectConfig) -> Self {
        Self { client, config }
    }
}

#[async_trait]
impl BoardPoller for BoardSnapshotPoller {
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError> {
        let (open, warnings) = self
            .client
            .item_facts(&self.config)
            .await
            .map_err(|error| CampaignError::Loop(format!("board poll failed: {error}")))?;
        let ready = orchestraitor_board::ready_queue(&open, &self.config);
        let blocked_candidates = orchestraitor_board::blocked_candidates(&open, &self.config);
        Ok(BoardSnapshot {
            open,
            ready,
            blocked_candidates,
            warnings,
        })
    }
}

/// The reconcile-wrapping poller (§9.36): delegates to the inner poller,
/// then runs the reconcile pass over the fresh snapshot — recording
/// `board-diverged` events (§9.43) and unblocked-task promotions (§9.40)
/// into the injected sink. The reconcile rides the poll tick, so every
/// pass's snapshot is reconciled exactly once.
pub struct ReconcilePoller<P: BoardPoller> {
    inner: P,
    runs: Arc<std::sync::Mutex<orchestraitor_campaign::LoopRunStore>>,
    events: Arc<std::sync::Mutex<orchestraitor_events::SqliteAuditStore>>,
    sink: Arc<dyn ReconcileSink>,
    /// The previous tick's blocked candidates (§9.40): a candidate that
    /// appears on the NEXT tick's ready queue is a newly unblocked task —
    /// without this, promotion events could never fire.
    previous_blocked: std::sync::Mutex<Vec<orchestraitor_board::ReadyItem>>,
    /// Task ids that already received a `board-diverged` event: the
    /// divergence records once per task, not once per tick (a task that
    /// stays absent from the open set must not produce an envelope every
    /// cadence tick).
    diverged_seen: std::sync::Mutex<std::collections::HashSet<String>>,
}

/// Where reconcile observations go. Production forwards into the daemon
/// runner's journal; tests capture in memory.
pub trait ReconcileSink: Send + Sync {
    /// Records one reconcile outcome.
    fn record(&self, outcome: &ReconcileOutcome);
}

#[async_trait]
impl<P: BoardPoller> BoardPoller for ReconcilePoller<P> {
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError> {
        let snapshot = self.inner.poll().await?;
        // The supervised slots' task ids are not visible here; the
        // reconcile pass scans durable `running` rows and the caller's
        // runner owns their supervision. A `running` row belonging to a
        // live slot whose task vanished from the board is still a
        // board-diverged observation — the board wins (§9.43).
        //
        // The run-state store is `rusqlite`-backed (Send, not Sync): the
        // poller owns a DEDICATED connection (not the runner's), and the
        // std-Mutex guard is held only across the synchronous `reconcile`
        // call — never across an await — so `blocking_lock`'s
        // async-context panic (a review finding, PR #555) cannot occur and
        // the runner's ticks are never blocked. WAL readers never block on
        // the runner's writes.
        //
        // Dedup: a task that stays absent from the open set records its
        // `board-diverged` event ONCE (the `diverged_seen` set), not once
        // per cadence tick. Promotion: the previous tick's blocked
        // candidates feed this tick's reconcile, so a candidate that
        // reaches the ready queue records `unblocked-task-promoted`.
        let previous_blocked = {
            let previous = self
                .previous_blocked
                .lock()
                .map_err(|_| CampaignError::Loop("reconcile state lock poisoned".to_string()))?;
            previous.clone()
        };
        let outcome = {
            let runs = self
                .runs
                .lock()
                .map_err(|_| CampaignError::Loop("run-state store lock poisoned".to_string()))?;
            reconcile(&snapshot, &previous_blocked, &[], &runs, &[])
        }
        .map_err(|error| CampaignError::Loop(format!("reconcile scan failed: {error}")))?;
        // Dedup the divergence events against `seen` (a task that stays
        // absent from the open set is recorded ONCE, not once per tick),
        // then update the reconcile state AFTER a successful pass: the
        // state must describe what the LAST RECORDED pass saw, so a failed
        // (unrecorded) pass re-observes on the next tick.
        let (events, newly_seen): (Vec<_>, Vec<_>) = {
            let seen = self
                .diverged_seen
                .lock()
                .map_err(|_| CampaignError::Loop("reconcile state lock poisoned".to_string()))?;
            let events: Vec<_> = outcome
                .events
                .iter()
                .filter(|event| match event {
                    ReconcileEvent::BoardDiverged { task_id, .. } => !seen.contains(task_id),
                    ReconcileEvent::UnblockedTaskPromoted { .. } => true,
                })
                .cloned()
                .collect();
            let newly: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    ReconcileEvent::BoardDiverged { task_id, .. } => Some(task_id.clone()),
                    ReconcileEvent::UnblockedTaskPromoted { .. } => None,
                })
                .collect();
            (events, newly)
        };
        {
            let mut previous = self
                .previous_blocked
                .lock()
                .map_err(|_| CampaignError::Loop("reconcile state lock poisoned".to_string()))?;
            previous.clone_from(&snapshot.blocked_candidates);
            let mut seen = self
                .diverged_seen
                .lock()
                .map_err(|_| CampaignError::Loop("reconcile state lock poisoned".to_string()))?;
            seen.extend(newly_seen);
        }
        if !events.is_empty() {
            let outcome = ReconcileOutcome {
                events,
                ready_task_ids: outcome.ready_task_ids,
            };
            self.record_events(&outcome)?;
            self.sink.record(&outcome);
        }
        Ok(snapshot)
    }
}

impl<P: BoardPoller> ReconcilePoller<P> {
    /// Builds the reconcile-wrapping poller.
    #[must_use]
    pub fn new(
        inner: P,
        runs: Arc<std::sync::Mutex<orchestraitor_campaign::LoopRunStore>>,
        events: Arc<std::sync::Mutex<orchestraitor_events::SqliteAuditStore>>,
        sink: Arc<dyn ReconcileSink>,
    ) -> Self {
        Self {
            inner,
            runs,
            events,
            sink,
            previous_blocked: std::sync::Mutex::new(Vec::new()),
            diverged_seen: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Persists the reconcile events into the daemon event store.
    /// Fail-closed: a reconcile that cannot be recorded surfaces as a
    /// transient poll error (the loop backs off and re-polls) — the
    /// observation is never silently dropped.
    fn record_events(&self, outcome: &ReconcileOutcome) -> Result<(), CampaignError> {
        use orchestraitor_events::{
            AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
        };
        let mut store = self
            .events
            .lock()
            .map_err(|_| CampaignError::Loop("reconcile event store lock poisoned".to_string()))?;
        for event in &outcome.events {
            let payload = match event {
                ReconcileEvent::BoardDiverged { task_id, .. } => serde_json::json!({
                    "event": "board-diverged",
                    "task_id": task_id,
                }),
                ReconcileEvent::UnblockedTaskPromoted {
                    task_id,
                    number,
                    repo,
                } => serde_json::json!({
                    "event": "unblocked-task-promoted",
                    "task_id": task_id,
                    "number": number,
                    "repo": repo,
                }),
            };
            let head = store
                .head()
                .map_err(|error| CampaignError::Loop(format!("event store head: {error}")))?;
            let envelope = EventEnvelope::try_new(EventEnvelopeInput {
                schema_version: CURRENT_SCHEMA_VERSION,
                monotonic_seq: u64::try_from(head.seq_base)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
                wall_clock_ts: rfc3339_now(),
                correlation_id: orchestraitor_model::OperationId::new(),
                parent_op_id: None,
                category: EventCategory::ToolRequest,
                payload,
                prev_hash: head.prev_hash,
            })
            .map_err(|error| CampaignError::Loop(format!("event envelope: {error}")))?;
            store
                .append(envelope)
                .map_err(|error| CampaignError::Loop(format!("event append: {error}")))?;
        }
        Ok(())
    }
}

/// RFC 3339 wall-clock timestamp; a formatting failure falls back to the
/// epoch rather than panicking (the same idiom the MCP board tools use).
fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

/// Production worker side for the daemon: the direct-path starter `orc
/// loop` uses, rebuilt over the daemon's config-dir layout (task fixtures
/// under `<config-dir>/worker-tasks/`, per-task git worktrees under
/// `<config-dir>/loop-worktrees/` — the same layouts, so leftovers from
/// either mode prune identically). The same [`WorkerBudgets`] instance the
/// loop runner validates feeds the worker, so the two enforcement layers
/// cannot drift.
pub struct DirectWatchStarter {
    project_dir: PathBuf,
    config_dir: PathBuf,
    provider_endpoint: Option<String>,
    budgets: WorkerBudgets,
}

impl DirectWatchStarter {
    /// Builds the daemon-side starter.
    #[must_use]
    pub fn new(
        project_dir: PathBuf,
        config_dir: PathBuf,
        provider_endpoint: Option<String>,
        budgets: WorkerBudgets,
    ) -> Self {
        Self {
            project_dir,
            config_dir,
            provider_endpoint,
            budgets,
        }
    }

    /// Prepares (or reuses) the task's git worktree under
    /// `<config-dir>/loop-worktrees/<task-id>` — the same layout and
    /// branch scheme (`orc-loop/<task-id>`) the foreground loop uses.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Spawn`] when `git worktree add` fails or
    /// the project directory is not a git work tree.
    fn prepare_worktree(&self, task_id: &str) -> Result<PathBuf, CampaignError> {
        let base = self.config_dir.join("loop-worktrees");
        std::fs::create_dir_all(&base).map_err(|error| CampaignError::Spawn {
            task_id: task_id.to_string(),
            message: format!("worktree base dir creation failed: {error}"),
        })?;
        // Task ids are repo-scoped slugs (deterministic charset per the
        // #313 selection contract), so they are safe path components.
        let worktree = base.join(task_id);
        let branch = format!("orc-loop/{task_id}");
        // `-B` (not `-b`): a previous invocation's branch for this task
        // still exists after its worktree was pruned — the reset makes the
        // task re-runnable.
        let output = Self::git(&self.project_dir)
            .args(["worktree", "add"])
            .arg(&worktree)
            .arg("-B")
            .arg(&branch)
            .output()
            .map_err(|error| CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!("git worktree add failed to run: {error}"),
            })?;
        if !output.status.success() {
            return Err(CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!(
                    "git worktree add failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(worktree)
    }

    /// A `git` invocation anchored at `dir` with repository-location
    /// environment variables scrubbed (the same hygiene `orc loop`
    /// applies): a `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` inherited
    /// from a hook or wrapper shell would otherwise redirect the worktree
    /// operations away from `dir`.
    fn git(dir: &Path) -> std::process::Command {
        let mut command = std::process::Command::new("git");
        command.current_dir(dir);
        for variable in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            command.env_remove(variable);
        }
        command
    }

    /// Removes every leftover worktree from previous invocations (of
    /// either `orc loop` or earlier watch runs). Called once at startup,
    /// before any spawn. Removal failures are logged — never fatal, never
    /// silent.
    pub fn prune_worktrees(config_dir: &Path, project_dir: &Path) {
        let base = config_dir.join("loop-worktrees");
        // Clear stale registrations FIRST (a removed directory, an
        // interrupted remove): without this, `worktree remove` on a
        // dangling path fails and the directory survives.
        let _ignore = Self::git(project_dir).args(["worktree", "prune"]).output();
        let Ok(entries) = std::fs::read_dir(&base) else {
            return; // no worktree base yet — nothing to prune
        };
        for entry in entries.filter_map(Result::ok) {
            let task_id = entry.file_name().to_string_lossy().into_owned();
            let output = Self::git(project_dir)
                .args(["worktree", "remove", "--force"])
                .arg(entry.path())
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    let branch = format!("orc-loop/{task_id}");
                    match Self::git(project_dir)
                        .args(["branch", "-D", &branch])
                        .output()
                    {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => tracing::warn!(
                            task_id,
                            detail = %String::from_utf8_lossy(&o.stderr).trim(),
                            "orcd watch: worktree branch prune failed"
                        ),
                        Err(error) => {
                            tracing::warn!(
                                task_id,
                                detail = %error,
                                "orcd watch: worktree branch prune failed"
                            );
                        }
                    }
                }
                Ok(output) => tracing::warn!(
                    task_id,
                    detail = %String::from_utf8_lossy(&output.stderr).trim(),
                    "orcd watch: worktree prune failed"
                ),
                Err(error) => tracing::warn!(
                    task_id,
                    detail = %error,
                    "orcd watch: worktree prune failed"
                ),
            }
        }
    }
}

#[async_trait]
impl LoopWorkerStarter for DirectWatchStarter {
    async fn start(
        &self,
        selected: &SelectedTask,
        routing: &RoleRoutingDecision,
        prior_daily_spend_usd: f64,
    ) -> Result<WorkerProcess, CampaignError> {
        let tasks_dir = self.config_dir.join("worker-tasks");
        std::fs::create_dir_all(&tasks_dir).map_err(|error| CampaignError::Spawn {
            task_id: selected.task_id.clone(),
            message: format!("worker-tasks dir creation failed: {error}"),
        })?;
        let task = orchestraitor_worker::WorkerTask {
            id: selected.task_id.clone(),
            slug: selected.task_id.clone(),
            description: format!(
                "Issue #{} ({}): {}\n\nTrack: {}\n\nImplement the leaf task as specified by \
                 the issue and its referenced spec sections. Work in the checked-out task \
                 worktree; deliver per the worker contract.",
                selected.number, selected.repo, selected.title, selected.url,
            ),
        };
        let task_path = tasks_dir.join(format!("{}.json", selected.task_id));
        let task_bytes =
            serde_json::to_vec_pretty(&task).map_err(|error| CampaignError::Spawn {
                task_id: selected.task_id.clone(),
                message: format!("task fixture serialization failed: {error}"),
            })?;
        // Write + rename so a crashed write can never leave a half-file
        // that the id-mismatch check would silently (mis)load.
        let temp_path = tasks_dir.join(format!(".{}.tmp", selected.task_id));
        std::fs::write(&temp_path, &task_bytes).map_err(|error| CampaignError::Spawn {
            task_id: selected.task_id.clone(),
            message: format!("task fixture write failed: {error}"),
        })?;
        std::fs::rename(&temp_path, &task_path).map_err(|error| CampaignError::Spawn {
            task_id: selected.task_id.clone(),
            message: format!("task fixture rename failed: {error}"),
        })?;
        let transport = Arc::new(
            build_bootstrap_transport(self.provider_endpoint.clone()).map_err(|error| {
                CampaignError::Spawn {
                    task_id: selected.task_id.clone(),
                    message: format!("transport construction failed: {error}"),
                }
            })?,
        );
        let mediator = Arc::new(MediatedBashMediator::new());
        let worktree = self.prepare_worktree(&selected.task_id)?;
        let (beats_tx, beats_rx) = tokio::sync::watch::channel(0_u64);
        let mut config = WorkerConfig::new(
            ProviderId::from_string(routing.provider.clone()),
            ModelId::from_string(routing.model.clone()),
            self.budgets.clone(),
        );
        config.prior_daily_spend_usd = prior_daily_spend_usd;
        let config = config.with_progress(beats_tx);
        let run = tokio::spawn(async move {
            run_worker(
                &task,
                &worktree,
                &*transport,
                &*mediator,
                &PendingDeliverySink,
                &config,
            )
            .await
        });
        Ok(WorkerProcess {
            beats: beats_rx,
            run,
        })
    }
}

/// The watch daemon's poll/supervise cycle: recover durable state, then
/// run the loop runner until shutdown. The poller passed here is expected
/// to be the [`ReconcilePoller`] wrapper so every tick's snapshot is
/// reconciled; the cadence rides `LoopConfig::min_poll_interval`.
///
/// Recovery (§9.24.2) runs once at startup — before the first tick — so a
/// kill -9 mid-run leaves `orphaned` rows, never stranded `running` ones,
/// and the tick resumes from durable state. `paused` stays paused by
/// construction: the recovery helper only ever transitions `running`
/// rows.
///
/// # Errors
///
/// Returns [`DaemonError::WatchRun`] when a durable write fails or the
/// loop runner exits with an error.
#[expect(
    clippy::too_many_arguments,
    reason = "each dependency is a distinct seam (config, two hermeticity traits, two durable stores, routing identity, shutdown channel, virtual clock origin); grouping them would hide which argument serves which invariant — the loop runner's own constructor documents the same tradeoff"
)]
pub async fn run_watch<P: BoardPoller, S: LoopWorkerStarter>(
    loop_config: LoopConfig,
    poller: P,
    starter: S,
    decisions: &CampaignDecisionStore,
    runs: &orchestraitor_campaign::LoopRunStore,
    routing: RoleRoutingDecision,
    shutdown: tokio::sync::watch::Receiver<u64>,
    start_unix_secs: u64,
) -> Result<LoopSummary, DaemonError> {
    // §9.24.2 restart recovery BEFORE the first tick: running → orphaned.
    let recovered = runs
        .recover_running_rows(start_unix_secs)
        .map_err(|error| DaemonError::WatchRun(error.to_string()))?;
    for row in &recovered {
        tracing::warn!(
            run_id = row.id,
            task_id = %row.task_id,
            "orcd watch: restart recovery orphaned a running row"
        );
    }

    let invocation_id = format!("watch-{start_unix_secs}");
    let runner = LoopRunner::new(
        loop_config,
        poller,
        starter,
        decisions,
        runs,
        routing,
        invocation_id,
        start_unix_secs,
    )
    .map_err(|error| DaemonError::WatchRun(error.to_string()))?;
    let summary = runner
        .run(shutdown)
        .await
        .map_err(|error| DaemonError::WatchRun(error.to_string()))?;
    Ok(summary)
}

/// A no-op reconcile sink for callers that only need the durable event
/// records.
#[derive(Debug, Default, Clone, Copy)]
pub struct JournallessSink;

impl ReconcileSink for JournallessSink {
    fn record(&self, _outcome: &ReconcileOutcome) {}
}

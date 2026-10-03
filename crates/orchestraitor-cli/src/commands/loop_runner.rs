//! `orc loop` — the cron-shaped foreground bootstrap loop (issue #314;
//! spec `10-orchestrator.md` §9.36 thin slice).
//!
//! One instance: poll the board through the #308 provider, run one campaign
//! pass (selection + exactly one decision record via `plan_pass`), spawn
//! the worker on the daemon-less direct path, supervise the in-flight runs
//! (stall kills, worker timeout, graceful shutdown), and pace the next
//! pass with the pinned backoff schedule — all through the
//! `orchestraitor-campaign` loop runner, which owns every guard.
//!
//! Single-instance enforcement uses an advisory file lock on
//! `<config-dir>/loop.lock`; a second concurrent invocation is a typed
//! `loop-already-running` rejection. The lock is released by the OS when
//! the process exits.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use miette::{Diagnostic, IntoDiagnostic, Result, miette};
use orchestraitor_agent_catalog::{RoleRouter, RoleRoutingDecision};
use orchestraitor_board::{BoardClient, BoardProjectConfig, SecretUriAuth};
use orchestraitor_campaign::{
    BoardPoller, BoardSnapshot, CampaignDecisionStore, CampaignError, LoopConfig, LoopRunner,
    LoopWorkerStarter, WorkerProcess,
};
use orchestraitor_worker::bootstrap::build_bootstrap_transport;
use orchestraitor_worker::{
    FixtureTaskSource, MediatedBashMediator, ModelId, PendingDeliverySink, ProviderId, TaskSource,
    WorkerBudgets, WorkerConfig, run_worker,
};

use crate::cli::{ConfigPaths, LoopArgs};
use crate::commands::config::layers::load_layers;

use super::{WORKER_ROLE, require_bootstrap_provider};

/// Production poll side: the same reconciled read `orc campaign run` does
/// (open items + ready queue + blocked candidates + warnings).
struct BoardSnapshotPoller {
    client: BoardClient,
    config: BoardProjectConfig,
}

#[async_trait]
impl BoardPoller for BoardSnapshotPoller {
    /// Reads board facts and derives ready and blocked candidates for one pass.
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

/// Production worker side: builds the bootstrap transport per spawn and
/// drives `run_worker` on a background task (the runner already executes
/// inside a runtime — no nested runtime is created). The starter receives
/// the loop's own `WorkerBudgets` instance, so the worker's enforcement is
/// the loop's enforcement — a single source, structurally no drift.
///
/// Each worker slot gets its own `git worktree` keyed by task id under
/// `<config-dir>/loop-worktrees/` (issue #434): with
/// `max_concurrent_workers = 2` two workers would otherwise edit the same
/// directory and overwrite each other's files. Worktrees from previous
/// invocations are pruned at startup (registration, directory, and the
/// `orc-loop/<task-id>` branch), so the base never grows unboundedly and a
/// task that stays Ready can be re-run by a later invocation.
struct DirectLoopStarter {
    project_dir: PathBuf,
    config_dir: PathBuf,
    tasks_dir: Option<PathBuf>,
    provider_endpoint: Option<String>,
    budgets: WorkerBudgets,
}

impl DirectLoopStarter {
    /// Prepares (or reuses) the task's worktree and returns its path.
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
        // still exists after its worktree was pruned (`git worktree remove`
        // never deletes branches) — the reset makes the task re-runnable
        // in a later board-driven invocation instead of failing every
        // spawn with "branch already exists".
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
    /// environment variables scrubbed: a `GIT_DIR`/`GIT_WORK_TREE`/
    /// `GIT_INDEX_FILE` inherited from a hook or wrapper shell would
    /// otherwise redirect the worktree operations away from `dir`.
    fn git(dir: &Path) -> std::process::Command {
        let mut command = std::process::Command::new("git");
        command.current_dir(dir);
        for variable in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            command.env_remove(variable);
        }
        command
    }

    /// Removes every leftover worktree from previous invocations. Called
    /// once at startup, before any spawn. `git worktree prune` first
    /// clears stale registrations (a removed directory, an interrupted
    /// remove), then each surviving worktree is force-removed with its
    /// `orc-loop/<task-id>` branch deleted, so a task that stays Ready can
    /// be re-selected and re-run by a later invocation. Removal failures
    /// are reported to stderr (the crate's best-effort diagnostic idiom):
    /// they never fail the run, but they are never silent either.
    fn prune_worktrees(config_dir: &Path, project_dir: &Path) {
        let base = config_dir.join("loop-worktrees");
        // Clear stale registrations FIRST (a removed directory, an
        // interrupted remove): without this, `worktree remove` on a
        // dangling path fails and the directory survives.
        let _ignore = Self::git(project_dir).args(["worktree", "prune"]).output();
        let Ok(entries) = std::fs::read_dir(&base) else {
            return; // no worktree base yet — nothing to prune
        };
        for entry in entries.filter_map(Result::ok) {
            // `git worktree remove --force` clears the project repo's
            // worktree registration; a bare directory removal would leave a
            // stale one behind. The branch is deleted ONLY after a
            // successful removal — a failed removal leaves the branch
            // checked out, and git refuses to delete a checked-out branch.
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
                        Ok(o) => {
                            Self::report_prune_failure(
                                &task_id,
                                &String::from_utf8_lossy(&o.stderr),
                            );
                        }
                        Err(error) => {
                            Self::report_prune_failure(&task_id, &error.to_string());
                        }
                    }
                }
                Ok(output) => {
                    Self::report_prune_failure(&task_id, &String::from_utf8_lossy(&output.stderr));
                }
                Err(error) => Self::report_prune_failure(&task_id, &error.to_string()),
            }
        }
    }

    /// Reports a worktree-prune failure to stderr (best-effort — the same
    /// idiom as `report_signal_failure`; a prune failure never fails the
    /// run but must be visible).
    fn report_prune_failure(task_id: &str, detail: &str) {
        let stderr = std::io::stderr();
        let _ignore = writeln!(
            stderr.lock(),
            "orc loop: worktree prune failed for {task_id}: {}",
            detail.trim()
        );
    }
}

#[async_trait]
impl LoopWorkerStarter for DirectLoopStarter {
    /// Starts a bootstrap worker with shared budgets, prior spend, and progress beats.
    async fn start(
        &self,
        task_id: &str,
        routing: &RoleRoutingDecision,
        prior_daily_spend_usd: f64,
    ) -> Result<WorkerProcess, CampaignError> {
        if let Err(error) = require_bootstrap_provider(&routing.provider) {
            return Err(CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!("{error:?}"),
            });
        }
        let tasks_dir = self
            .tasks_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.join("worker-tasks"));
        let task = FixtureTaskSource::new(tasks_dir)
            .load(task_id)
            .map_err(|error| CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!("fixture task load failed: {error}"),
            })?;
        let transport = Arc::new(
            build_bootstrap_transport(self.provider_endpoint.clone()).map_err(|error| {
                CampaignError::Spawn {
                    task_id: task_id.to_string(),
                    message: format!("transport construction failed: {error}"),
                }
            })?,
        );
        let mediator = Arc::new(MediatedBashMediator::new());
        let project_dir = self.prepare_worktree(task_id)?;
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
                &project_dir,
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

/// Holds the advisory instance lock for the duration of the run.
struct InstanceLock {
    _file: std::fs::File,
}

/// A second concurrent `orc loop` invocation refused the instance lock.
///
/// The rejection carries the `loop-already-running` code as a structured
/// miette diagnostic code — rendered on its own report header (readable by
/// stderr-report consumers that scrape for the code), independent of
/// terminal width and message wrapping.
#[derive(Debug, thiserror::Error, Diagnostic)]
#[error("another `orc loop` instance already holds the lock at {path}")]
#[diagnostic(code("loop-already-running"))]
struct LoopAlreadyRunning {
    path: PathBuf,
}

/// Acquires the advisory lock or rejects with `loop-already-running`.
///
/// # Errors
///
/// Returns a diagnostic when the lock file cannot be created or is held by
/// another invocation.
fn acquire_instance_lock(config_dir: &std::path::Path) -> Result<InstanceLock> {
    // A fresh checkout has no config dir yet — the lock file's parent must
    // exist before the open, or the run dies on a bare ENOENT.
    std::fs::create_dir_all(config_dir).into_diagnostic()?;
    let path = config_dir.join("loop.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .into_diagnostic()?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(LoopAlreadyRunning { path }.into()),
        // A permission or OS-level failure is NOT another instance —
        // reporting it as `loop-already-running` would misdirect the
        // operator; surface the underlying error instead.
        Err(error) => Err(miette!(
            "instance lock at {} could not be acquired: {error}",
            path.display()
        )),
    }
}

/// Spawns the SIGTERM/SIGINT fan-in task: one channel increment per signal.
/// The unix-specific path is `#[cfg(unix)]`; other platforms fall back to
/// `ctrl_c()` (SIGINT only). Registration failure is reported and degrades
/// the channel set — never silently swallowed.
#[cfg(unix)]
fn spawn_signal_task() -> (
    tokio::sync::watch::Receiver<u64>,
    tokio::task::JoinHandle<()>,
) {
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    let signals = tokio::spawn(async move {
        let count_rx = signal_tx.subscribe();
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(stream) => Some(stream),
                Err(error) => {
                    report_signal_failure(&format!(
                        "SIGTERM handler registration failed: {error}; \
                         SIGTERM will use the process default action"
                    ));
                    None
                }
            };
        let mut interrupt =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                Ok(stream) => Some(stream),
                Err(error) => {
                    report_signal_failure(&format!(
                        "SIGINT handler registration failed: {error}; \
                         SIGINT will use the process default action"
                    ));
                    None
                }
            };
        if terminate.is_none() && interrupt.is_none() {
            // No channel registered: signals take the process default
            // action (terminate) — fail loudly, never run unkillable.
            report_signal_failure(
                "no signal handlers could be installed; shutdown signals \
                 will terminate the process abruptly",
            );
            return;
        }
        loop {
            let mut count = *count_rx.borrow();
            tokio::select! {
                _ = async {
                    match terminate.as_mut() {
                        Some(stream) => stream.recv().await,
                        None => std::future::pending().await,
                    }
                } => {}
                _ = async {
                    match interrupt.as_mut() {
                        Some(stream) => stream.recv().await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            count += 1;
            let _ignore = signal_tx.send(count);
        }
    });
    (signal_rx, signals)
}

/// Non-unix fallback: `ctrl_c()` only (SIGINT-equivalent console event).
#[cfg(not(unix))]
fn spawn_signal_task() -> (
    tokio::sync::watch::Receiver<u64>,
    tokio::task::JoinHandle<()>,
) {
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    let signals = tokio::spawn(async move {
        let count_rx = signal_tx.subscribe();
        // `tokio::signal::ctrl_c()` is an async fn (`io::Result<()>`), not a
        // stream: await it once per signal and let it re-register the
        // console handler on each iteration.
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                report_signal_failure("ctrl-c handler registration failed");
                return;
            }
            let count = count_rx.borrow().wrapping_add(1);
            let _ignore = signal_tx.send(count);
        }
    });
    (signal_rx, signals)
}

/// Writes a signal-handling warning to stderr (the crate's stderr idiom —
/// the lint denies `eprintln!`; write failures are best-effort diagnostics).
fn report_signal_failure(message: &str) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ignore = writeln!(lock, "orc loop: {message}");
}

/// Runs an `orc loop` invocation.
///
/// # Errors
/// Returns a diagnostic when the board client, role resolution, stores,
/// instance lock, or loop runner fail. Worker failures are recorded run
/// outcomes, not process errors.
pub fn run(paths: &ConfigPaths, args: &LoopArgs, writer: &mut dyn Write) -> Result<()> {
    let _lock = acquire_instance_lock(&paths.config_dir)?;

    let (config, _path) = BoardProjectConfig::load(&paths.project_dir).into_diagnostic()?;
    let token_uri = config
        .token_uri
        .clone()
        .ok_or(orchestraitor_board::BoardError::AuthNotConfigured)
        .into_diagnostic()?;
    let auth = Arc::new(SecretUriAuth::new(token_uri));
    let client = match &paths.github_graphql_endpoint {
        Some(endpoint) => BoardClient::with_endpoint(endpoint.clone(), auth),
        None => BoardClient::new(auth),
    }
    .into_diagnostic()?;
    let client = match &paths.board_cache_path {
        Some(path) => client.with_cache_path(Some(path.clone())),
        None => client,
    };

    let layers = load_layers(paths)?;
    let routing = RoleRouter::new(&layers.resolver)
        .resolve(WORKER_ROLE)
        .map_err(|error| miette!("{error}"))?;

    let decisions =
        CampaignDecisionStore::open(&paths.config_dir.join("campaign.db")).into_diagnostic()?;
    let runs = orchestraitor_campaign::LoopRunStore::open(&paths.config_dir.join("loop.db"))
        .into_diagnostic()?;

    let loop_config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        std::time::Duration::from_secs(5),
        args.max_cycles,
    )
    .map_err(|error| miette!("{error}"))?;

    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    // Prune leftover worktrees from previous invocations before any spawn
    // (see `prune_worktrees`): the loop runs unattended, so the worktree
    // base must not grow across invocations.
    DirectLoopStarter::prune_worktrees(&paths.config_dir, &paths.project_dir);
    let run_result = runtime.block_on(async {
        let (signal_rx, signals) = spawn_signal_task();
        let invocation_id = format!(
            "loop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default()
        );
        let start_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let poller = BoardSnapshotPoller { client, config };
        let starter = DirectLoopStarter {
            project_dir: paths.project_dir.clone(),
            config_dir: paths.config_dir.clone(),
            tasks_dir: args.worker_tasks_dir.clone(),
            provider_endpoint: args.worker_provider_endpoint.clone(),
            // The same guard instance the runner validates and enforces:
            // the worker layer reads these pinned values, so the two
            // enforcement layers cannot drift.
            budgets: loop_config.budgets.clone(),
        };
        let runner = LoopRunner::new(
            loop_config,
            poller,
            starter,
            &decisions,
            &runs,
            routing,
            invocation_id,
            start_unix_secs,
        );
        let summary = match runner {
            Ok(runner) => runner.run(signal_rx).await,
            Err(error) => Err(error),
        };
        signals.abort();
        summary.map_err(|error| miette!("{error}"))
    });
    // Bound the shutdown by the daemon budget: a `spawn_blocking` bash op
    // (mediated execution runs its blocking I/O on the blocking pool) must
    // not hold process exit open past the advertised five seconds.
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    let summary = run_result?;

    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &summary).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
    } else {
        render_text(writer, &summary)?;
    }
    Ok(())
}

/// Writes the human-readable stop reason, outcome counts, and elapsed time.
fn render_text(
    writer: &mut dyn Write,
    summary: &orchestraitor_campaign::LoopSummary,
) -> Result<()> {
    writeln!(writer, "loop stopped: {:?}", summary.stop_reason).into_diagnostic()?;
    writeln!(
        writer,
        "cycles = {} spawns = {}",
        summary.cycles, summary.spawns
    )
    .into_diagnostic()?;
    writeln!(
        writer,
        "completed = {} failed = {} stalled = {} timed-out = {} aborted = {} poll-failures = {}",
        summary.completed,
        summary.failed,
        summary.stalled,
        summary.timed_out,
        summary.aborted_on_stop,
        summary.poll_failures
    )
    .into_diagnostic()?;
    writeln!(writer, "elapsed = {}s", summary.elapsed_secs).into_diagnostic()?;
    Ok(())
}

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
use std::path::PathBuf;
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

/// The role the dispatched workers run as (mirrors `orc campaign`).
const WORKER_ROLE: &str = "implement";

/// Production poll side: the same reconciled read `orc campaign run` does
/// (open items + ready queue + blocked candidates + warnings).
struct BoardSnapshotPoller {
    client: BoardClient,
    config: BoardProjectConfig,
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

/// Production worker side: builds the bootstrap transport per spawn and
/// drives `run_worker` on a background task (the runner already executes
/// inside a runtime — no nested runtime is created). The starter receives
/// the loop's own `WorkerBudgets` instance, so the worker's enforcement is
/// the loop's enforcement — a single source, structurally no drift.
struct DirectLoopStarter {
    project_dir: PathBuf,
    config_dir: PathBuf,
    tasks_dir: Option<PathBuf>,
    provider_endpoint: Option<String>,
    budgets: WorkerBudgets,
}

#[async_trait]
impl LoopWorkerStarter for DirectLoopStarter {
    async fn start(
        &self,
        task_id: &str,
        routing: &RoleRoutingDecision,
        prior_daily_spend_usd: f64,
    ) -> Result<WorkerProcess, CampaignError> {
        if routing.provider != "neuralwatt" {
            return Err(CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!(
                    "bootstrap worker supports only the `neuralwatt` provider (spec §10.3); \
                     roles.{WORKER_ROLE}.routing.provider resolved to `{}`",
                    routing.provider
                ),
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
        let project_dir = self.project_dir.clone();
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
        let mut interrupt = match tokio::signal::ctrl_c() {
            Ok(stream) => stream,
            Err(error) => {
                report_signal_failure(&format!("ctrl-c handler registration failed: {error}"));
                return;
            }
        };
        loop {
            let mut count = *count_rx.borrow();
            interrupt.recv().await;
            count += 1;
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
    let summary = runtime.block_on(async {
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
    })?;

    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &summary).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
    } else {
        render_text(writer, &summary)?;
    }
    Ok(())
}

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

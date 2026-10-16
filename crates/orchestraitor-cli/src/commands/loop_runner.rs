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
use miette::{Diagnostic, IntoDiagnostic, Result, WrapErr, miette};
use orchestraitor_agent_catalog::{
    RoleRouter, RoleRoutingDecision, resolve_decision_provider_with,
};
use orchestraitor_board::{BoardClient, BoardProjectConfig, SecretUriAuth};
use orchestraitor_campaign::{
    BoardPoller, BoardSnapshot, CampaignDecisionStore, CampaignError, LoopConfig, LoopRunner,
    LoopWorkerStarter, SelectedTask, WorkerProcess,
};
use orchestraitor_worker::bootstrap::build_bootstrap_transport;
use orchestraitor_worker::{
    MediatedBashMediator, ModelId, PendingDeliverySink, ProviderId, WorkerBudgets, WorkerConfig,
    run_worker,
};

use crate::cli::{ConfigPaths, LoopArgs};
use crate::commands::config::layers::load_layers;

use super::{WORKER_ROLE, require_bootstrap_provider};

/// Resolves the anti-stuck guardrail settings from the layered config
/// `[loop]` (spec `10-orchestrator.md` §9.27.1): an absent block leaves
/// the bootstrap defaults active; an explicit `0` disables a guard
/// deliberately (warned loudly, never silent). On config resolution
/// failure a warning is emitted and the defaults are used — the guard set
/// stays functional over a partially broken config layer.
fn resolve_guardrails(
    resolver: &orchestraitor_core::ConfigResolver,
) -> orchestraitor_campaign::GuardrailsSettings {
    match resolver.resolve_config() {
        Ok(effective) => {
            let guard_values = effective.r#loop.as_ref().map(config_values);
            let (settings, warnings) =
                orchestraitor_campaign::GuardrailsSettings::from_config(guard_values.as_ref());
            for warning in &warnings {
                report_loop_warning(warning);
            }
            settings
        }
        Err(error) => {
            report_loop_warning(&format!(
                "config resolution failed ({error}); anti-stuck guardrails fall back to defaults"
            ));
            orchestraitor_campaign::GuardrailsSettings::default()
        }
    }
}

/// Converts the core config block into the campaign settings input shape
/// (identical field names; a `None` block maps to all-`None` keys).
fn config_values(
    block: &orchestraitor_core::config::LoopGuardrailsConfig,
) -> orchestraitor_campaign::GuardrailsConfigValues {
    orchestraitor_campaign::GuardrailsConfigValues {
        no_progress_turns: block.no_progress_turns,
        tool_repeat_count: block.tool_repeat_count,
        tool_repeat_window: block.tool_repeat_window,
        ci_poll_budget_secs: block.ci_poll_budget_secs,
        max_task_attempts: block.max_task_attempts,
        task_retry_backoff_secs: block.task_retry_backoff_secs,
        max_turns_per_attempt: block.max_turns_per_attempt,
    }
}

/// Production poll side: the same reconciled read `orc campaign run` does
/// (open items + ready queue + blocked candidates + warnings).
struct BoardSnapshotPoller {
    client: Arc<BoardClient>,
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
    /// The board client the #495 issue-body fetch routes through (the same
    /// App-identity route the poller reads): the starter materializes the
    /// task fixture WITH the issue content.
    board_client: Arc<BoardClient>,
    /// The delivery sink (issue #656): a completed run's worktree state
    /// lands through `orc github push-branch` + the labeling-chained PR
    /// path. `None` keeps the typed `Pending` bootstrap outcome.
    delivery: Option<Arc<orchestraitor_worker::PushBranchDeliverySink>>,
    budgets: WorkerBudgets,
    /// The worker-side anti-stuck guardrail thresholds resolved from
    /// `[loop]` — the same settings the runner validates and enforces.
    guardrails: orchestraitor_worker::GuardrailsConfig,
    /// Shared process-lifetime cost ledger for per-call attribution
    /// (spec §9.19.4); `None` when the ledger could not be opened and the
    /// run degrades to unattributed. `CostLedger` wraps a `rusqlite`
    /// connection (`!Sync`), so spawns share it behind a mutex.
    cost_ledger: Option<Arc<std::sync::Mutex<orchestraitor_cost_ledger::CostLedger>>>,
    /// Loop invocation id, minted once per `orc loop` run; names each
    /// worker run's cost-attribution session.
    invocation_id: String,
    /// The §13.5.1 profile label (spec §9.22.5) resolved from
    /// `roles.<role>.routing.profile`; recorded on every cost entry the
    /// spawned run writes so `orc stats efficiency --group-by-profile`
    /// can compare with/without-compaction profiles. `None` = unprofiled.
    profile: Option<String>,
    /// Declared-tool surface built ONCE at startup (the layer-trust gate,
    /// role routing, and sub-role provider gate all run before the loop
    /// accepts work): a broken tool definition is a typed startup error,
    /// never a per-spawn failure after worktrees are created.
    declared_tools: Vec<orchestraitor_worker::ToolDefinition>,
    /// The built-in `think` tool's kill-switch state (§9.48.2).
    think_enabled: bool,
    /// The §9.48.8 payload-recording consent gate, resolved once at startup.
    record_payloads: bool,
    /// The §9.17 event store: every receipted `think` call (accepted step or
    /// typed refusal) persists as an `agent reasoning` event when the run
    /// completes — the SAME behavior the daemon path provides (§9.48.8).
    reasoning_events: std::sync::Arc<std::sync::Mutex<orchestraitor_events::SqliteAuditStore>>,
    /// The sub-session routing resolved with that tool surface (the §9.35
    /// decision-record evidence).
    subsession_routing:
        std::collections::BTreeMap<String, orchestraitor_worker::RoleRoutingEvidence>,
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
        // Seed the worker's codegraph lane (spec `10-orchestrator.md`
        // §9.38): the fresh checkout has no index snapshot (gitignored
        // build artifact), so copy the project root's if present. Best
        // effort — a missing or unreadable snapshot degrades the worker's
        // `sym:` search to plain content search, never blocks the spawn.
        let snapshot = self.project_dir.join(".orchestraitor/codegraph.json");
        if snapshot.is_file() {
            let target_dir = worktree.join(".orchestraitor");
            if std::fs::create_dir_all(&target_dir).is_ok() {
                let _ignore = std::fs::copy(&snapshot, target_dir.join("codegraph.json"));
            }
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

    /// Materializes the task fixture from the board selection (write +
    /// rename so a crashed write can never leave a half-file that the
    /// id-mismatch check would silently (mis)load).
    ///
    /// The description carries the issue body verbatim in a fenced
    /// untrusted-content block (issue #495): the fixture is built through
    /// the shared bounded posture (`task_description`), so both starters
    /// cannot drift. A body-fetch failure degrades to the title+URL
    /// description with a typed notice — never a spawn failure, never a
    /// silent omission.
    async fn materialize_task_fixture(
        &self,
        tasks_dir: &Path,
        selected: &SelectedTask,
    ) -> Result<orchestraitor_worker::WorkerTask, CampaignError> {
        let fetched = match self
            .board_client
            .issue_body(&selected.repo, selected.number)
            .await
        {
            Ok(content) => Some(content.body),
            Err(error) => {
                report_loop_warning(&format!(
                    "issue #{} body fetch failed for task {}; the fixture carries the \
                     typed notice: {error}",
                    selected.number, selected.task_id
                ));
                None
            }
        };
        let task = orchestraitor_worker::WorkerTask {
            id: selected.task_id.clone(),
            slug: selected.task_id.clone(),
            description: orchestraitor_campaign::task_description(selected, fetched.as_deref()),
        };
        let task_path = tasks_dir.join(format!("{}.json", selected.task_id));
        let task_bytes =
            serde_json::to_vec_pretty(&task).map_err(|error| CampaignError::Spawn {
                task_id: selected.task_id.clone(),
                message: format!("task fixture serialization failed: {error}"),
            })?;
        let temp_path = tasks_dir.join(format!(".{}.tmp", selected.task_id));
        std::fs::write(&temp_path, &task_bytes).map_err(|error| CampaignError::Spawn {
            task_id: selected.task_id.clone(),
            message: format!("task fixture write failed: {error}"),
        })?;
        std::fs::rename(&temp_path, &task_path).map_err(|error| CampaignError::Spawn {
            task_id: selected.task_id.clone(),
            message: format!("task fixture rename failed: {error}"),
        })?;
        Ok(task)
    }

    /// Attaches per-call cost tracking when the ledger is available: one
    /// cost row per model call, attributed to
    ///   agent = the board task (the domain agent),
    ///   role  = the routed orchestration role,
    ///   session = this loop invocation + run id.
    fn with_cost_tracking(
        &self,
        mut config: WorkerConfig,
        selected: &SelectedTask,
        routing: &RoleRoutingDecision,
    ) -> WorkerConfig {
        let Some(ledger) = &self.cost_ledger else {
            return config;
        };
        let attribution = orchestraitor_provider_neuralwatt::cost::CostAttribution {
            agent_domain_id: orchestraitor_model::AgentId::from_string(selected.task_id.clone()),
            role: routing.role.clone(),
            project: self.project_dir.file_name().map_or_else(
                || "unknown".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
            session: orchestraitor_model::SessionId::from_string(format!(
                "{}-{}",
                self.invocation_id, selected.task_id
            )),
            repository: orchestraitor_model::RepositoryId::from_string(
                self.project_dir.display().to_string(),
            ),
        };
        let sink: std::sync::Arc<dyn orchestraitor_provider_neuralwatt::CostSink> =
            std::sync::Arc::new(MutexLedgerSink {
                ledger: Arc::clone(ledger),
            });
        config = config.with_cost_tracking(attribution, sink);
        config
    }
}

#[async_trait]
impl LoopWorkerStarter for DirectLoopStarter {
    /// Starts a bootstrap worker with shared budgets, prior spend, and progress beats.
    ///
    /// Materializes the task fixture from the board selection first: the
    /// loop cannot rely on an operator hand-writing
    /// `<config-dir>/worker-tasks/<id>.json` for every board item. The
    /// fixture is (re)written on every spawn from the `SelectedTask` the
    /// board produced — the board is the description's source of truth, and
    /// a stale file from an earlier selection can never shadow it. The
    #[expect(
        clippy::too_many_lines,
        reason = "the spawn path (fixture materialization, transport, worktree, config assembly, delivery wiring, spawned run + reasoning persistence) is one linear audited sequence; splitting it would scatter the per-spawn gates the reviewer must see together"
    )]
    /// description carries the issue body VERBATIM (issue #495), fetched at
    /// fixture-creation time through the board client and bounded by the
    /// shared `task_description` posture (URLs are data, not pre-fetched
    /// content; the body is the one exception the loop fetches for the
    /// worker).
    async fn start(
        &self,
        selected: &SelectedTask,
        routing: &RoleRoutingDecision,
        prior_daily_spend_usd: f64,
    ) -> Result<WorkerProcess, CampaignError> {
        if let Err(error) = require_bootstrap_provider(&routing.provider) {
            return Err(CampaignError::Spawn {
                task_id: selected.task_id.clone(),
                message: format!("{error:?}"),
            });
        }
        let tasks_dir = self
            .tasks_dir
            .clone()
            .unwrap_or_else(|| self.config_dir.join("worker-tasks"));
        if let Err(error) = std::fs::create_dir_all(&tasks_dir) {
            return Err(CampaignError::Spawn {
                task_id: selected.task_id.clone(),
                message: format!("worker-tasks dir creation failed: {error}"),
            });
        }
        let task = self.materialize_task_fixture(&tasks_dir, selected).await?;
        let transport = Arc::new(
            build_bootstrap_transport(self.provider_endpoint.clone()).map_err(|error| {
                CampaignError::Spawn {
                    task_id: selected.task_id.clone(),
                    message: format!("transport construction failed: {error}"),
                }
            })?,
        );
        let mediator = Arc::new(MediatedBashMediator::new());
        let project_dir = self.prepare_worktree(&selected.task_id)?;
        let (beats_tx, beats_rx) = tokio::sync::watch::channel(0_u64);
        let mut config = WorkerConfig::new(
            ProviderId::from_string(routing.provider.clone()),
            ModelId::from_string(routing.model.clone()),
            self.budgets.clone(),
        );
        config.guardrails = self.guardrails.clone();
        config.prior_daily_spend_usd = prior_daily_spend_usd;
        // Issue #656: the run's delivery request carries the worktree
        // branch the sink lands (`orc-loop/<task-id>` — the branch
        // `prepare_worktree` checked out).
        config.worktree_branch = Some(format!("orc-loop/{}", selected.task_id));
        // The delivery sink is cloned per spawn (`Arc`): a push failure is
        // a typed delivery outcome on the run, never a silent drop, and
        // never blocks the loop's success accounting (the run result
        // carries the typed `DeliveryFailed` failure).
        let delivery: Arc<dyn orchestraitor_worker::DeliverySink> = match &self.delivery {
            Some(sink) => Arc::clone(sink) as Arc<dyn orchestraitor_worker::DeliverySink>,
            None => Arc::new(PendingDeliverySink),
        };
        let config = super::attach_declared_surface(
            config,
            &super::DeclaredToolSurface {
                tools: self.declared_tools.clone(),
                subsession_routing: self.subsession_routing.clone(),
                think_enabled: self.think_enabled,
            },
        );
        let config = self.with_cost_tracking(config, selected, routing);
        let config = config
            .with_profile(self.profile.clone())
            .with_record_payloads(self.record_payloads);
        let config = config.with_progress(beats_tx);
        let reasoning_events = std::sync::Arc::clone(&self.reasoning_events);
        let record_payloads_spawn = self.record_payloads;
        let run_task_id = selected.task_id.clone();
        let run = tokio::spawn(async move {
            let result = run_worker(
                &task,
                &project_dir,
                &*transport,
                &*mediator,
                &*delivery,
                &config,
            )
            .await;
            // §9.48.8 persistence (the daemon path's behavior, mirrored):
            // every receipted `think` call appends one hash-chained
            // `agent reasoning` event. Synchronous SQLite I/O moves off
            // the async runtime; a failure degrades to a warning — the run
            // result still carries the records (never silently dropped).
            if let Ok(worker_run) = &result {
                let persistence = tokio::task::spawn_blocking({
                    let reasoning_events = std::sync::Arc::clone(&reasoning_events);
                    let run_task_id = run_task_id.clone();
                    let records = worker_run.reasoning_events.clone();
                    move || {
                        orchestraitor_events::append_reasoning_events(
                            &reasoning_events,
                            &run_task_id,
                            &records,
                            record_payloads_spawn,
                        )
                    }
                })
                .await;
                match persistence {
                    Ok(Err(error)) => {
                        report_loop_warning(&format!(
                            "reasoning event persistence failed: {error}"
                        ));
                    }
                    Err(join_error) => {
                        report_loop_warning(&format!(
                            "reasoning event persistence task failed: {join_error}"
                        ));
                    }
                    Ok(Ok(())) => {}
                }
            }
            result
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

/// Bridges a shared `CostLedger` (rusqlite `!Sync`) into the `Send + Sync`
/// `CostSink` trait by locking per call.
struct MutexLedgerSink {
    ledger: Arc<std::sync::Mutex<orchestraitor_cost_ledger::CostLedger>>,
}

impl orchestraitor_provider_neuralwatt::CostSink for MutexLedgerSink {
    fn record(&self, entry: &orchestraitor_cost_ledger::CostEntry) -> Result<(), String> {
        let ledger = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger
            .api_spend()
            .insert_cost_entry(entry)
            .map_err(|error| error.to_string())
    }
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
                    report_loop_warning(&format!(
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
                    report_loop_warning(&format!(
                        "SIGINT handler registration failed: {error}; \
                         SIGINT will use the process default action"
                    ));
                    None
                }
            };
        if terminate.is_none() && interrupt.is_none() {
            // No channel registered: signals take the process default
            // action (terminate) — fail loudly, never run unkillable.
            report_loop_warning(
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
                report_loop_warning("ctrl-c handler registration failed");
                return;
            }
            let count = count_rx.borrow().wrapping_add(1);
            let _ignore = signal_tx.send(count);
        }
    });
    (signal_rx, signals)
}

/// Writes a best-effort loop warning to stderr (the crate's stderr idiom —
/// the lint denies `eprintln!`; write failures are ignored). Used for
/// degraded-but-fatal-to-nothing conditions: signal-handler fallbacks and
/// the cost-ledger open failure.
fn report_loop_warning(message: &str) {
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
#[expect(
    clippy::too_many_lines,
    reason = "the startup wiring (lock, config, client, routing, decision provider, stores, ledger, starter) is one linear assembly; splitting it would scatter the fail-closed gates the operator must see together"
)]
pub fn run(paths: &ConfigPaths, args: &LoopArgs, writer: &mut dyn Write) -> Result<()> {
    let _lock = acquire_instance_lock(&paths.config_dir)?;

    let (config, _path) = BoardProjectConfig::load(&paths.project_dir).into_diagnostic()?;
    // Issue #656: the delivery target defaults to the board config's first
    // repo (the MVP single-repo loop). Captured BEFORE `config` moves into
    // the poller.
    let board_config_repos: Vec<String> = config.repos.clone();
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
    // The §13.5.1 A/B grouping label: the profile named on the role's
    // routing entry (spec §9.22.5), if any. Absent = unprofiled rows.
    // A config resolution failure is a WARNING with no profile label,
    // never a startup abort: the label is optional bookkeeping and
    // "bookkeeping must never block delivery" (a PR #565 gen-3 review
    // finding — the `?` here made the same error fatal that
    // `resolve_guardrails` treats as a warning).
    let (routing_profile, record_payloads) = match layers.resolver.resolve_config() {
        Ok(effective) => {
            let routing_profile = effective
                .roles
                .and_then(|roles| roles.get(WORKER_ROLE).cloned())
                .and_then(|role| role.routing)
                .and_then(|routing| routing.profile);
            // The 9.48.8 consent gate, read once at startup like the daemon
            // path: `thought_text` persists only when the operator enabled
            // payload recording (spec 50 20.4.2; default false).
            let record_payloads = effective
                .observability
                .is_some_and(|observability| observability.record_payloads);
            (routing_profile, record_payloads)
        }
        Err(error) => {
            report_loop_warning(&format!(
                "profile label unresolved ({error}); runs record no profile"
            ));
            (None, false)
        }
    };

    // Decision-provider consultation (spec `30-model-routing.md` §9.45, default off): when the
    // `routing.provider` flag names an implementation, each pass consults it
    // for task selection first; any error falls back to the deterministic
    // selector inside the campaign session and is recorded in the decision.
    // The provider lives in the outer scope so its lifetime covers `run`.
    let decision_provider =
        resolve_decision_provider_with(&layers.resolver, &super::build_named_decision_provider)
            .map_err(|error| miette!("{error}"))?;

    let decisions =
        CampaignDecisionStore::open(&paths.config_dir.join("campaign.db")).into_diagnostic()?;
    let runs = orchestraitor_campaign::LoopRunStore::open(&paths.config_dir.join("loop.db"))
        .into_diagnostic()?;

    let guardrails = resolve_guardrails(&layers.resolver);
    let loop_config = LoopConfig::with_guardrails(
        WorkerBudgets::bootstrap_defaults(),
        std::time::Duration::from_secs(5),
        args.max_cycles,
        guardrails,
    )
    .map_err(|error| miette!("{error}"))?;

    // Per-agent cost tracking (spec §9.19.4): one process-lifetime ledger
    // at `.orchestraitor/cost.db`; every worker model call records a
    // CostEntry attributed to its run. A ledger open failure degrades to
    // unattributed runs (spend still flows through loop.db's soft-cap
    // path) — bookkeeping must never block delivery.
    let cost_ledger =
        match orchestraitor_cost_ledger::CostLedger::open(&paths.config_dir.join("cost.db")) {
            Ok(ledger) => Some(Arc::new(std::sync::Mutex::new(ledger))),
            Err(error) => {
                report_loop_warning(&format!("cost ledger open failed: {error}"));
                None
            }
        };

    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    // Prune leftover worktrees from previous invocations before any spawn
    // (see `prune_worktrees`): the loop runs unattended, so the worktree
    // base must not grow across invocations.
    DirectLoopStarter::prune_worktrees(&paths.config_dir, &paths.project_dir);
    let run_result = runtime.block_on(async {
        let (signal_rx, signals) = spawn_signal_task();
        // Millis alone can collide across hosts sharing a ledger dir (and
        // clock regressions collapse to "loop-0"); pid + nanos makes the
        // id process-unique in practice.
        let invocation_id = format!(
            "loop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
        );
        let start_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let board_client = Arc::new(client);
        // Issue #656: resolve the delivery sink ONCE at startup from the
        // loop flags. `--deliver` requires a repo (the board config's first
        // repo is the default for the MVP single-repo loop); a missing orc
        // binary is a typed startup error — the delivery must never
        // discover a missing binary only after a task completes.
        let delivery_sink = if args.deliver {
            let repo = args
                .delivery_repo
                .clone()
                .or_else(|| board_config_repos.first().cloned())
                .ok_or_else(|| {
                    miette!(
                        "--deliver requires --delivery-repo owner/name (no board-config repo found)"
                    )
                })?;
            let orc_bin = args
                .delivery_orc_bin
                .clone()
                .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "orc".into()));
            let base = args
                .delivery_base
                .clone()
                .unwrap_or_else(|| "main".to_string());
            Some(Arc::new(orchestraitor_worker::PushBranchDeliverySink::new(
                orc_bin, repo, base,
            )))
        } else {
            None
        };
        let poller = BoardSnapshotPoller {
            client: Arc::clone(&board_client),
            config,
        };
        // Gated ONCE at startup, before the loop accepts work: a broken
        // tool definition is a typed startup error, never a per-spawn
        // failure after worktrees are created.
        let surface = super::build_declared_tools(&layers.resolver, &routing.role)
            .map_err(|error| miette!("{error}"))?;
        // The §9.48.8 reasoning-event store: the foreground loop persists
        // the same `agent reasoning` events the daemon path persists — one
        // store, the same category and consent semantics (§9.48.8).
        let reasoning_events = orchestraitor_events::SqliteAuditStore::open(
            paths.config_dir.join("loop-events.db").as_path(),
        )
        .into_diagnostic()
        .wrap_err("loop reasoning event store open failed")?;
        let starter = DirectLoopStarter {
            project_dir: paths.project_dir.clone(),
            config_dir: paths.config_dir.clone(),
            tasks_dir: args.worker_tasks_dir.clone(),
            provider_endpoint: args.worker_provider_endpoint.clone(),
            board_client,
            // Issue #656: the delivery sink is resolved from the loop CLI
            // flags (`--deliver` + repo/base); `None` keeps the typed
            // `Pending` bootstrap outcome. The first configured repo is the
            // delivery target for the MVP single-repo loop.
            delivery: delivery_sink,
            // The same guard instance the runner validates and enforces:
            // the worker layer reads these pinned values, so the two
            // enforcement layers cannot drift.
            budgets: loop_config.budgets.clone(),
            guardrails: loop_config.guardrails.worker_guardrails(),
            cost_ledger: cost_ledger.clone(),
            invocation_id: invocation_id.clone(),
            profile: routing_profile.clone(),
            // Gated once, above: never re-validated per spawn.
            declared_tools: surface.tools,
            think_enabled: surface.think_enabled,
            record_payloads,
            reasoning_events: std::sync::Arc::new(std::sync::Mutex::new(reasoning_events)),
            subsession_routing: surface.subsession_routing,
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
        )
        .map(|runner| match decision_provider.as_deref() {
            Some(provider) => runner.with_task_selector(provider),
            None => runner,
        });
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
        write_json_summary(writer, &summary, cost_ledger.as_deref())?;
    } else {
        render_text(&mut *writer, &summary)?;
        render_agent_costs(&mut *writer, cost_ledger.as_deref())?;
    }
    Ok(())
}

/// Queries the per-agent rollups (spec §9.19.4); `None` when tracking is
/// off. A query failure reports to stderr and yields an empty set — a
/// reporting failure never fails the run.
fn agent_cost_rollups(
    ledger: Option<&std::sync::Mutex<orchestraitor_cost_ledger::CostLedger>>,
) -> Vec<orchestraitor_cost_ledger::DomainCostRollup> {
    let Some(ledger) = ledger else {
        return Vec::new();
    };
    match ledger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .api_spend()
        .all_agent_rollups()
    {
        Ok(rollups) => rollups,
        Err(error) => {
            let stderr = std::io::stderr();
            let _ignore = writeln!(stderr.lock(), "orc loop: agent cost report failed: {error}");
            Vec::new()
        }
    }
}

/// Writes the JSON end-of-run summary: per-agent cost rollups (spec
/// §9.19.4) ride INSIDE the one summary document as `agent_costs` so
/// single-document consumers are unaffected when tracking is off.
fn write_json_summary(
    writer: &mut dyn Write,
    summary: &orchestraitor_campaign::LoopSummary,
    ledger: Option<&std::sync::Mutex<orchestraitor_cost_ledger::CostLedger>>,
) -> Result<()> {
    let mut document = serde_json::to_value(summary).into_diagnostic()?;
    let rollups = agent_cost_rollups(ledger);
    if !rollups.is_empty() {
        document["agent_costs"] = serde_json::to_value(&rollups).into_diagnostic()?;
    }
    serde_json::to_writer_pretty(&mut *writer, &document).into_diagnostic()?;
    writeln!(&mut *writer).into_diagnostic()?;
    Ok(())
}

/// Appends the text-mode per-agent cost report (spec §9.19.4).
fn render_agent_costs(
    writer: &mut dyn Write,
    ledger: Option<&std::sync::Mutex<orchestraitor_cost_ledger::CostLedger>>,
) -> Result<()> {
    let rollups = agent_cost_rollups(ledger);
    if rollups.is_empty() {
        return Ok(());
    }
    writeln!(writer, "agent costs:").into_diagnostic()?;
    for rollup in rollups {
        writeln!(
            writer,
            "  {}  in={} out={} calls={} est=${:.4}",
            rollup.agent_domain_id.as_str(),
            rollup.input_tokens,
            rollup.output_tokens,
            rollup.request_count,
            rollup.monetary_cost_estimated
        )
        .into_diagnostic()?;
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

#[cfg(test)]
mod tests {
    use super::DirectLoopStarter;
    use miette::IntoDiagnostic;
    use orchestraitor_board::BoardClient;
    use orchestraitor_campaign::SelectedTask;
    use std::io::Read;
    use std::sync::Arc;

    /// A minimal GraphQL stub serving one canned `data` payload per request
    /// (the same needle-match shape the board crate's `ScriptServer` uses,
    /// reduced to what the fixture tests need).
    struct OneShotServer {
        endpoint: String,
        shutdown: Arc<std::sync::Mutex<bool>>,
        join: Option<std::thread::JoinHandle<()>>,
        // The listener thread holds its own Arc of the payload (cloned
        // before spawn), so no field is needed here.
    }

    impl OneShotServer {
        fn start(payload: String) -> std::io::Result<Self> {
            use std::net::TcpListener;
            let payload: Arc<str> = payload.into();
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            listener.set_nonblocking(true)?;
            let endpoint = format!("http://{}/graphql", listener.local_addr()?);
            let shutdown = Arc::new(std::sync::Mutex::new(false));
            let thread_shutdown = Arc::clone(&shutdown);
            let thread_payload = Arc::clone(&payload);
            let join = std::thread::spawn(move || {
                loop {
                    if let Ok(guard) = thread_shutdown.lock()
                        && *guard
                    {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            let connection_payload = Arc::clone(&thread_payload);
                            let _ignore = std::thread::spawn(move || {
                                serve_one(stream, &connection_payload);
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                }
            });
            Ok(Self {
                endpoint,
                shutdown,
                join: Some(join),
            })
        }
    }

    impl OneShotServer {
        /// The endpoint; an accessor so `Drop` never blocks a move-out.
        fn endpoint(&self) -> &str {
            &self.endpoint
        }
    }

    /// Serves ONE connection: reads the request, answers with `payload`,
    /// and drains to peer-EOF before dropping (the same shape the board
    /// crate's `ScriptServer` harness uses — avoids TCP RST on drop).
    fn serve_one(mut stream: std::net::TcpStream, payload: &str) {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let _ignore = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(clone) => clone,
            Err(_) => return,
        });
        let mut content_length = 0_usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if let Some(value) = line
                .trim_end()
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
            if line.trim_end().is_empty() {
                break;
            }
        }
        let mut body = vec![0_u8; content_length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ignore = stream.write_all(response.as_bytes());
        let _ignore = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
        let _ignore = stream.shutdown(std::net::Shutdown::Write);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut tail = [0_u8; 1024];
        while std::time::Instant::now() < deadline {
            match stream.read(&mut tail) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    impl Drop for OneShotServer {
        fn drop(&mut self) {
            if let Ok(mut guard) = self.shutdown.lock() {
                *guard = true;
            }
            if let Some(join) = self.join.take() {
                let _ignore = join.join();
            }
        }
    }

    /// A starter whose board client answers the `issueBody` query with
    /// `payload` (every request; the fixture path needs one query).
    fn starter_with_body(
        payload: String,
    ) -> std::io::Result<(DirectLoopStarter, OneShotServer, Arc<tempfile::TempDir>)> {
        let server = OneShotServer::start(payload)?;
        let auth: Arc<dyn orchestraitor_board::BoardAuth> = Arc::new(FixtureAuth);
        let client = BoardClient::with_endpoint(server.endpoint().to_string(), auth)
            .map_err(|error| std::io::Error::other(format!("client construction failed: {error}")))?
            .with_cache_path(None);
        let (reasoning_store, event_dir) = test_reasoning_store()?;
        let starter = DirectLoopStarter {
            project_dir: std::env::temp_dir(),
            config_dir: std::env::temp_dir(),
            tasks_dir: None,
            provider_endpoint: None,
            board_client: Arc::new(client),
            delivery: None,
            budgets: orchestraitor_worker::WorkerBudgets::bootstrap_defaults(),
            guardrails: orchestraitor_worker::GuardrailsConfig::bootstrap_defaults(),
            cost_ledger: None,
            invocation_id: "test-invocation".to_string(),
            profile: None,
            declared_tools: Vec::new(),
            think_enabled: true,
            record_payloads: false,
            reasoning_events: reasoning_store,
            subsession_routing: std::collections::BTreeMap::new(),
        };
        Ok((starter, server, Arc::new(event_dir)))
    }

    /// Static bearer auth: the stub server never validates the value.
    struct FixtureAuth;

    #[async_trait::async_trait]
    impl orchestraitor_board::BoardAuth for FixtureAuth {
        async fn bearer_token(
            &self,
        ) -> Result<secrecy::SecretString, orchestraitor_board::BoardError> {
            Ok(secrecy::SecretString::from("fixture-token-value"))
        }
    }

    /// Opens a throwaway reasoning-event store in a temp dir (the starter
    /// field's type; the fixture tests never write reasoning events).
    fn test_reasoning_store() -> std::io::Result<(
        std::sync::Arc<std::sync::Mutex<orchestraitor_events::SqliteAuditStore>>,
        tempfile::TempDir,
    )> {
        let dir = tempfile::tempdir().map_err(|error| {
            std::io::Error::other(format!("temp dir for the event store: {error}"))
        })?;
        let path = dir.path().join("events.db");
        // The store opens the file under the TempDir; the CALLER keeps the
        // TempDir alive for the test's scope, so the OS cleanup can never
        // race the open store — no leak, no forget.
        let store = orchestraitor_events::SqliteAuditStore::open(&path)
            .map_err(|error| std::io::Error::other(format!("event store open failed: {error}")))?;
        Ok((std::sync::Arc::new(std::sync::Mutex::new(store)), dir))
    }

    /// The campaign tests drive `orc loop` with fake starters, so the
    /// production fixture (`materialize_task_fixture`) is only covered here:
    /// assert the written task JSON carries the selected board identity
    /// (id, repo, issue number, title, URL) AND the issue body (issue
    /// #495), and that re-materializing the same task with a CHANGED title
    /// atomically replaces the fixture.
    #[tokio::test]
    async fn materialize_task_fixture_writes_and_replaces_selected_task() -> miette::Result<()> {
        let tasks_dir = tempfile::tempdir().into_diagnostic()?;
        let dir = tasks_dir.path();

        let selected = SelectedTask {
            repo: "arbsec/orchestraitor".to_string(),
            number: 519,
            title: "commit-identity guard & thread-reply discipline".to_string(),
            url: "https://github.com/arbsec/orchestraitor/pull/519".to_string(),
            item_node_id: "PRR_519".to_string(),
            task_id: "pr519-agent-ops".to_string(),
        };
        let body_payload = serde_json::json!({
            "data": {
                "repository": {
                    "issue": {
                        "title": "t",
                        "body": "## Acceptance\n- the guard fires\n"
                    }
                }
            }
        })
        .to_string();
        let (starter, _server, _event_dir) = starter_with_body(body_payload).into_diagnostic()?;
        let task = starter
            .materialize_task_fixture(dir, &selected)
            .await
            .map_err(|error| miette::miette!("fixture materialization failed: {error}"))?;

        let path = dir.join(format!("{}.json", selected.task_id));
        let first = read_fixture(&path)?;
        assert_eq!(first["id"], selected.task_id);
        assert_eq!(first["slug"], selected.task_id);
        let description = first["description"].as_str().unwrap_or_default();
        assert!(description.contains("#519"), "issue number in description");
        assert!(
            description.contains("arbsec/orchestraitor"),
            "repo in description"
        );
        assert!(
            description.contains("commit-identity guard & thread-reply discipline"),
            "title in description"
        );
        assert!(
            description.contains("https://github.com/arbsec/orchestraitor/pull/519"),
            "url in description"
        );
        // Issue #495: the body rides the fixture verbatim, fenced.
        assert!(
            description.contains("## Acceptance\n- the guard fires\n"),
            "issue body verbatim in the fixture description: {description}"
        );
        assert!(
            description.contains("UNTRUSTED DATA"),
            "the 6.1 untrusted-content label is present: {description}"
        );
        assert_eq!(
            task.description, description,
            "the returned task matches the written fixture"
        );

        // Re-materialize with a changed title: the write+rename contract
        // must REPLACE the fixture, not fail on the existing file.
        let renamed = SelectedTask {
            title: "renamed: ops discipline".to_string(),
            ..selected
        };
        starter
            .materialize_task_fixture(dir, &renamed)
            .await
            .map_err(|error| miette::miette!("replacement materialization failed: {error}"))?;
        let second = read_fixture(&path)?;
        assert!(
            second["description"]
                .as_str()
                .unwrap_or_default()
                .contains("renamed: ops discipline"),
            "re-materialization must carry the new title"
        );
        // No temp half-file may survive the rename.
        let leftover: Vec<_> = std::fs::read_dir(dir)
            .into_diagnostic()?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(
            leftover.is_empty(),
            "no temp files may survive: {leftover:?}"
        );
        Ok(())
    }

    /// A board client that CANNOT serve the body (connection refused: the
    /// endpoint points at a closed port) must NOT fail the spawn — the
    /// fixture carries the pre-#495 framing plus the typed fetch-failure
    /// notice, so the worker is told the body is absent instead of
    /// inferring criteria from the title alone (issue #495, degrade path).
    #[tokio::test]
    async fn materialize_task_fixture_degrades_when_body_fetch_fails() -> miette::Result<()> {
        let tasks_dir = tempfile::tempdir().into_diagnostic()?;
        let dir = tasks_dir.path();
        let auth: Arc<dyn orchestraitor_board::BoardAuth> = Arc::new(FixtureAuth);
        // Port 1: closed; the fetch fails at the transport layer.
        let client = BoardClient::with_endpoint("http://127.0.0.1:1/graphql".to_string(), auth)
            .into_diagnostic()?
            .with_cache_path(None);
        // The TempDir binding outlives the Arc'd store in THIS test's scope:
        // the store's file lives under it, so no forget and no leak.
        let (event_store, _event_dir) = test_reasoning_store()
            .map_err(|error| miette::miette!("event store open failed: {error}"))?;
        let starter = DirectLoopStarter {
            project_dir: std::env::temp_dir(),
            config_dir: std::env::temp_dir(),
            tasks_dir: None,
            provider_endpoint: None,
            board_client: Arc::new(client),
            delivery: None,
            budgets: orchestraitor_worker::WorkerBudgets::bootstrap_defaults(),
            guardrails: orchestraitor_worker::GuardrailsConfig::bootstrap_defaults(),
            cost_ledger: None,
            invocation_id: "test-invocation".to_string(),
            profile: None,
            declared_tools: Vec::new(),
            think_enabled: true,
            record_payloads: false,
            reasoning_events: Arc::clone(&event_store),
            subsession_routing: std::collections::BTreeMap::new(),
        };
        let selected = SelectedTask {
            repo: "arbsec/orchestraitor".to_string(),
            number: 495,
            title: "task descriptions lose the issue body".to_string(),
            url: "https://github.com/arbsec/orchestraitor/issues/495".to_string(),
            item_node_id: "PVTI_495".to_string(),
            task_id: "issue-495-body".to_string(),
        };
        starter
            .materialize_task_fixture(dir, &selected)
            .await
            .map_err(|error| miette::miette!("degrade path must not fail the spawn: {error}"))?;
        let description =
            read_fixture(&dir.join(format!("{}.json", selected.task_id)))?["description"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        assert!(
            description
                .contains("NOTICE: the issue body could not be fetched at fixture-creation time"),
            "typed fetch-failure notice present: {description}"
        );
        assert!(
            description.contains("#495 (arbsec/orchestraitor)"),
            "pre-#495 framing survives: {description}"
        );
        Ok(())
    }

    /// An oversized issue body is capped in the fixture with the explicit
    /// truncation notice; the inline cap keeps the worker prompt bounded
    /// (issue #495, bounded-payload posture).
    #[tokio::test]
    async fn materialize_task_fixture_caps_oversized_body() -> miette::Result<()> {
        let tasks_dir = tempfile::tempdir().into_diagnostic()?;
        let dir = tasks_dir.path();
        let long_body = "y".repeat(orchestraitor_campaign::MAX_INLINE_BODY_CHARS + 500);
        let payload = serde_json::json!({
            "data": {
                "repository": {
                    "issue": { "title": "t", "body": long_body }
                }
            }
        })
        .to_string();
        let (starter, _server, _event_dir) = starter_with_body(payload).into_diagnostic()?;
        let selected = SelectedTask {
            repo: "arbsec/orchestraitor".to_string(),
            number: 496,
            title: "turn-budget".to_string(),
            url: "https://github.com/arbsec/orchestraitor/issues/496".to_string(),
            item_node_id: "PVTI_496".to_string(),
            task_id: "issue-496-budget".to_string(),
        };
        starter
            .materialize_task_fixture(dir, &selected)
            .await
            .map_err(|error| miette::miette!("capped materialization failed: {error}"))?;
        let description =
            read_fixture(&dir.join(format!("{}.json", selected.task_id)))?["description"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        assert!(
            description.contains("exceeded the 8192-character inline cap and was truncated"),
            "truncation notice present"
        );
        // The description must NOT contain the body's tail (the 500 y's
        // beyond the cap) — the cap, not the full body, is inlined.
        assert!(
            !description.contains(&long_body[..]),
            "the full oversized body must not be inlined"
        );
        assert!(
            !description.contains(&"y".repeat(orchestraitor_campaign::MAX_INLINE_BODY_CHARS + 1)),
            "no run beyond the cap is inlined"
        );
        assert!(
            description.contains(&"y".repeat(orchestraitor_campaign::MAX_INLINE_BODY_CHARS)),
            "the cap-sized prefix IS inlined"
        );
        Ok(())
    }

    /// Reads and parses one task fixture from `path`.
    fn read_fixture(path: &std::path::Path) -> miette::Result<serde_json::Value> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .into_diagnostic()?
            .read_to_end(&mut bytes)
            .into_diagnostic()?;
        serde_json::from_slice(&bytes)
            .map_err(|error| miette::miette!("fixture must be valid JSON: {error}"))
    }
}

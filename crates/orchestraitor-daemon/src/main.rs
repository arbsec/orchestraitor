//! `orcd` daemon binary.
//!
//! Two modes (spec `10-orchestrator.md` §9.36):
//! - default: the JSON-RPC server on a Unix-domain socket;
//! - `watch`: the §9.36 watch thin slice (issue #503) — the port of `orc
//!   loop`'s poll/supervise cycle as the daemon's running mode, with
//!   §9.24.2 restart recovery, board-wins reconcile, and single-flight
//!   ownership of the loop's instance lock.

use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use miette::{IntoDiagnostic, Result, WrapErr};
use orchestraitor_daemon::{
    DaemonConfig, DaemonError, DirectWatchStarter, JournallessSink, ReconcilePoller, WatchConfig,
    acquire_instance_lock, run_until_signal, run_watch,
};
use orchestraitor_worker::WorkerBudgets;

/// The `implement` role the dispatched workers run as (the loop's own
/// role; the daemon spawns the same bootstrap worker).
const WORKER_ROLE: &str = "implement";

fn main() -> Result<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().and_then(|arg| arg.into_string().ok());
    match mode.as_deref() {
        Some("watch") => run_watch_mode(),
        _ => run_rpc_mode(),
    }
}

/// Starts the `orcd` JSON-RPC daemon on a Unix-domain socket.
fn run_rpc_mode() -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::BuildRuntime)
        .into_diagnostic()?;
    runtime.block_on(async {
        run_until_signal(DaemonConfig::new(socket_path()))
            .await
            .into_diagnostic()?;
        Ok(ExitCode::SUCCESS)
    })
}

/// Runs the `orcd watch` mode: the §9.36 poll/supervise cycle.
fn run_watch_mode() -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::BuildRuntime)
        .into_diagnostic()?;
    runtime.block_on(async { watch_cycle().await })
}

/// One watch cycle: lock, recover, run the loop runner until shutdown.
#[expect(
    clippy::too_many_lines,
    reason = "the startup wiring (lock, layered config, board client, stores, worktree prune, guard set, signal fan-in, runner assembly) is one linear fail-closed gate sequence; splitting it would scatter the gates the operator must see together — the same tradeoff `orc loop`'s runner documents"
)]
async fn watch_cycle() -> Result<ExitCode> {
    let config_dir = std::env::var_os("ORCHESTRAITOR_CONFIG_DIR")
        .map_or_else(|| PathBuf::from(".orchestraitor"), PathBuf::from);
    let project_dir = std::env::var_os("ORCHESTRAITOR_PROJECT_DIR")
        .map_or_else(|| PathBuf::from("."), PathBuf::from);

    // Single-flight: the daemon takes over the loop's instance lock — a
    // foreground `orc loop` or a second watch daemon is refused.
    let _lock = acquire_instance_lock(&config_dir).into_diagnostic()?;

    // §9.22 layered configuration: the poll cadence and the worker role
    // routing resolve through the same layer stack the CLI reads.
    let resolver = load_layered_resolver(&config_dir, &project_dir)?;
    let watch_config = WatchConfig::from_layers(&resolver).into_diagnostic()?;
    let routing = orchestraitor_agent_catalog::RoleRouter::new(&resolver)
        .resolve(WORKER_ROLE)
        .into_diagnostic()
        .wrap_err("worker role resolution failed")?;

    // The board client: the same reconciled read `orc loop` polls through.
    let (board_config_template, _path) =
        orchestraitor_board::BoardProjectConfig::load(&project_dir).into_diagnostic()?;
    let token_uri = board_config_template
        .token_uri
        .clone()
        .ok_or(orchestraitor_board::BoardError::AuthNotConfigured)
        .into_diagnostic()?;
    let auth = Arc::new(orchestraitor_board::SecretUriAuth::new(token_uri));

    // Durable stores: the same files `orc loop` uses, so recovery and
    // history are shared between the two operating modes.
    let decisions =
        orchestraitor_campaign::CampaignDecisionStore::open(&config_dir.join("campaign.db"))
            .into_diagnostic()
            .wrap_err("campaign decision store open failed")?;
    let runs = orchestraitor_campaign::LoopRunStore::open(&config_dir.join("loop.db"))
        .into_diagnostic()
        .wrap_err("loop run-state store open failed")?;
    // Two independent connections to the same WAL database: the runner
    // borrows one directly (its borrow spans awaits, which a Mutex cannot
    // — and need not — guard on a current-thread runtime); the reconcile
    // poller owns the other behind a std Mutex held only across its
    // synchronous scan. `blocking_lock` would panic in async context (a
    // PR #555 review finding) and serializing one connection behind a
    // guard held for the whole run would deadlock the poller. WAL readers
    // never block on the runner's writes.

    // Prune leftover worktrees from previous invocations (of either
    // operating mode) before any spawn.
    DirectWatchStarter::prune_worktrees(&config_dir, &project_dir);

    // The pinned guard set: the same `WorkerBudgets` instance the runner
    // validates feeds the worker starter — the two layers cannot drift.
    let budgets = WorkerBudgets::bootstrap_defaults();
    // The [loop] guardrails resolve from the SAME layered config the
    // foreground loop reads — the guard set is the same for both operating
    // modes (a PR #555 review finding: with_cadence dropped operator
    // guardrails).
    // Fail closed on config resolution failure: defaulting the guardrails
    // would silently ignore an operator's loop.* settings (a disabled or
    // tightened guard must never silently widen — the same fail-closed rule
    // WatchConfig::from_layers applies to the cadence).
    let effective = resolver
        .resolve_config()
        .map_err(|error| miette::miette!("configuration resolution failed: {error}"))?;
    let values =
        effective
            .r#loop
            .as_ref()
            .map(|block| orchestraitor_campaign::GuardrailsConfigValues {
                no_progress_turns: block.no_progress_turns,
                tool_repeat_count: block.tool_repeat_count,
                tool_repeat_window: block.tool_repeat_window,
                ci_poll_budget_secs: block.ci_poll_budget_secs,
                max_task_attempts: block.max_task_attempts,
                task_retry_backoff_secs: block.task_retry_backoff_secs,
            });
    let (guardrails, warnings) =
        orchestraitor_campaign::GuardrailsSettings::from_config(values.as_ref());
    for warning in &warnings {
        tracing::warn!("orcd watch: {warning}");
    }
    let loop_config = orchestraitor_campaign::LoopConfig::with_guardrails(
        budgets.clone(),
        Duration::from_secs(5),
        None,
        guardrails,
    )
    .into_diagnostic()
    .wrap_err("watch loop configuration rejected")?
    .with_min_poll_interval(Some(Duration::from_secs(watch_config.poll_interval_secs)));

    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    // SIGTERM/SIGINT fan-in: one channel increment per signal. The loop
    // runner's graceful drain bounds the shutdown by the 5s daemon budget.
    #[cfg(unix)]
    let signal_task = tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut interrupt = signal(SignalKind::interrupt()).ok();
        let mut count = 0_u64;
        loop {
            tokio::select! {
                () = async {
                    match terminate.as_mut() {
                        Some(stream) => { stream.recv().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {}
                () = async {
                    match interrupt.as_mut() {
                        Some(stream) => { stream.recv().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {}
            }
            count = count.wrapping_add(1);
            let _ignore = signal_tx.send(count);
        }
    });
    #[cfg(not(unix))]
    let signal_task = tokio::spawn(async move {
        let mut count = 0_u64;
        loop {
            match tokio::signal::ctrl_c().await {
                // Each send increments the watch channel: a second signal
                // still registers as a change.
                Ok(()) => {
                    count = count.wrapping_add(1);
                    let _ignore = signal_tx.send(count);
                }
                // Handler registration failed: retrying at once would
                // busy-spin the current-thread runtime; break and let the
                // process default action handle further signals.
                Err(error) => {
                    tracing::warn!(
                        "orcd watch: ctrl-c handler failed ({error}); \
                         further signals take the process default action"
                    );
                    break;
                }
            }
        }
    });

    let starter = DirectWatchStarter::new(project_dir, config_dir.clone(), None, budgets);

    // The daemon is ALWAYS-RUNNING (§9.36): the 4h whole-run budget ends one
    // runner invocation, not the daemon — on RunBudgetExhausted a fresh
    // invocation (NEW id and clock origin, derived inside the loop: a reused
    // id would make excluded_tasks accumulate across 4h windows and a stale
    // clock origin would skew every guard timestamp) continues watching
    // until the operator's shutdown signal. A shutdown stop (or any other
    // terminal reason) exits.
    let mut run_ordinal = 0_u64;
    let summary = loop {
        // The run ordinal disambiguates invocation ids even if two restarts
        // land in the same second (a PR #555 review thread): a repeated id
        // would merge the exclusion scopes of two invocations.
        run_ordinal = run_ordinal.wrapping_add(1);
        // The signal count observed when this invocation started: a signal
        // DURING the invocation preempts the budget drain and keeps
        // RunBudgetExhausted as the summary reason, but the operator asked
        // for shutdown — compare against the baseline, not absolute zero
        // (a PR #555 review thread: the cumulative count is not a
        // pending-signal test).
        let signal_baseline = *signal_rx.borrow();
        // Fresh per-invocation identity and clock origin (the Tokio clock
        // restarts at zero on each runner run, so the wall-clock origin
        // must be re-read too — a stale origin would skew daily-spend day
        // boundaries, row timestamps, and backoff checks).
        let start_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .into_diagnostic()
            .wrap_err("system clock is before the Unix epoch")?;
        let invocation_id = format!("watch-{start_unix_secs}-{run_ordinal}");
        // A fresh client per invocation (reqwest build is cheap; the auth Arc
        // is shared): the loop restarts only on the 4h run budget. The board
        // config is cloned per invocation (cheap validated struct).
        let client = orchestraitor_board::BoardClient::new(
            Arc::clone(&auth) as Arc<dyn orchestraitor_board::BoardAuth>
        )
        .into_diagnostic()
        .wrap_err("board client construction failed")?;
        let board_config = board_config_template.clone();
        // Fresh reconcile connections per invocation: cheap SQLite opens; the
        // databases are durable and shared.
        let poller_runs = orchestraitor_campaign::LoopRunStore::open(&config_dir.join("loop.db"))
            .into_diagnostic()
            .wrap_err("loop run-state store open failed (reconcile handle)")?;
        let events =
            orchestraitor_events::SqliteAuditStore::open(config_dir.join("watch-events.db"))
                .into_diagnostic()
                .wrap_err("watch event store open failed")?;
        // A fresh poller per invocation: the reconcile state (promoted-set,
        // divergence dedup) is invocation-scoped by design — recovery
        // re-orphans and the new invocation re-observes cleanly. The event
        // store is shared and durable.
        let poller = ReconcilePoller::new(
            orchestraitor_daemon::BoardSnapshotPoller::new(client, board_config),
            // The poller's dedicated connection (see the comment above): opening a
            // second handle on the same WAL database is safe — the reconcile scan
            // only reads, and WAL readers never block the runner's writes.
            Arc::new(std::sync::Mutex::new(poller_runs)),
            Arc::new(std::sync::Mutex::new(events)),
            Arc::new(JournallessSink),
        );
        let summary = run_watch(
            loop_config.clone(),
            poller,
            starter.clone(),
            &decisions,
            &runs,
            routing.clone(),
            signal_rx.clone(),
            start_unix_secs,
            &invocation_id,
        )
        .await
        .into_diagnostic()?;
        match summary.stop_reason {
            orchestraitor_campaign::StopReason::RunBudgetExhausted
                // Restart only when no signal arrived during this
                // invocation (baseline comparison above).
                if *signal_rx.borrow() == signal_baseline =>
            {
                tracing::info!(
                    cycles = summary.cycles,
                    spawns = summary.spawns,
                    "orcd watch: run budget exhausted; starting the next invocation"
                );
            }
            _ => break summary,
        }
    };

    signal_task.abort();
    tracing::info!(
        cycles = summary.cycles,
        spawns = summary.spawns,
        completed = summary.completed,
        "orcd watch: stopped ({:?})",
        summary.stop_reason
    );
    // A non-shutdown terminal stop (spend soft cap, guardrails exhausted)
    // must be VISIBLE to the supervisor: exit non-zero so `Restart=on-
    // failure` restarts the daemon (e.g. the next UTC day after a spend
    // cap) instead of silently staying down (a PR #555 review finding).
    match summary.stop_reason {
        orchestraitor_campaign::StopReason::Shutdown => Ok(ExitCode::SUCCESS),
        _ => Err(miette::miette!(
            "orcd watch stopped: {:?} ({} cycles, {} spawns, {} completed)",
            summary.stop_reason,
            summary.cycles,
            summary.spawns,
            summary.completed
        )),
    }
}

fn socket_path() -> PathBuf {
    std::env::args_os()
        .skip(1)
        .find_map(|arg| arg.into_string().ok())
        .or_else(|| std::env::var("ORCHESTRAITOR_DAEMON_SOCKET").ok())
        .map_or_else(default_socket_path, PathBuf::from)
}

fn default_socket_path() -> PathBuf {
    std::env::temp_dir().join("orchestraitor").join("orcd.sock")
}

/// Builds the §9.22 layered resolver from the same layer files the CLI
/// reads (built-in defaults, user, org, project, dir shards). The daemon
/// resolves `watch.*` and `roles.<id>.routing.*` through it.
fn load_layered_resolver(
    config_dir: &std::path::Path,
    project_dir: &std::path::Path,
) -> Result<orchestraitor_core::config::ConfigResolver> {
    use orchestraitor_core::config::{ConfigLayer, ConfigResolver, ConfigSource};

    const BUILT_IN_DEFAULTS: &str = r#"
[roles.explore.routing]
provider = "neuralwatt"
model = "glm-5.3-flash"

[roles.research.routing]
provider = "neuralwatt"
model = "glm-5.3-flash"

[roles.plan.routing]
provider = "neuralwatt"
model = "glm-5.3-flash"

[roles.implement.routing]
provider = "neuralwatt"
model = "glm-5.3-flash"

[roles.review.routing]
provider = "neuralwatt"
model = "glm-5.3-flash"

[roles.verify.routing]
provider = "neuralwatt"
model = "glm-5.3-flash"
"#;

    let mut resolver = ConfigResolver::new()
        .with_toml(
            ConfigSource {
                layer: ConfigLayer::BuiltInDefaults,
                name: "built-in defaults".to_string(),
            },
            BUILT_IN_DEFAULTS,
        )
        .map_err(|error| miette::miette!("configuration parse failed: {error}"))?;

    // Primary files plus their `*.toml` shard directories (user.d/, org.d/,
    // orchestraitor.d/, dir.d/) — the same layer files the CLI reads, shard
    // support included. All files in one layer group share the layer, so a
    // duplicate key across shards fails through the resolver's
    // same-layer-conflict check instead of silently choosing one file.
    let layers: Vec<(ConfigLayer, Vec<PathBuf>)> = vec![
        (
            ConfigLayer::GlobalUser,
            primary_and_shards(config_dir, "user.toml", "user.d"),
        ),
        (
            ConfigLayer::OrganizationTeam,
            primary_and_shards(config_dir, "org.toml", "org.d"),
        ),
        (
            ConfigLayer::Project,
            primary_and_shards(project_dir, "orchestraitor.toml", "orchestraitor.d"),
        ),
        (
            ConfigLayer::DirectoryDomain,
            primary_and_shards(config_dir, "dir.toml", "dir.d"),
        ),
    ];
    for (layer, files) in layers {
        for file in files {
            if !file.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&file).into_diagnostic()?;
            resolver = resolver
                .with_toml(
                    ConfigSource {
                        layer,
                        name: file.display().to_string(),
                    },
                    &content,
                )
                .map_err(|error| miette::miette!("configuration parse failed: {error}"))?;
        }
    }
    Ok(resolver)
}

/// A layer's primary file followed by its shard directory's `*.toml` files
/// (sorted for deterministic order — the CLI sorts the same way).
fn primary_and_shards(base: &std::path::Path, primary: &str, shard_dir_name: &str) -> Vec<PathBuf> {
    let mut files = vec![base.join(primary)];
    let shard_dir = base.join(shard_dir_name);
    let Ok(entries) = std::fs::read_dir(&shard_dir) else {
        return files;
    };
    let mut shards: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("toml"))
        .collect();
    shards.sort();
    files.extend(shards);
    files
}

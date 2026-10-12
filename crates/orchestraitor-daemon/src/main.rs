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
    // The §9.47 event ingress polls these repositories (issue #504);
    // captured before the config template moves into the board poller.
    let ingress_repos = board_config_template.repos.clone();
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
    .with_min_poll_interval(Some(Duration::from_secs(watch_config.poll_interval_secs)))
    .with_kickoff(watch_config.kickoff);

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

    // The daemon is ALWAYS-RUNNING (§9.36): the 4h whole-run budget ends one
    // runner invocation, not the daemon — on RunBudgetExhausted a fresh
    // invocation (NEW id and clock origin, derived inside the loop: a reused
    // id would make excluded_tasks accumulate across 4h windows and a stale
    // clock origin would skew every guard timestamp) continues watching
    // until the operator's shutdown signal. A shutdown stop (or any other
    // terminal reason) exits.
    // The reconcile poller is built ONCE for the daemon's lifetime: its
    // `previous_blocked` state CARRIES across run-budget boundaries (a task
    // blocked on the last tick of invocation N and ready on the first tick
    // of N+1 is a promotion §9.40 requires recording), while run_watch's
    // begin_invocation re-scopes the divergence dedup set per invocation.
    // The board client and dedicated run-state/event-store handles are
    // shared for the daemon's lifetime.
    let client = orchestraitor_board::BoardClient::new(
        Arc::clone(&auth) as Arc<dyn orchestraitor_board::BoardAuth>
    )
    .into_diagnostic()
    .wrap_err("board client construction failed")?;
    let events = orchestraitor_events::SqliteAuditStore::open(config_dir.join("watch-events.db"))
        .into_diagnostic()
        .wrap_err("watch event store open failed")?;
    let poller = ReconcilePoller::new(
        orchestraitor_daemon::BoardSnapshotPoller::new(Arc::new(client), board_config_template),
        Arc::new(std::sync::Mutex::new(events)),
        Arc::new(JournallessSink),
    );
    // The §9.47 event ingress (issue #504): opt-in via
    // `[watch.ingress].enabled`. When enabled, the conditional-request
    // poller feeds the bounded queue and the kick-off source drains it —
    // the operator arms the condition separately via
    // `[watch.kickoff].external_event`. Disabled (the default) keeps the
    // trigger set board/reconcile-derived only.
    if watch_config.ingress.enabled && !watch_config.kickoff.external_event {
        // Enabled ingress with a disarmed consumer: the queue fills to
        // capacity, drops surplus, and stays `degraded` permanently (a
        // gen-13 review finding). The poller still runs (polling is the
        // operator's explicit opt-in and the reconcile poll remains the
        // completeness authority), but the operator must see the
        // mismatched configuration.
        tracing::warn!(
            "orcd watch: watch.ingress.enabled is true but \
             watch.kickoff.external_event is false - the ingress queue \
             has no kick-off consumer and will fill, drop surplus, and \
             stay degraded; arm external_event or disable the ingress"
        );
    }
    let event_source: Option<Arc<dyn orchestraitor_campaign::KickoffEventSource>> =
        if watch_config.ingress.enabled {
            let (ingress_queue, ingress_poller, source) = orchestraitor_daemon::build_ingress(
                &config_dir.join("ingress-events.db"),
                Arc::clone(&auth) as Arc<dyn orchestraitor_board::BoardAuth>,
                watch_config.ingress.queue_capacity,
            )
            .into_diagnostic()
            .wrap_err("event ingress construction failed")?;
            // The poll task shares the daemon's lifetime: one conditional
            // round per tick, sleeping the largest server-advised interval
            // (never wider than the rate-limit feedback, §9.47(b)), racing
            // the operator's shutdown signal.
            let targets =
                orchestraitor_daemon::poll_targets(&ingress_repos, "https://api.github.com");
            let poll_shutdown = signal_rx.clone();
            tokio::spawn(async move {
                ingress_poll_loop(ingress_poller, targets, ingress_queue, poll_shutdown).await;
            });
            Some(Arc::new(source))
        } else {
            None
        };
    // The worker starter carries the SAME worker-side guardrail thresholds
    // the runner validates (from the loop config just built) — the daemon's
    // enforcement layers cannot drift from the foreground loop's (a PR #564
    // review finding: the worker config never received them).
    // The prune inside the invocation loop needs the project dir after the
    // starter owns its clone.
    let prune_project_dir = project_dir.clone();
    // The spend-cap comparison in the invocation loop reads the SAME
    // pinned cap the runner enforces — cloned before the starter takes
    // its owned copy, so the two layers cannot drift.
    let daily_spend_cap_usd = budgets.daily_spend_soft_cap_usd;
    let starter = DirectWatchStarter::new(
        project_dir,
        config_dir.clone(),
        None,
        budgets,
        loop_config.guardrails.worker_guardrails(),
    );
    let mut run_ordinal = 0_u64;
    // The §9.19.6 budget window carried across invocations: the previous
    // invocation's last-pass UTC day (`None` before the first). See the
    // invocation loop below.
    let mut budget_day: Option<u64> = None;
    // Whether the operator signalled shutdown during the FINAL invocation
    // (a signal during a budget drain preempts the drain but the summary
    // keeps the budget cause — the exit code must still be a clean
    // shutdown, a PR #564 review finding).
    let signalled: bool;
    let summary = loop {
        // The run ordinal disambiguates invocation ids even if two restarts
        // land in the same second (a PR #555 review thread): a repeated id
        // would merge the exclusion scopes of two invocations.
        run_ordinal = run_ordinal.wrapping_add(1);
        // Prune leftover worktrees before EVERY invocation, not only at
        // process start: a task that ran in invocation N keeps its
        // worktree at `<config-dir>/loop-worktrees/<task-id>` (worktrees
        // are never removed on run completion), and `git worktree add`
        // into an already-registered path fails — a retry in invocation
        // N+1 would then count as a task failure and could strand the
        // task as `stuck` without ever running (a PR #564 review
        // finding). The previous invocation drained all its slots before
        // returning, so no live worktree is affected here.
        DirectWatchStarter::prune_worktrees(&config_dir, &prune_project_dir);
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
        // boundaries, row timestamps, and backoff checks). The run ordinal
        // disambiguates ids even if two restarts land in the same second.
        let start_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .into_diagnostic()
            .wrap_err("system clock is before the Unix epoch")?;
        let invocation_id = format!("watch-{start_unix_secs}-{run_ordinal}");
        // The §9.19.6 budget window crosses invocation boundaries (a gen-4
        // review fix): the previous invocation's last-pass UTC day seeds
        // this invocation's first-pass comparison, so a day boundary
        // between invocations observes the budget-window-reset kick-off
        // condition. `None` on the first invocation (no previous window).
        let previous_budget_day = budget_day.take();
        // The poller (built once, before the loop) carries its
        // previous_blocked state across budget boundaries; run_watch's
        // begin_invocation re-scopes the divergence dedup set per invocation.
        let summary = run_watch(
            loop_config.clone(),
            poller.clone(),
            starter.clone(),
            &decisions,
            &runs,
            routing.clone(),
            signal_rx.clone(),
            start_unix_secs,
            &invocation_id,
            previous_budget_day,
            event_source.clone(),
        )
        .await
        .into_diagnostic()?;
        // Hand this invocation's final budget window to the next one.
        budget_day = summary.final_budget_day;
        match summary.stop_reason {
            // Both budget stops restart the ALWAYS-ON daemon (§9.36): the
            // run budget ends one invocation, and the daily-spend soft cap
            // ends one invocation UNTIL ITS WINDOW RESETS (§9.19.6 — the
            // per-day budget window). A restart before the window resets
            // would spin: the spend stays at/above the cap for the rest of
            // the UTC day, so every restart would immediately re-stop.
            // After a spend-cap stop the loop therefore WAITS for the next
            // UTC day boundary before the restart — racing the operator's
            // shutdown signal, which ends the wait (and the daemon) at
            // once. Only then can the fresh invocation's first pass
            // observe the §9.19.6 budget-window-reset kick-off condition.
            // Restart only when no signal arrived during this invocation
            // (baseline comparison above); a non-shutdown stop still exits
            // non-zero when the restart bound is exhausted by a signal
            // race, keeping the supervisor visible.
            orchestraitor_campaign::StopReason::RunBudgetExhausted
                if *signal_rx.borrow() == signal_baseline =>
            {
                tracing::info!(
                    cycles = summary.cycles,
                    spawns = summary.spawns,
                    "orcd watch: run budget exhausted; starting the next invocation"
                );
            }
            orchestraitor_campaign::StopReason::SpendSoftCap
                if *signal_rx.borrow() == signal_baseline =>
            {
                let now_unix_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .into_diagnostic()
                    .wrap_err("system clock is before the Unix epoch")?;
                // The spend-cap drain is unbounded (in-flight runs finish
                // naturally), so it CAN cross midnight UTC: when it does,
                // the §9.19.6 window has already reset and the recorded
                // daily spend (new UTC day = empty window) is below the
                // cap — restart at once instead of idling for up to a
                // full extra day (a gen-6 review finding). Wait for the
                // next boundary only when the spend is still at or above
                // the cap.
                let daily_spend = runs
                    .daily_spend(now_unix_secs)
                    .map_err(|error| miette::miette!("daily spend read failed: {error}"))?;
                if daily_spend < daily_spend_cap_usd {
                    tracing::info!(
                        daily_spend,
                        "orcd watch: spend-cap drain crossed the UTC day \
                         boundary and the new window is below the cap; \
                         starting the next invocation"
                    );
                } else {
                    let wait = std::time::Duration::from_secs(86_400 - (now_unix_secs % 86_400));
                    tracing::info!(
                        wait_secs = wait.as_secs(),
                        daily_spend,
                        "orcd watch: daily spend cap reached; waiting for the next \
                         UTC day boundary (the §9.19.6 budget window reset) before \
                         the next invocation"
                    );
                    let mut wait_rx = signal_rx.clone();
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {
                            // The window reset: start the next invocation.
                        }
                        changed = wait_rx.changed() => {
                            // The operator signalled shutdown during the
                            // wait: a signal during a budget drain
                            // preempts the drain but the summary keeps the
                            // budget cause; the OPERATOR asked for
                            // shutdown, so exit SUCCESS below (the same
                            // rule as a signal during a drain).
                            if changed.is_ok() {
                                signalled = true;
                                break summary;
                            }
                            // Sender gone without a signal cannot happen
                            // here (the signal task owns the sender for
                            // the process's lifetime); treat it as
                            // shutdown.
                            signalled = true;
                            break summary;
                        }
                    }
                }
            }
            _ => {
                // A signal during a budget drain preempts the drain but the
                // summary keeps the budget cause (the audit trail's reason);
                // the OPERATOR asked for shutdown, so remember that here and
                // exit SUCCESS below (a PR #564 review finding: a non-zero
                // exit after SIGTERM makes `Restart=on-failure` restart the
                // daemon against the operator's shutdown).
                signalled = *signal_rx.borrow() != signal_baseline;
                break summary;
            }
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
    // A non-shutdown terminal stop (guardrails exhausted) must be VISIBLE
    // to the supervisor: exit non-zero so `Restart=on-failure` restarts
    // the daemon instead of silently staying down (a PR #555 review
    // finding). The spend soft cap and the run budget are NOT in this
    // class anymore: the invocation loop above restarts them itself (the
    // run budget ends one invocation; the spend cap ends one until its
    // §9.19.6 per-day window resets), so they only reach this line when a
    // signal raced the budget stop.
    // EXCEPT when the operator signalled shutdown during the drain: then
    // the exit is a clean shutdown (the audit rows keep the budget cause).
    if signalled || summary.stop_reason == orchestraitor_campaign::StopReason::Shutdown {
        Ok(ExitCode::SUCCESS)
    } else {
        Err(miette::miette!(
            "orcd watch stopped: {:?} ({} cycles, {} spawns, {} completed)",
            summary.stop_reason,
            summary.cycles,
            summary.spawns,
            summary.completed
        ))
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

    // The §9.36 kick-off condition enables ship as TOML data (§9.22.1:
    // built-in defaults are data, never compiled constants), layered so
    // every enable is overridable at any higher layer.
    let mut resolver = ConfigResolver::new()
        .with_toml(
            ConfigSource {
                layer: ConfigLayer::BuiltInDefaults,
                name: "built-in defaults".to_string(),
            },
            BUILT_IN_DEFAULTS,
        )
        .map_err(|error| miette::miette!("configuration parse failed: {error}"))?
        .with_toml(
            ConfigSource {
                layer: ConfigLayer::BuiltInDefaults,
                name: "built-in watch defaults".to_string(),
            },
            orchestraitor_daemon::WATCH_BUILT_IN_DEFAULTS_TOML,
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
            primary_and_shards(config_dir, "user.toml", "user.d").into_diagnostic()?,
        ),
        (
            ConfigLayer::OrganizationTeam,
            primary_and_shards(config_dir, "org.toml", "org.d").into_diagnostic()?,
        ),
        (
            ConfigLayer::Project,
            primary_and_shards(project_dir, "orchestraitor.toml", "orchestraitor.d")
                .into_diagnostic()?,
        ),
        (
            ConfigLayer::DirectoryDomain,
            primary_and_shards(config_dir, "dir.toml", "dir.d").into_diagnostic()?,
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
/// (sorted for deterministic order — the CLI sorts the same way). Only
/// `NotFound` means "no shard directory"; any other directory-read or entry
/// error propagates so a shard is never silently omitted (a PR #555 review
/// thread).
fn primary_and_shards(
    base: &std::path::Path,
    primary: &str,
    shard_dir_name: &str,
) -> std::io::Result<Vec<PathBuf>> {
    let mut files = vec![base.join(primary)];
    let shard_dir = base.join(shard_dir_name);
    let entries = match std::fs::read_dir(&shard_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error),
    };
    let mut shards: Vec<PathBuf> = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("toml"))
        .collect();
    shards.sort();
    files.extend(shards);
    Ok(files)
}

/// The §9.47(b) ingress poll loop (issue #504): one conditional-request
/// round per tick over every target, sleeping the largest server-advised
/// interval across the endpoints (the honored `X-Poll-Interval`, widened
/// ONLY by rate-limit feedback — never beyond it, §9.47(b)). Runs for
/// the daemon's lifetime; the operator's shutdown signal ends it.
///
/// Failures are per-endpoint and logged — a failed or rate-limited poll
/// backs off on the advised interval and never retries in a tight loop;
/// the §9.43 reconcile poll stays the completeness authority, so a
/// missed event never loses work.
async fn ingress_poll_loop(
    poller: orchestraitor_daemon::EventPoller,
    targets: Vec<orchestraitor_daemon::PollTarget>,
    queue: std::sync::Arc<orchestraitor_daemon::IngressQueue>,
    mut shutdown: tokio::sync::watch::Receiver<u64>,
) {
    // The observable `degraded` marker (§7.3): entered on overflow,
    // cleared after the flood-free window. Transitions are logged once.
    let mut degraded_logged = false;
    loop {
        let now_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let poll_outcomes = match poller.poll_all(&targets, now_unix_secs).await {
            Ok(outcomes) => outcomes,
            Err(error) => {
                tracing::warn!("orcd watch: ingress poll round failed: {error}");
                // The bounded fallback keeps the loop bounded (never a
                // tight retry): the documented default cadence.
                Vec::new()
            }
        };
        let mut wait_secs = 0_u64;
        for (target, outcome) in targets.iter().zip(poll_outcomes.iter()) {
            match outcome {
                Ok(poll_outcome) => {
                    if poll_outcome.dropped > 0 {
                        // §7.3: the queue dropped surplus — the degraded
                        // marker is already recorded by the queue; surface
                        // it in the log so the operator sees it.
                        tracing::warn!(
                            endpoint = %target.key,
                            dropped = poll_outcome.dropped,
                            "orcd watch: ingress queue flooded; surplus events \
                             dropped (degraded)"
                        );
                    }
                    wait_secs = wait_secs.max(poll_outcome.poll_after_secs);
                }
                Err(orchestraitor_daemon::ingress::poller::PollError::RateLimited {
                    retry_after_secs,
                }) => {
                    // Rate-limit feedback (§9.43): the wait adapts —
                    // never wider than the reset, never a silent retry.
                    tracing::warn!(
                        endpoint = %target.key,
                        retry_after_secs,
                        "orcd watch: ingress poll rate-limited; backing off"
                    );
                    wait_secs = wait_secs.max(*retry_after_secs);
                }
                Err(orchestraitor_daemon::ingress::poller::PollError::Failed) => {
                    tracing::warn!(
                        endpoint = %target.key,
                        "orcd watch: ingress poll failed; retrying after the \
                         advised interval"
                    );
                }
            }
        }
        // The observable degraded marker: log the ENTER and CLEAR
        // transitions once each (§7.3 — the marker is observable, never
        // a silent drop).
        let now_unix_secs_degraded = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let degraded_now = queue.degraded(now_unix_secs_degraded);
        if degraded_now && !degraded_logged {
            tracing::warn!(
                "orcd watch: ingress queue DEGRADED (overflow); surplus \
                 events are being dropped until the flood-free window \
                 elapses"
            );
            degraded_logged = true;
        } else if !degraded_now && degraded_logged {
            tracing::info!(
                "orcd watch: ingress queue degraded marker CLEARED after \
                 the flood-free window"
            );
            degraded_logged = false;
        }
        // The stored per-endpoint intervals already include this round's
        // feedback; the loop's wait is the largest across endpoints, at
        // least the documented default.
        let wait = std::time::Duration::from_secs(
            wait_secs.max(orchestraitor_daemon::DEFAULT_POLL_AFTER_SECS),
        );
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            changed = shutdown.changed() => {
                // A shutdown signal — or a closed sender (the daemon is
                // exiting) — ends the ingress loop at once. Returning
                // (not looping) also keeps each later signal from
                // triggering an extra immediate poll round (a gen-5
                // review finding).
                let _ = changed;
                return;
            }
        }
    }
}

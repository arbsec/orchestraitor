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
    let (board_config, _path) =
        orchestraitor_board::BoardProjectConfig::load(&project_dir).into_diagnostic()?;
    let token_uri = board_config
        .token_uri
        .clone()
        .ok_or(orchestraitor_board::BoardError::AuthNotConfigured)
        .into_diagnostic()?;
    let auth = Arc::new(orchestraitor_board::SecretUriAuth::new(token_uri));
    let client = orchestraitor_board::BoardClient::new(auth)
        .into_diagnostic()
        .wrap_err("board client construction failed")?;

    // Durable stores: the same files `orc loop` uses, so recovery and
    // history are shared between the two operating modes.
    let decisions =
        orchestraitor_campaign::CampaignDecisionStore::open(&config_dir.join("campaign.db"))
            .into_diagnostic()
            .wrap_err("campaign decision store open failed")?;
    let runs = orchestraitor_campaign::LoopRunStore::open(&config_dir.join("loop.db"))
        .into_diagnostic()
        .wrap_err("loop run-state store open failed")?;
    // Shared between the reconcile poller and the runner: `rusqlite` is
    // Send but not Sync, and the current-thread runtime means the mutex
    // is never contended (the runner awaits each poll before ticking).
    // `tokio::sync::Mutex` because the runner's borrow legitimately spans
    // awaits; the reconcile poller takes `blocking_lock()` inside its
    // synchronous scan (current-thread runtime: no contention, never
    // blocking a worker).
    let runs = Arc::new(tokio::sync::Mutex::new(runs));
    let events = orchestraitor_events::SqliteAuditStore::open(config_dir.join("watch-events.db"))
        .into_diagnostic()
        .wrap_err("watch event store open failed")?;

    // Prune leftover worktrees from previous invocations (of either
    // operating mode) before any spawn.
    DirectWatchStarter::prune_worktrees(&config_dir, &project_dir);

    // The pinned guard set: the same `WorkerBudgets` instance the runner
    // validates feeds the worker starter — the two layers cannot drift.
    let budgets = WorkerBudgets::bootstrap_defaults();
    let loop_config = orchestraitor_campaign::LoopConfig::with_cadence(
        budgets.clone(),
        Duration::from_secs(5),
        None,
        Some(Duration::from_secs(watch_config.poll_interval_secs)),
    )
    .into_diagnostic()
    .wrap_err("watch loop configuration rejected")?;

    let start_unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();

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
        loop {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ignore = signal_tx.send(1);
            }
        }
    });

    let poller = ReconcilePoller::new(
        orchestraitor_daemon::BoardSnapshotPoller::new(client, board_config),
        Arc::clone(&runs),
        Arc::new(std::sync::Mutex::new(events)),
        Arc::new(JournallessSink),
    );
    let starter = DirectWatchStarter::new(project_dir, config_dir.clone(), None, budgets);

    let runs_guard = runs.blocking_lock();
    let summary = run_watch(
        loop_config,
        poller,
        starter,
        &decisions,
        &runs_guard,
        routing,
        signal_rx,
        start_unix_secs,
    )
    .await
    .into_diagnostic()?;
    drop(runs_guard);

    signal_task.abort();
    tracing::info!(
        cycles = summary.cycles,
        spawns = summary.spawns,
        completed = summary.completed,
        "orcd watch: stopped ({:?})",
        summary.stop_reason
    );
    Ok(ExitCode::SUCCESS)
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

    let layers: [(ConfigLayer, &str, Vec<PathBuf>); 4] = [
        (
            ConfigLayer::GlobalUser,
            "user",
            vec![config_dir.join("user.toml")],
        ),
        (
            ConfigLayer::OrganizationTeam,
            "org",
            vec![config_dir.join("org.toml")],
        ),
        (
            ConfigLayer::Project,
            "project",
            vec![project_dir.join("orchestraitor.toml")],
        ),
        (
            ConfigLayer::DirectoryDomain,
            "dir",
            vec![config_dir.join("dir.toml")],
        ),
    ];
    for (layer, _name, files) in layers {
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

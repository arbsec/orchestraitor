//! `orc worker` implementation (issue #310 thin slice).
//!
//! Dispatch stays thin: the loop, tools, and budgets live in
//! `orchestraitor-worker`; this module resolves the route (the control plane
//! routes — never the worker, spec `30-model-routing.md` §9.45), loads the
//! fixture task, and renders the structured result.

use std::io::Write;

use miette::{IntoDiagnostic, Result, miette};
use orchestraitor_worker::{
    FixtureTaskSource, MediatedBashMediator, ModelId, PendingDeliverySink, ProviderId, RunStatus,
    TaskSource, WorkerBudgets, WorkerConfig, run_worker,
};

use crate::cli::{ConfigPaths, WorkerCommand, WorkerRunArgs};
use crate::commands::config::layers::load_layers;
use orchestraitor_agent_catalog::RoleRouter;

use super::{WORKER_ROLE, require_bootstrap_provider};

/// Runs an `orc worker` subcommand.
///
/// # Errors
/// Returns a diagnostic when routing, task loading, transport construction,
/// or the worker run fails. A typed worker failure prints the result JSON
/// (when requested) and returns a non-zero exit via the error path.
pub fn run<W: Write>(paths: &ConfigPaths, command: WorkerCommand, writer: &mut W) -> Result<()> {
    match command {
        WorkerCommand::Run(args) => run_task(paths, &args, writer),
    }
}

fn run_task<W: Write>(paths: &ConfigPaths, args: &WorkerRunArgs, writer: &mut W) -> Result<()> {
    let layers = load_layers(paths)?;
    let router = RoleRouter::new(&layers.resolver);
    let decision = router
        .resolve(WORKER_ROLE)
        .map_err(|error| miette!("{error}"))?;
    require_bootstrap_provider(&decision.provider)?;

    let tasks_dir = args
        .worker_tasks_dir
        .clone()
        .unwrap_or_else(|| paths.config_dir.join("worker-tasks"));
    let task = FixtureTaskSource::new(tasks_dir)
        .load(&args.task)
        .map_err(|error| miette!("{error}"))?;

    let transport = orchestraitor_worker::bootstrap::build_bootstrap_transport(
        args.worker_provider_endpoint.clone(),
    )
    .map_err(|error| miette!("{error}"))?;
    let bash = MediatedBashMediator::new();
    // Issue #656: `--deliver` wires the real push-branch delivery sink;
    // default OFF keeps the typed `Pending` bootstrap outcome.
    let delivery: std::sync::Arc<dyn orchestraitor_worker::DeliverySink> = if args.deliver {
        let repo = args
            .delivery_repo
            .clone()
            .ok_or_else(|| miette!("--deliver requires --delivery-repo owner/name"))?;
        let orc_bin = args
            .delivery_orc_bin
            .clone()
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "orc".into()));
        std::sync::Arc::new(orchestraitor_worker::PushBranchDeliverySink::new(
            orc_bin,
            repo,
            args.delivery_base
                .clone()
                .unwrap_or_else(|| "main".to_string()),
        ))
    } else {
        std::sync::Arc::new(PendingDeliverySink)
    };
    let budgets = WorkerBudgets::bootstrap_defaults();
    let mut config = WorkerConfig::new(
        ProviderId::from_string(decision.provider),
        ModelId::from_string(decision.model),
        budgets,
    );
    // Issue #656: the one-shot run delivers the ambient checkout's own
    // branch (the run operates directly in `paths.project_dir`) — resolved
    // ONLY when delivery is armed, so a `--deliver`-less run never hits the
    // branch-resolution startup gate (a fixture worktree is not a git
    // checkout).
    if args.deliver {
        config.worktree_branch = Some(current_branch_name(&paths.project_dir)?);
    }
    let surface = super::build_declared_tools(&layers.resolver, WORKER_ROLE)?;
    let config = super::attach_declared_surface(config, &surface);

    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    let run = runtime
        .block_on(run_worker(
            &task,
            &paths.project_dir,
            &transport,
            &bash,
            &*delivery,
            &config,
        ))
        .map_err(|error| miette!("{error}"))?;

    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &run).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
    } else {
        writeln!(writer, "task_id = {}", run.task_id).into_diagnostic()?;
        writeln!(writer, "status = {:?}", run.status).into_diagnostic()?;
        writeln!(writer, "attempts = {}", run.attempts).into_diagnostic()?;
        if let Some(failure) = &run.failure {
            writeln!(writer, "failure = {:?} ({})", failure.class, failure.reason)
                .into_diagnostic()?;
        }
    }

    if run.status == RunStatus::Completed {
        return Ok(());
    }
    let failure = run.failure.as_ref().map_or("unknown", |f| f.reason);
    Err(miette!("worker run failed: {failure}"))
}

/// Resolves the ambient checkout's current branch (issue #656): the
/// one-shot run delivers the branch the project directory has checked out.
/// A detached HEAD or an unreadable ref is a typed startup error — the
/// delivery must never land an unknown tree.
fn current_branch_name(project_dir: &std::path::Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(project_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|error| miette!("git rev-parse --abbrev-ref HEAD failed: {error}"))?;
    if !output.status.success() {
        return Err(miette!(
            "cannot resolve the project directory's current branch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err(miette!(
            "the project directory is on a detached HEAD; delivery needs a named branch"
        ));
    }
    Ok(branch)
}

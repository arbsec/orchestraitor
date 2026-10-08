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
    let delivery = PendingDeliverySink;
    let budgets = WorkerBudgets::bootstrap_defaults();
    let config = WorkerConfig::new(
        ProviderId::from_string(decision.provider),
        ModelId::from_string(decision.model),
        budgets,
    );
    let config = super::attach_declared_tools(config, &layers.resolver, WORKER_ROLE)?;

    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    let run = runtime
        .block_on(run_worker(
            &task,
            &paths.project_dir,
            &transport,
            &bash,
            &delivery,
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

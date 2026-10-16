//! `orc campaign` — one-shot campaign pass (spec `10-orchestrator.md` §9.35
//! thin slice, issue #313).
//!
//! Reads the reconciled board through the #308 provider, resolves the
//! implement role through the #309 router, and hands the snapshot to the
//! `orchestraitor-campaign` session, which selects at most one task, persists
//! exactly one decision record, and — for a selection — spawns the worker on
//! the daemon-less direct path (the same seams `orc worker run` uses).

use std::io::Write;
use std::sync::Arc;

use miette::{IntoDiagnostic, Result, miette};
use orchestraitor_agent_catalog::{
    RoleRouter, RoleRoutingDecision, resolve_decision_provider_with,
};
use orchestraitor_board::{BoardClient, BoardProjectConfig, SecretUriAuth};
use orchestraitor_campaign::{
    BoardSnapshot, CampaignDecisionStore, CampaignError, CampaignOutcome, WorkerSpawner,
    run_once_with_selector,
};
use orchestraitor_worker::bootstrap::build_bootstrap_transport;
use orchestraitor_worker::{
    FixtureTaskSource, MediatedBashMediator, ModelId, PendingDeliverySink, ProviderId, RunStatus,
    TaskSource, WorkerBudgets, WorkerConfig, run_worker,
};

use crate::cli::{CampaignCommand, CampaignRunArgs, ConfigPaths};
use crate::commands::config::layers::load_layers;

use super::WORKER_ROLE;

/// Production spawner: builds the bootstrap transport for the resolved
/// routing, loads the fixture task, and drives the same bounded loop
/// `orc worker run` drives.
struct DirectSpawner<'a> {
    paths: &'a ConfigPaths,
    tasks_dir: Option<std::path::PathBuf>,
    provider_endpoint: Option<String>,
    /// Declared-tool surface built ONCE at startup (the layer-trust gate,
    /// role routing, and sub-role provider gate run before any spawn); a
    /// broken tool definition is a typed startup error, never a per-spawn
    /// failure.
    declared_surface: super::DeclaredToolSurface,
    /// The delivery sink (issue #656): a completed run's worktree state
    /// lands through `orc github push-branch` + the labeling-chained PR
    /// path. `None` keeps the typed `Pending` bootstrap outcome.
    delivery: Option<Arc<orchestraitor_worker::PushBranchDeliverySink>>,
}

impl WorkerSpawner for DirectSpawner<'_> {
    fn spawn(
        &self,
        task_id: &str,
        routing: &RoleRoutingDecision,
    ) -> Result<orchestraitor_worker::WorkerRun, CampaignError> {
        if let Err(error) = super::require_bootstrap_provider(&routing.provider) {
            return Err(CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!("{error:?}"),
            });
        }
        let tasks_dir = self
            .tasks_dir
            .clone()
            .unwrap_or_else(|| self.paths.config_dir.join("worker-tasks"));
        let task = FixtureTaskSource::new(tasks_dir)
            .load(task_id)
            .map_err(|error| CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!("fixture task load failed: {error}"),
            })?;
        let transport =
            build_bootstrap_transport(self.provider_endpoint.clone()).map_err(|error| {
                CampaignError::Spawn {
                    task_id: task_id.to_string(),
                    message: format!("transport construction failed: {error}"),
                }
            })?;
        let runtime = tokio::runtime::Runtime::new().map_err(|error| CampaignError::Spawn {
            task_id: task_id.to_string(),
            message: format!("runtime construction failed: {error}"),
        })?;
        let config = WorkerConfig::new(
            ProviderId::from_string(routing.provider.clone()),
            ModelId::from_string(routing.model.clone()),
            WorkerBudgets::bootstrap_defaults(),
        );
        // Issue #656: the one-shot campaign pass delivers the ambient
        // checkout's own branch when `--deliver` is set; default OFF keeps
        // the typed `Pending` bootstrap outcome. A push failure is a typed
        // delivery outcome on the run, never a silent drop.
        let mut config = config;
        if self.delivery.is_some() {
            config.worktree_branch = Some(current_branch_name(&self.paths.project_dir).map_err(
                |error| CampaignError::Spawn {
                    task_id: task_id.to_string(),
                    message: error.to_string(),
                },
            )?);
        }
        let delivery: Arc<dyn orchestraitor_worker::DeliverySink> = match &self.delivery {
            Some(sink) => Arc::clone(sink) as Arc<dyn orchestraitor_worker::DeliverySink>,
            None => Arc::new(PendingDeliverySink),
        };
        let config = super::attach_declared_surface(config, &self.declared_surface);
        runtime
            .block_on(run_worker(
                &task,
                &self.paths.project_dir,
                &transport,
                &MediatedBashMediator::new(),
                &*delivery,
                &config,
            ))
            .map_err(|error| CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: error.to_string(),
            })
    }
}

/// Runs an `orc campaign` subcommand.
///
/// # Errors
/// Returns a diagnostic when the board read, role resolution, decision
/// store, or worker spawn fails. A typed worker failure renders the decision
/// JSON (when requested) and returns a non-zero exit via the error path.
pub fn run<W: Write>(paths: &ConfigPaths, command: CampaignCommand, writer: &mut W) -> Result<()> {
    match command {
        CampaignCommand::Run(args) => run_pass(paths, &args, writer),
    }
}

fn run_pass<W: Write>(paths: &ConfigPaths, args: &CampaignRunArgs, writer: &mut W) -> Result<()> {
    if !args.once {
        return Err(miette!(
            "only `--once` exists in this slice; the cron-shaped loop runner lands with the \
             bootstrap loop"
        ));
    }
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
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    let (open, warnings) = runtime
        .block_on(client.item_facts(&config))
        .into_diagnostic()?;
    let ready = orchestraitor_board::ready_queue(&open, &config);
    let blocked_candidates = orchestraitor_board::blocked_candidates(&open, &config);
    let snapshot = BoardSnapshot {
        open,
        ready,
        blocked_candidates,
        warnings,
    };

    let layers = load_layers(paths)?;
    let routing = RoleRouter::new(&layers.resolver)
        .resolve(WORKER_ROLE)
        .map_err(|error| miette!("{error}"))?;

    // Decision-provider consultation (spec `30-model-routing.md` §9.45, default off): when the
    // `routing.provider` flag names an implementation, it is consulted for
    // task selection first; any error falls back to the deterministic
    // selector inside the campaign session and is recorded in the decision.
    let decision_provider =
        resolve_decision_provider_with(&layers.resolver, &super::build_named_decision_provider)
            .map_err(|error| miette!("{error}"))?;
    let task_selector: Option<&dyn orchestraitor_provider_api::DecisionProvider> =
        decision_provider.as_deref();

    let store =
        CampaignDecisionStore::open(&paths.config_dir.join("campaign.db")).into_diagnostic()?;
    // The declared-tool surface is gated ONCE at startup: a broken tool
    // definition fails the invocation here, never per spawn.
    let declared_surface = super::build_declared_tools(&layers.resolver, &routing.role)
        .map_err(|error| miette!("{error}"))?;
    // Issue #656: resolve the delivery sink ONCE at startup from the
    // campaign flags. `--deliver` requires a repo; a missing orc binary is
    // a typed startup error — the delivery must never discover a missing
    // binary only after a task completes.
    let delivery: Option<Arc<orchestraitor_worker::PushBranchDeliverySink>> = if args.deliver {
        let repo = args
            .delivery_repo
            .clone()
            .ok_or_else(|| miette!("--deliver requires --delivery-repo owner/name"))?;
        let orc_bin = args
            .delivery_orc_bin
            .clone()
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "orc".into()));
        Some(Arc::new(orchestraitor_worker::PushBranchDeliverySink::new(
            orc_bin,
            repo,
            args.delivery_base
                .clone()
                .unwrap_or_else(|| "main".to_string()),
        )))
    } else {
        None
    };
    let spawner = DirectSpawner {
        paths,
        tasks_dir: args.worker_tasks_dir.clone(),
        provider_endpoint: args.worker_provider_endpoint.clone(),
        declared_surface,
        delivery,
    };
    let outcome = run_once_with_selector(&snapshot, &routing, &store, &spawner, task_selector)
        .into_diagnostic()?;

    if args.json {
        render_json(writer, &outcome)?;
    } else {
        render_text(writer, &outcome)?;
    }
    check_exit(&outcome)
}

fn render_json<W: Write>(writer: &mut W, outcome: &CampaignOutcome) -> Result<()> {
    let decision = &outcome.decision.decision;
    let payload = serde_json::json!({
        "decision_id": outcome.decision.id,
        "created_at": outcome.decision.created_at,
        "kind": decision.kind,
        "no_op_reason": decision.no_op_reason,
        "selected": decision.selected,
        "role": decision.role,
        "provider": decision.provider,
        "model": decision.model,
        "precedence_path": decision.precedence_path,
        "fallback_reason": decision.fallback_reason,
        "worker_args": decision.worker_args,
        "rationale": decision.rationale,
        "alternatives": decision.alternatives,
        "blocked_graph": decision.blocked_graph,
        "skipped": decision.skipped,
        "worker": outcome.worker,
    });
    serde_json::to_writer_pretty(&mut *writer, &payload).into_diagnostic()?;
    writeln!(writer).into_diagnostic()
}

fn render_text<W: Write>(writer: &mut W, outcome: &CampaignOutcome) -> Result<()> {
    let decision = &outcome.decision.decision;
    writeln!(writer, "decision_id = {}", outcome.decision.id).into_diagnostic()?;
    writeln!(
        writer,
        "kind = {}",
        serde_json::to_string(&decision.kind).unwrap_or_default()
    )
    .into_diagnostic()?;
    if let Some(reason) = decision.no_op_reason {
        writeln!(
            writer,
            "no_op_reason = {}",
            serde_json::to_string(&reason).unwrap_or_default()
        )
        .into_diagnostic()?;
    }
    if let Some(selected) = &decision.selected {
        writeln!(writer, "selected = {}#{}", selected.repo, selected.number).into_diagnostic()?;
        writeln!(writer, "task_id = {}", selected.task_id).into_diagnostic()?;
    }
    writeln!(
        writer,
        "provider = {} model = {}",
        decision.provider, decision.model
    )
    .into_diagnostic()?;
    writeln!(writer, "rationale = {}", decision.rationale).into_diagnostic()?;
    if !decision.skipped.is_empty() {
        writeln!(
            writer,
            "skipped = {} item(s) unevaluable (fail-closed)",
            decision.skipped.len()
        )
        .into_diagnostic()?;
    }
    if let Some(worker) = &outcome.worker {
        writeln!(writer, "worker_status = {:?}", worker.status).into_diagnostic()?;
        writeln!(writer, "worker_exit = {}", worker.exit_code).into_diagnostic()?;
    }
    Ok(())
}

/// A typed worker failure is a non-zero exit even though the decision record
/// persisted (mirrors `orc worker run`).
fn check_exit(outcome: &CampaignOutcome) -> Result<()> {
    let Some(worker) = &outcome.worker else {
        return Ok(());
    };
    if worker.status != RunStatus::Completed {
        let class = worker.failure.as_ref().map_or_else(
            || "typed-failure".to_string(),
            |failure| format!("{:?}", failure.class),
        );
        return Err(miette!(
            "worker run for task {} ended with a typed failure ({}); decision record {} persisted",
            worker.task_id,
            class,
            outcome.decision.id
        ));
    }
    Ok(())
}

/// Resolves the ambient checkout's current branch (issue #656): the
/// one-shot campaign delivery lands the branch the project directory has
/// checked out. A detached HEAD or an unreadable ref is a typed startup
/// error — the delivery must never land an unknown tree.
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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestraitor_agent_catalog::RoleRoutingDecision;

    #[test]
    fn non_neuralwatt_provider_is_a_typed_spawn_refusal() -> Result<()> {
        let temp = tempfile::tempdir().into_diagnostic()?;
        let paths = ConfigPaths {
            config_dir: temp.path().to_path_buf(),
            project_dir: temp.path().to_path_buf(),
            models_dev_endpoint: None,
            github_api_endpoint: None,
            github_graphql_endpoint: None,
            board_cache_path: None,
        };
        let spawner = DirectSpawner {
            paths: &paths,
            tasks_dir: None,
            provider_endpoint: None,
            declared_surface: crate::commands::DeclaredToolSurface {
                tools: Vec::new(),
                subsession_routing: std::collections::BTreeMap::new(),
                think_enabled: true,
            },
            delivery: None,
        };
        let routing = RoleRoutingDecision {
            role: "implement".to_string(),
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            precedence_path: "test".to_string(),
            fallback_reason: None,
        };
        match spawner.spawn("board-x-1", &routing) {
            Ok(_) => Err(miette!("non-neuralwatt provider must be refused")),
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("neuralwatt"),
                    "refusal must name the supported provider: {message}"
                );
                Ok(())
            }
        }
    }
}

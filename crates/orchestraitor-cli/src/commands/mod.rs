//! Implementations for `orc` subcommands.

use miette::bail;

pub mod board;
pub mod campaign;
pub mod chat;
pub mod config;
pub mod github;
pub mod loop_runner;
pub mod models;
pub mod pr_status;
pub mod routing;
pub mod simplify;
pub mod stats;
pub mod worker;

use miette::miette;

use orchestraitor_agent_catalog::{DecisionEndpointConfig, SYSTEMONE_DECISION_PROVIDER};

/// Builds the decision provider named by the `routing.provider` config flag
/// (spec `30-model-routing.md` §9.45). The `systemone` name selects the
/// protocol-level `SystemOneDecisionProvider` in `orchestraitor-provider-api`
/// — provider- and model-agnostic, pointed at whatever System
/// One-compatible endpoint the PROJECT's `[routing]` block names:
///
/// - `routing.base_url` — REQUIRED for `systemone` (the Neuralwatt cloud,
///   a local self-hosted Clef engine over tailscale, anything else serving
///   the protocol). Its absence is a typed build failure, never a silent
///   default URL: the endpoint is per-project operator configuration.
/// - `routing.model` — any model id the endpoint serves (default
///   `clef-flash`).
/// - `routing.api_key` — optional `secret://…` URI; absent or `none` sends
///   no `Authorization` header. The resolved credential never enters an
///   error or log line.
///
/// Names this factory does not own return `Ok(None)` (the agent-catalog
/// resolver reports the typed unknown-provider error). Configuration
/// failures surface as a typed build failure — the operator asked for a
/// decision provider, so silently degrading to the heuristic table would
/// mask a broken configuration.
///
/// # Errors
///
/// Returns the typed build failure message when the named provider cannot
/// be built.
pub(crate) fn build_named_decision_provider(
    name: &str,
    endpoint: &DecisionEndpointConfig,
) -> std::result::Result<Option<Box<dyn orchestraitor_provider_api::DecisionProvider>>, String> {
    match name {
        SYSTEMONE_DECISION_PROVIDER => {
            let base_url = endpoint.base_url.clone().ok_or_else(|| {
                "routing.base_url is required for routing.provider = \"systemone\": \
                 the System One protocol has no default endpoint — point it at \
                 your decision endpoint in the project's orchestraitor.toml"
                    .to_string()
            })?;
            let resolved = orchestraitor_provider_api::resolve_endpoint(
                base_url,
                endpoint.model.clone(),
                endpoint.api_key_uri.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            let provider = orchestraitor_provider_api::SystemOneDecisionProvider::new(resolved)
                .map_err(|error| error.to_string())?;
            Ok(Some(Box::new(provider)))
        }
        _ => Ok(None),
    }
}

/// The role the dispatched workers run as.
pub(crate) const WORKER_ROLE: &str = "implement";

/// The startup-built declared-tool surface for one role: the resolved
/// definitions and their sub-session routing evidence.
pub(crate) struct DeclaredToolSurface {
    /// The resolved definitions visible to the role.
    pub(crate) tools: Vec<orchestraitor_worker::ToolDefinition>,
    /// The sub-session routing resolved for each subagent tool's role
    /// (the §9.35 decision-record evidence).
    pub(crate) subsession_routing:
        std::collections::BTreeMap<String, orchestraitor_worker::RoleRoutingEvidence>,
    /// Whether the built-in `think` tool is on the surface (§9.48.2): a
    /// trusted-layer `[tools.think] enabled = false` entry removes it.
    pub(crate) think_enabled: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "the per-mechanism mapping (command argv, subagent role/routing/effort/budget) is the audited startup gate; splitting it would scatter the fail-fast checks the reviewer must see together"
)]
/// Builds the declared tools visible to the worker role (issue #535,
/// T2/T4). Call ONCE at startup: the layer-trust gate, mechanism
/// validation, role routing, and the sub-role provider gate all run here,
/// so a broken tool definition is a typed startup error — never a
/// per-spawn failure after worktrees exist. Role filtering happens here:
/// only tools whose `visible_to` contains the worker role are mapped.
///
/// # Errors
///
/// Returns the registry error (untrusted layer, unknown kind, unresolvable
/// role, invalid id, unknown effort) as a diagnostic.
pub(crate) fn build_declared_tools(
    resolver: &orchestraitor_core::ConfigResolver,
    role: &str,
) -> miette::Result<DeclaredToolSurface> {
    let catalog_roles: Vec<String> =
        orchestraitor_agent_catalog::RoleRegistry::from_resolver(resolver)
            .map_err(|error| miette!("{error}"))?
            .list()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
    let registry = orchestraitor_core::ToolRegistry::from_resolver(resolver, &catalog_roles, true)
        .map_err(|error| {
            // The stable ORC-CONFIG-<NNN> code rides the message (spec §9.34
            // declared-tool contract): visible and auditable, never a
            // message-only error.
            let structured = error.structured();
            miette!("{}: {}", structured.code, structured.cause)
        })?;
    let router = orchestraitor_agent_catalog::RoleRouter::new(resolver);
    let mut tools = Vec::new();
    let mut routing_map = std::collections::BTreeMap::new();
    for id in registry.ids() {
        let Some(tool) = registry.get(id) else {
            continue;
        };
        if !tool.visible_to.contains(role) {
            continue;
        }
        // Resolve the sub-session role now (fail fast at startup): a
        // subagent tool whose role cannot route is a typed error, never a
        // mid-run surprise.
        if let orchestraitor_core::ResolvedToolMechanism::Subagent { role: sub_role, .. } =
            &tool.mechanism
        {
            let decision = router
                .resolve(sub_role)
                .map_err(|error| miette!("{error}"))?;
            // The child runs on the parent's (bootstrap) transport: a
            // sub-role routed to any other provider would send the
            // sub-role's model id to the bootstrap endpoint and fail at
            // runtime with mis-attributed ledger rows — a startup error,
            // matching the parent-run gate (spec §10.3).
            require_bootstrap_provider_named(&decision.provider, sub_role)?;
            routing_map.insert(
                sub_role.clone(),
                orchestraitor_worker::RoleRoutingEvidence {
                    role: sub_role.clone(),
                    provider: decision.provider,
                    model: decision.model,
                    // The §9.35 decision-record evidence rides through: the
                    // spawn record is replayable, not provider/model-only.
                    precedence_path: decision.precedence_path,
                    fallback_reason: decision.fallback_reason,
                },
            );
        }
        // The `think` kill-switch marker (§9.48.2) is not a dispatchable
        // tool: it never enters the declared surface (the built-in tool's
        // default-on behavior is handled by the worker itself).
        if matches!(
            &tool.mechanism,
            orchestraitor_core::ResolvedToolMechanism::ThinkSwitch { .. }
        ) {
            continue;
        }
        tools.push(orchestraitor_worker::ToolDefinition {
            id: tool.id.clone(),
            mechanism: match &tool.mechanism {
                orchestraitor_core::ResolvedToolMechanism::Command { argv } => {
                    orchestraitor_worker::ToolMechanism::Command { argv: argv.clone() }
                }
                orchestraitor_core::ResolvedToolMechanism::Subagent {
                    role,
                    internal_tools,
                    instructions,
                } => orchestraitor_worker::ToolMechanism::Subagent {
                    role: role.clone(),
                    internal_tools: internal_tools
                        .iter()
                        .map(|internal| match internal {
                            orchestraitor_core::ResolvedInternalTool::ReadFile => {
                                orchestraitor_worker::InternalTool::ReadFile
                            }
                            orchestraitor_core::ResolvedInternalTool::Search => {
                                orchestraitor_worker::InternalTool::Search
                            }
                            orchestraitor_core::ResolvedInternalTool::Bash => {
                                orchestraitor_worker::InternalTool::Bash
                            }
                            orchestraitor_core::ResolvedInternalTool::Think => {
                                orchestraitor_worker::InternalTool::Think
                            }
                        })
                        .collect(),
                    instructions: instructions.clone(),
                },
                // Unreachable: ThinkSwitch markers skip the push above.
                orchestraitor_core::ResolvedToolMechanism::ThinkSwitch { .. } => {
                    continue;
                }
            },
            budget: orchestraitor_worker::ToolBudget {
                max_turns: tool.max_turns.unwrap_or(12),
                wall_clock_secs: tool.wall_clock_secs,
                max_result_bytes: tool.max_result_bytes.unwrap_or(8 * 1024),
                structured_summary: tool.structured_summary.unwrap_or(false),
            },
            visible_to: tool.visible_to.clone(),
            // The §9.45 settings ride the definition into the worker (never
            // a silent drop): effort feeds the child's model calls,
            // max_summary_bytes caps the finish summary returned to the
            // parent, structured_summary shapes the child's finish prompt.
            effort: tool.effort.map(|effort| match effort {
                orchestraitor_core::ResolvedEffort::Low => {
                    orchestraitor_provider_api::transport::ReasoningEffort::Low
                }
                orchestraitor_core::ResolvedEffort::Medium => {
                    orchestraitor_provider_api::transport::ReasoningEffort::Medium
                }
                orchestraitor_core::ResolvedEffort::High => {
                    orchestraitor_provider_api::transport::ReasoningEffort::High
                }
            }),
            max_summary_bytes: tool.max_summary_bytes,
            structured_summary: tool.structured_summary,
        });
    }
    Ok(DeclaredToolSurface {
        tools,
        subsession_routing: routing_map,
        think_enabled: registry.think_enabled(),
    })
}

/// Attaches the prebuilt declared-tool surface to a run config: the
/// startup-validated definitions and routing, applied per spawn with no
/// re-validation (the surface was gated once, at startup).
pub(crate) fn attach_declared_surface(
    mut config: orchestraitor_worker::WorkerConfig,
    surface: &DeclaredToolSurface,
) -> orchestraitor_worker::WorkerConfig {
    config.subsession_routing = surface.subsession_routing.clone();
    config.think_enabled = surface.think_enabled;
    config.with_tools(surface.tools.clone())
}

/// Fails closed when the resolved routing does not target the bootstrap
/// provider: the daemon-less direct path wires a bootstrap transport that
/// speaks only `neuralwatt` (spec §10.3). The settings layer already
/// rejects a non-configured provider id (`RoleRouter::validate_pair`
/// checks the provider allowlist); this gate covers the one provider id
/// that remains routable in configuration but unrunnable on the direct
/// path. Shared verbatim by `orc campaign run`, `orc worker run`, and
/// `orc loop` — the three entry points that spawn the direct-path worker.
///
/// # Errors
///
/// Resolves the ambient checkout's current branch (issue #656): the
/// one-shot run delivers the branch the project directory has checked out.
/// A detached HEAD or an unreadable ref is a typed startup error — the
/// delivery must never land an unknown tree.
pub(crate) fn current_branch_name(project_dir: &std::path::Path) -> miette::Result<String> {
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

/// Refuses a delivery-armed one-shot run when the ambient checkout is DIRTY
/// or checked out on the PR base branch itself (issue #656, review gen-1/2
/// majors): the sink commits the worktree state onto the checked-out
/// branch, so a dirty checkout would sweep the operator's unrelated local
/// state into the landed PR, and a base-branch checkout would land the
/// whole tree against itself. A typed startup error — never a mid-run
/// discovery.
///
/// # Errors
///
/// Returns a diagnostic naming the refusing condition.
pub(crate) fn refuse_dirty_or_base_checkout(
    project_dir: &std::path::Path,
    branch: &str,
    base_branch: &str,
) -> miette::Result<()> {
    if branch == base_branch {
        bail!(
            "--deliver refuses to run on the base branch '{base_branch}': the delivery \
             would land the whole checkout tree against itself; run on a task branch"
        );
    }
    let status = std::process::Command::new("git")
        .current_dir(project_dir)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .map_err(|error| miette!("git status failed: {error}"))?;
    if !status.status.success() {
        return Err(miette!(
            "cannot read the checkout state: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    if !output_is_blank(&status.stdout) {
        bail!(
            "--deliver refuses a DIRTY checkout: uncommitted/untracked state would be \
             committed into the landed PR; commit or stash it first"
        );
    }
    Ok(())
}

/// Blank-check: `git status --porcelain` prints nothing on a clean checkout.
fn output_is_blank(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_whitespace)
}

/// Returns the bootstrap-only-provider diagnostic carrying the resolved
/// provider id.
pub(crate) fn require_bootstrap_provider(
    provider: &str,
) -> std::result::Result<(), miette::Report> {
    require_bootstrap_provider_named(provider, WORKER_ROLE)
}

/// The bootstrap-provider gate, parameterized by the role whose routing
/// resolved to a non-bootstrap provider (the parent role, or a sub-session
/// role inside a declared subagent tool).
fn require_bootstrap_provider_named(
    provider: &str,
    role: &str,
) -> std::result::Result<(), miette::Report> {
    use orchestraitor_agent_catalog::BOOTSTRAP_PROVIDER;

    if provider != BOOTSTRAP_PROVIDER {
        return Err(miette::miette!(
            "bootstrap worker supports only the `{BOOTSTRAP_PROVIDER}` provider (spec §10.3); \
             roles.{role}.routing.provider resolved to `{provider}`"
        ));
    }
    Ok(())
}

//! Implementations for `orc` subcommands.

pub mod board;
pub mod campaign;
pub mod config;
pub mod github;
pub mod loop_runner;
pub mod models;
pub mod routing;
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

/// Builds the declared tools visible to the worker role and attaches them
/// to the run config (issue #535, T2/T4).
///
/// T4: the subagent runtime is wired (`subagent_wired = true`) and each
/// subagent tool's role resolves `(provider, model)` through the §9.45
/// `RoleRouter` chain (the control plane routes; the parent never chooses).
/// The resolved map lands on `WorkerConfig.subsession_routing`.
///
/// The registry enforces the layer-trust gate (trusted config layers only)
/// and mechanism validation at build time; an error is fatal to the spawn —
/// a broken tool definition is a typed startup failure, never a silently
/// reduced tool surface. Role filtering happens here: only tools whose
/// `visible_to` contains the worker role are mapped.
///
/// # Errors
///
/// Returns the registry error (untrusted layer, unknown kind, unresolvable
/// role, invalid id, unknown effort) as a diagnostic.
pub(crate) fn attach_declared_tools(
    mut config: orchestraitor_worker::WorkerConfig,
    resolver: &orchestraitor_core::ConfigResolver,
    role: &str,
) -> miette::Result<orchestraitor_worker::WorkerConfig> {
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
            config.subsession_routing.insert(
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
                        })
                        .collect(),
                    instructions: instructions.clone(),
                },
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
    Ok(config.with_tools(tools))
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

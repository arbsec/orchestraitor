//! The declared-tool registry (issue #535, T2): configuration turned into
//! typed tool definitions, with the layer-trust gate as its load-bearing
//! rule.
//!
//! ## Layer-trust gate
//!
//! A hostile repository can write `orchestraitor.toml` (spec
//! `40-arbitraitor-integration.md` §6.1 lists repository instructions and
//! build systems as untrusted; §9.14 classes "agent configuration" and
//! "build-system plugin" are agent-writable output). A config-declared tool
//! silently reshapes the tool surface the model sees every turn, and its
//! `instructions` blob is operator-trust scaffolding — so tool definitions
//! are honored from the **trusted layers only** (built-in defaults, plugin
//! defaults, global user, organization/team). A definition supplied by the
//! project, directory/domain, task/agent, or CLI-flag layer is a typed
//! error naming the tool id — never a silent drop (silent drops contradict
//! the §9.22.9 visible/auditable rule).
//!
//! Provenance: the merged config map flattens layers, so the gate reads
//! each tool id through [`ConfigResolver::supplying_layer_for_entry`]: the
//! highest-precedence layer that carries `tools.<id>` at all (any field).
//! This is the ONE deliberate config-read divergence from the standard
//! merged-map read; everything else merges through the standard path.
//!
//! ## Validation
//!
//! The registry fails fast at build time (typed config errors), mirroring
//! the `RoutingDecisionProviderConfig` unknown-provider posture: unknown
//! mechanism kinds, unknown internal-tool names, unknown effort values,
//! `command` tools with a `subagent_role` (and vice versa), unresolvable
//! subagent roles, reserved tool ids, and unwired mechanisms in the current
//! runtime are all typed errors.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use orchestraitor_model::error_codes::ErrorComponent;
use thiserror::Error;

use crate::OrchestraitorError;
use crate::config::{ConfigLayer, ConfigResolver, OrchestraitorConfig, ToolConfig};
use crate::error::{Retryability, StructuredError};

/// The typed failure vocabulary of the tool registry. Every variant names
/// the offending tool id or key: config errors are visible and auditable.
#[derive(Debug, Error)]
pub enum ToolRegistryError {
    /// A tool was defined in a config layer below the trust line (project,
    /// directory/domain, task/agent). Repo-supplied execution config is an
    /// untrusted-weakening vector (spec §6.1, §9.8, §9.14).
    #[error(
        "tool `{tool_id}` is defined in the untrusted `{layer}` layer; tool definitions are \
         honored from built-in defaults, plugin defaults, user, and organization/team layers only"
    )]
    UntrustedLayer {
        /// Rejected tool id.
        tool_id: String,
        /// The layer that supplied the definition.
        layer: &'static str,
    },
    /// The `kind` value matches no shipped mechanism.
    #[error("tool `{tool_id}` has unknown kind `{kind}`; expected `command` or `subagent`")]
    UnknownKind {
        /// Offending tool id.
        tool_id: String,
        /// Configured kind value.
        kind: String,
    },
    /// `command` and `subagent` fields are mixed or missing.
    #[error(
        "tool `{tool_id}` is malformed: {reason} (a `command` tool needs a non-empty `command` \
         argv and no subagent fields; a `subagent` tool needs `subagent_role`)"
    )]
    MalformedMechanism {
        /// Offending tool id.
        tool_id: String,
        /// What is wrong.
        reason: String,
    },
    /// A `subagent` tool names a role that resolves nowhere.
    #[error("tool `{tool_id}` names unknown subagent role `{role}`; known roles are: {known}")]
    UnknownSubagentRole {
        /// Offending tool id.
        tool_id: String,
        /// Configured role id.
        role: String,
        /// Comma-separated known role ids.
        known: String,
    },
    /// A role id in `visible_to` is not in the role registry.
    #[error(
        "tool `{tool_id}` names unknown role `{role}` in `visible_to`; known roles are: {known}"
    )]
    UnknownVisibleRole {
        /// Offending tool id.
        tool_id: String,
        /// Configured role id.
        role: String,
        /// Comma-separated known role ids.
        known: String,
    },
    /// `internal_tools` carries a name outside the typed enum.
    #[error(
        "tool `{tool_id}` has unknown internal tool `{name}`; allowed values are: read_file, \
         search, bash"
    )]
    UnknownInternalTool {
        /// Offending tool id.
        tool_id: String,
        /// Configured name.
        name: String,
    },
    /// An unknown `effort` value (issue #535 effort policy).
    #[error("tool `{tool_id}` has unknown effort `{effort}`; expected `low`, `medium`, or `high`")]
    UnknownEffort {
        /// Offending tool id.
        tool_id: String,
        /// Configured effort value.
        effort: String,
    },
    /// A tool id violates the identifier shape rules or reserves a
    /// protocol/coordinator name.
    #[error(
        "tool `{tool_id}` has an invalid or reserved id; ids must be 1..=64 lowercase \
             ASCII letters, digits, `-` or `_`, starting with a letter or digit, and must not \
             name a built-in or coordinator tool"
    )]
    InvalidToolId {
        /// Rejected tool id.
        tool_id: String,
    },
    /// Layered configuration resolution itself failed.
    #[error("tool registry configuration resolution failed: {0}")]
    Config(#[from] OrchestraitorError),
}

impl ToolRegistryError {
    /// Returns stable structured metadata for this registry failure (spec
    /// §9.34): a stable `ORC-CONFIG-<NNN>` code per variant, the offending
    /// tool id as the cause, and the relevant `tools.<id>` config key. The
    /// codes are part of the declared-tool contract — visible and auditable,
    /// never a message-only error.
    #[must_use]
    pub fn structured(&self) -> StructuredError {
        let relevant_tool: Option<String> = match self {
            Self::UntrustedLayer { tool_id, .. }
            | Self::UnknownKind { tool_id, .. }
            | Self::MalformedMechanism { tool_id, .. }
            | Self::UnknownSubagentRole { tool_id, .. }
            | Self::UnknownVisibleRole { tool_id, .. }
            | Self::UnknownInternalTool { tool_id, .. }
            | Self::UnknownEffort { tool_id, .. }
            | Self::InvalidToolId { tool_id } => Some(tool_id.clone()),
            Self::Config(_) => None,
        };
        let code = match self {
            Self::UntrustedLayer { .. } => ErrorComponent::Config.code(2),
            Self::UnknownKind { .. } => ErrorComponent::Config.code(3),
            Self::MalformedMechanism { .. } => ErrorComponent::Config.code(4),
            Self::UnknownSubagentRole { .. } | Self::UnknownVisibleRole { .. } => {
                ErrorComponent::Config.code(5)
            }
            Self::UnknownInternalTool { .. } => ErrorComponent::Config.code(6),
            Self::UnknownEffort { .. } => ErrorComponent::Config.code(7),
            Self::InvalidToolId { .. } => ErrorComponent::Config.code(8),
            Self::Config(_) => ErrorComponent::Config.code(1),
        };
        let mut structured = StructuredError {
            code,
            cause: self.to_string(),
            source_chain: Vec::new(),
            component: ErrorComponent::Config,
            retryability: Retryability::NeedsUserAction,
            suggested_action:
                "Fix the reported `[tools.<id>]` entry in its config layer and re-run".to_string(),
            relevant_config: relevant_tool.map(|tool_id| format!("tools.{tool_id}")),
            trace_reference: None,
        };
        let mut current: Option<&dyn std::error::Error> = std::error::Error::source(self);
        while let Some(source) = current {
            structured.source_chain.push(source.to_string());
            current = source.source();
        }
        structured
    }
}

/// The worker-facing resolved definition of one declared tool. Mirrors the
/// shape `orchestraitor_worker::ToolDefinition` maps from; core defines it
/// so the registry needs no worker dependency (core owns no I/O and no
/// worker types).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTool {
    /// Stable tool id (the `[tools.<id>]` key).
    pub id: String,
    /// Mechanism: fixed argv for command tools, or the sub-session spec.
    pub mechanism: ResolvedToolMechanism,
    /// Maximum conversation turns for one invocation (`None` = runtime
    /// default).
    pub max_turns: Option<u32>,
    /// Hard wall-clock bound for one invocation (seconds; `None` inherits
    /// the parent run's remaining deadline).
    pub wall_clock_secs: Option<u64>,
    /// Cap on the bytes of the result fed back to the parent (`None` =
    /// runtime default).
    pub max_result_bytes: Option<u64>,
    /// Orchestration roles that may invoke the tool; empty = nobody.
    pub visible_to: BTreeSet<String>,
    /// Resolved effort tier (issue #535 policy); `None` = routing default.
    pub effort: Option<ResolvedEffort>,
    /// Cap on the sub-session finish summary returned to the parent.
    pub max_summary_bytes: Option<u64>,
    /// Structured-only finish for the sub-session.
    pub structured_summary: Option<bool>,
    /// The config layer that supplied the winning definition (provenance).
    pub layer: ConfigLayer,
}

/// The resolved mechanism of one declared tool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedToolMechanism {
    /// A rule-driven command invocation: fixed argv, dispatched through the
    /// mediated bash seam.
    Command {
        /// The fixed argv.
        argv: Vec<String>,
    },
    /// A cheap-model sub-session.
    Subagent {
        /// Orchestration role id the sub-session resolves as.
        role: String,
        /// The scoped internal-tool allowlist.
        internal_tools: BTreeSet<ResolvedInternalTool>,
        /// Operator-authored instructions.
        instructions: Option<String>,
    },
}

/// The typed internal-tool allowlist (mirrors the worker enum).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ResolvedInternalTool {
    /// Read a worktree-relative file.
    ReadFile,
    /// Plain-text content search over the worktree.
    Search,
    /// Mediated bash execution.
    Bash,
}

impl ResolvedInternalTool {
    /// Parses a config string into the typed value.
    ///
    /// # Errors
    ///
    /// [`ToolRegistryError::UnknownInternalTool`] for any other name.
    pub fn parse(tool_id: &str, name: &str) -> Result<Self, ToolRegistryError> {
        match name {
            "read_file" => Ok(Self::ReadFile),
            "search" => Ok(Self::Search),
            "bash" => Ok(Self::Bash),
            other => Err(ToolRegistryError::UnknownInternalTool {
                tool_id: tool_id.to_string(),
                name: other.to_string(),
            }),
        }
    }

    /// The built-in tool name the internal tool dispatches as.
    #[must_use]
    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::Search => "search",
            Self::Bash => "bash",
        }
    }
}

/// The resolved reasoning-effort tier (issue #535 owner-approved policy).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedEffort {
    /// Minimal reasoning effort.
    Low,
    /// Balanced reasoning effort.
    Medium,
    /// Maximum reasoning effort.
    High,
}

impl ResolvedEffort {
    /// Parses a config string into the typed value.
    ///
    /// # Errors
    ///
    /// [`ToolRegistryError::UnknownEffort`] for any other value.
    pub fn parse(tool_id: &str, effort: &str) -> Result<Self, ToolRegistryError> {
        match effort {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(ToolRegistryError::UnknownEffort {
                tool_id: tool_id.to_string(),
                effort: other.to_string(),
            }),
        }
    }
}

/// The trust line: layers at or below this layer may define tools.
const TRUSTED_TOOL_LAYERS: [ConfigLayer; 4] = [
    ConfigLayer::BuiltInDefaults,
    ConfigLayer::PluginDefaults,
    ConfigLayer::GlobalUser,
    ConfigLayer::OrganizationTeam,
];

fn layer_name(layer: ConfigLayer) -> &'static str {
    match layer {
        ConfigLayer::BuiltInDefaults => "built-in-defaults",
        ConfigLayer::PluginDefaults => "plugin-defaults",
        ConfigLayer::GlobalUser => "user",
        ConfigLayer::OrganizationTeam => "org",
        ConfigLayer::Project => "project",
        ConfigLayer::DirectoryDomain => "dir",
        ConfigLayer::TaskAgent => "task-agent",
        ConfigLayer::CliFlag => "cli-flag",
    }
}

/// The declared-tool registry: the resolved definitions of every trusted
/// `[tools.<id>]` entry, keyed by tool id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ResolvedTool>,
}

impl ToolRegistry {
    /// Builds the registry from a layered configuration resolver.
    ///
    /// The gate reads per-tool provenance through `supplying_layer_for_entry`
    /// (the one deliberate divergence from the merged-map read — see the
    /// module doc) and fails on the FIRST untrusted definition, naming the
    /// tool id.
    ///
    /// `role_ids` is the known-role vocabulary for validating
    /// `subagent_role` and `visible_to` entries (the built-in six plus any
    /// configured custom roles).
    ///
    /// `subagent_wired` is false in the T2 slice (the sub-session runtime
    /// lands in T3): a `subagent` tool is a typed error until the runtime
    /// exists — never a silently unresolvable tool.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRegistryError`] for any validation failure; see the
    /// variant docs.
    pub fn from_resolver(
        resolver: &ConfigResolver,
        role_ids: &[String],
        subagent_wired: bool,
    ) -> Result<Self, ToolRegistryError> {
        // Per-tool provenance: find every tool id defined anywhere, then
        // resolve each one's winning layer.
        let merged = resolver.resolve_config()?;
        let merged_tools = merged.tools.clone();
        let mut tools = BTreeMap::new();
        let Some(defined) = merged_tools else {
            return Ok(Self { tools });
        };
        for tool_id in defined.keys() {
            // The WINNING definition's layer: the highest-precedence layer
            // that defines this tool id at all.
            let Some(winning) = resolver.supplying_layer_for_entry("tools", tool_id) else {
                continue;
            };
            if !TRUSTED_TOOL_LAYERS.contains(&winning.layer) {
                return Err(ToolRegistryError::UntrustedLayer {
                    tool_id: tool_id.clone(),
                    layer: layer_name(winning.layer),
                });
            }
            // The MERGED tool config: field-wise merge across layers is the
            // standard semantics; the gate reads the winning layer above.
            let Some(tool_config) = defined.get(tool_id) else {
                continue;
            };
            let tool = resolve_tool(
                tool_id,
                tool_config,
                role_ids,
                subagent_wired,
                winning.layer,
            )?;
            tools.insert(tool_id.clone(), tool);
        }
        Ok(Self { tools })
    }

    /// Builds the registry from one already-resolved config (the built-in
    /// defaults layer; the trusted source by construction).
    ///
    /// # Errors
    ///
    /// Returns [`ToolRegistryError`] for any validation failure.
    pub fn from_builtin(
        config: &OrchestraitorConfig,
        role_ids: &[String],
        subagent_wired: bool,
    ) -> Result<Self, ToolRegistryError> {
        let mut tools = BTreeMap::new();
        let Some(defined) = &config.tools else {
            return Ok(Self { tools });
        };
        for (tool_id, tool_config) in defined {
            let tool = resolve_tool(
                tool_id,
                tool_config,
                role_ids,
                subagent_wired,
                ConfigLayer::BuiltInDefaults,
            )?;
            tools.insert(tool_id.clone(), tool);
        }
        Ok(Self { tools })
    }

    /// Looks up one tool definition.
    #[must_use]
    pub fn get(&self, tool_id: &str) -> Option<&ResolvedTool> {
        self.tools.get(tool_id)
    }

    /// All defined tool ids, in stable (sorted) order.
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    /// Whether the registry defines no tools (the empty default).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The number of defined tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

/// Resolves one raw `ToolConfig` into a validated `ResolvedTool`.
fn resolve_tool(
    tool_id: &str,
    tool: &ToolConfig,
    role_ids: &[String],
    subagent_wired: bool,
    layer: ConfigLayer,
) -> Result<ResolvedTool, ToolRegistryError> {
    if !is_valid_tool_id(tool_id) || is_reserved_tool_id(tool_id) {
        return Err(ToolRegistryError::InvalidToolId {
            tool_id: tool_id.to_string(),
        });
    }
    let kind = tool
        .kind
        .as_deref()
        .ok_or_else(|| ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "missing `kind`".to_string(),
        })?;
    let mechanism = resolve_mechanism(tool_id, tool, kind, role_ids, subagent_wired)?;
    let effort = tool
        .effort
        .as_deref()
        .map(|effort| ResolvedEffort::parse(tool_id, effort))
        .transpose()?;
    let visible_to = resolve_visible_to(tool_id, tool, role_ids)?;
    validate_budget(tool_id, tool)?;
    Ok(ResolvedTool {
        id: tool_id.to_string(),
        mechanism,
        max_turns: tool.budget.as_ref().and_then(|b| b.max_turns),
        wall_clock_secs: tool.budget.as_ref().and_then(|b| b.wall_clock_secs),
        max_result_bytes: tool.budget.as_ref().and_then(|b| b.max_result_bytes),
        visible_to,
        effort,
        max_summary_bytes: tool.max_summary_bytes,
        structured_summary: tool.structured_summary,
        layer,
    })
}

/// Validates and resolves the mechanism half of one tool definition.
fn resolve_mechanism(
    tool_id: &str,
    tool: &ToolConfig,
    kind: &str,
    role_ids: &[String],
    subagent_wired: bool,
) -> Result<ResolvedToolMechanism, ToolRegistryError> {
    match kind {
        "command" => resolve_command_mechanism(tool_id, tool),
        "subagent" => resolve_subagent_mechanism(tool_id, tool, role_ids, subagent_wired),
        // `hybrid` is schema-reserved, unimplemented in v1 (plan D.2).
        other => Err(ToolRegistryError::UnknownKind {
            tool_id: tool_id.to_string(),
            kind: other.to_string(),
        }),
    }
}

/// Validates and resolves a `command` mechanism.
fn resolve_command_mechanism(
    tool_id: &str,
    tool: &ToolConfig,
) -> Result<ResolvedToolMechanism, ToolRegistryError> {
    if tool.subagent_role.is_some()
        || tool.internal_tools.is_some()
        || tool.instructions.is_some()
        || tool.structured_summary.is_some()
    {
        return Err(ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "a `command` tool carries subagent-only fields".to_string(),
        });
    }
    let argv = tool
        .command
        .clone()
        .ok_or_else(|| ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "`command` tool is missing its `command` argv".to_string(),
        })?;
    if argv.is_empty() || argv.iter().any(String::is_empty) {
        return Err(ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "`command` argv must be non-empty with no empty entries".to_string(),
        });
    }
    Ok(ResolvedToolMechanism::Command { argv })
}

/// Validates and resolves a `subagent` mechanism.
fn resolve_subagent_mechanism(
    tool_id: &str,
    tool: &ToolConfig,
    role_ids: &[String],
    subagent_wired: bool,
) -> Result<ResolvedToolMechanism, ToolRegistryError> {
    if !subagent_wired {
        return Err(ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "the subagent mechanism is not wired in this runtime".to_string(),
        });
    }
    if tool.command.is_some() {
        return Err(ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "a `subagent` tool carries a `command` argv".to_string(),
        });
    }
    let role = tool
        .subagent_role
        .clone()
        .ok_or_else(|| ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "`subagent` tool is missing `subagent_role`".to_string(),
        })?;
    if !role_ids.contains(&role) {
        return Err(ToolRegistryError::UnknownSubagentRole {
            tool_id: tool_id.to_string(),
            known: role_ids.join(", "),
            role,
        });
    }
    let mut internal_tools = BTreeSet::new();
    for name in tool.internal_tools.as_deref().unwrap_or_default() {
        internal_tools.insert(ResolvedInternalTool::parse(tool_id, name)?);
    }
    if internal_tools.is_empty() {
        // Read-only default (plan A.2): absent allowlist = the two
        // read-only internal tools.
        internal_tools.insert(ResolvedInternalTool::ReadFile);
        internal_tools.insert(ResolvedInternalTool::Search);
    }
    Ok(ResolvedToolMechanism::Subagent {
        role,
        internal_tools,
        instructions: tool.instructions.clone(),
    })
}

/// Validates and resolves the `visible_to` role set.
fn resolve_visible_to(
    tool_id: &str,
    tool: &ToolConfig,
    role_ids: &[String],
) -> Result<BTreeSet<String>, ToolRegistryError> {
    let mut visible_to = BTreeSet::new();
    for role in tool.visible_to.as_deref().unwrap_or_default() {
        if !role_ids.contains(role) {
            return Err(ToolRegistryError::UnknownVisibleRole {
                tool_id: tool_id.to_string(),
                known: role_ids.join(", "),
                role: role.clone(),
            });
        }
        visible_to.insert(role.clone());
    }
    Ok(visible_to)
}

/// Rejects zeroed budget values: a zero would silently disable the tool.
fn validate_budget(tool_id: &str, tool: &ToolConfig) -> Result<(), ToolRegistryError> {
    let Some(budget) = &tool.budget else {
        return Ok(());
    };
    if budget.max_turns == Some(0) {
        return Err(ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "budget.max_turns of 0 would disable the tool".to_string(),
        });
    }
    if budget.max_result_bytes == Some(0) {
        return Err(ToolRegistryError::MalformedMechanism {
            tool_id: tool_id.to_string(),
            reason: "budget.max_result_bytes of 0 would disable the tool".to_string(),
        });
    }
    Ok(())
}

/// Shared tool-id rules: the registry and the worker parser MUST apply the
/// same shape and reservation rules, so the implementation lives once in
/// `orchestraitor-model` (the common dependency of both crates) and both
/// surfaces re-export it.
pub use orchestraitor_model::{MAX_TOOL_ID_CHARS, is_reserved_tool_id, is_valid_tool_id};

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tool_tests;

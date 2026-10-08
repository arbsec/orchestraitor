//! Declarative tool definitions: the resolved, worker-facing shape of a
//! `[tools.<id>]` configuration entry (issue #535, T1).
//!
//! A declared tool is DATA, not a compiled variant: its id comes from
//! configuration (trusted layers only — the registry in
//! `orchestraitor-core` enforces that), its mechanism is one of the two
//! closed variants of [`ToolMechanism`], and every dispatch is receipted
//! through the same [`crate::tools::ToolExecutor`] seam as the four built-in
//! tools. The variant stays closed on purpose: mechanisms are
//! security-relevant and few; a trait seam would let any future crate add an
//! execution mechanism without review of the mediation path.
//!
//! v1 (boring scope): no per-call arguments beyond the question — command
//! tools carry a fixed argv, sub-agent tools take the question string only
//! (adversarial review C.4/A4: opaque args are schema-reserved but
//! rejected).

use std::collections::BTreeSet;

use serde::Serialize;

/// Maximum accepted length for a declared tool id (chars). The id becomes
/// part of receipt records and task ids; the same bound as worker task ids.
pub const MAX_TOOL_ID_CHARS: usize = 64;

/// Maximum accepted length for the sub-session question (chars). The
/// question is untrusted model output (spec `40-arbitraitor-integration.md`
/// §6.1) carried as data; the cap keeps one tool call from flooding the
/// child session's context.
pub const MAX_QUESTION_CHARS: usize = 8 * 1024;

/// The internal tools a sub-session may use. A typed enum, never free
/// strings: the allowlist is the sub-session's entire execution authority,
/// so admission must not hinge on string equality against config text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalTool {
    /// Read a worktree-relative file.
    ReadFile,
    /// Plain-text content search over the worktree.
    Search,
    /// Mediated bash execution (crosses the Arbitraitor boundary).
    Bash,
}

impl InternalTool {
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

/// How a declared tool executes. Closed: two mechanisms, audited.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolMechanism {
    /// A rule-driven command invocation: fixed argv, dispatched through the
    /// same mediated bash seam as the built-in `bash` tool (one mediation
    /// path; never a second executor).
    Command {
        /// The fixed argv. Quoting into a mediated script happens in the
        /// executor (quoting code lives in Orchestraitor, never in config).
        argv: Vec<String>,
    },
    /// A cheap-model sub-session with a scoped internal-tool allowlist and
    /// operator-authored instructions.
    Subagent {
        /// Orchestration role id the sub-session resolves as (spec
        /// `30-model-routing.md` §9.45 chain).
        role: String,
        /// The scoped internal-tool allowlist (read-only by default).
        internal_tools: BTreeSet<InternalTool>,
        /// Operator-authored instructions prepended to the task prompt.
        /// Trusted-layer content by construction of the registry's
        /// layer-trust gate.
        instructions: Option<String>,
    },
}

/// Per-invocation budget carved out for one declared tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ToolBudget {
    /// Maximum conversation turns for one sub-session invocation.
    pub max_turns: u32,
    /// Hard wall-clock bound for one invocation (seconds; `None` inherits
    /// the parent run's remaining deadline).
    pub wall_clock_secs: Option<u64>,
    /// Cap on the bytes of the result fed back to the parent (summary for
    /// subagent tools, captured output for command tools).
    pub max_result_bytes: u64,
}

impl ToolBudget {
    /// Bootstrap defaults for a declared tool: bounded but generous; an
    /// owner tightens per tool in configuration.
    #[must_use]
    pub const fn bootstrap_defaults() -> Self {
        Self {
            max_turns: 12,
            wall_clock_secs: None,
            max_result_bytes: 8 * 1024,
        }
    }
}

/// One resolved declared tool: configuration turned into a typed definition
/// the executor can dispatch and the prompt can list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolDefinition {
    /// Stable tool id (the `[tools.<id>]` key). Charset-validated at parse.
    pub id: String,
    /// How the tool executes.
    pub mechanism: ToolMechanism,
    /// Per-invocation budget.
    pub budget: ToolBudget,
    /// Orchestration roles that may invoke the tool. Empty = nobody: an
    /// undeclared visibility is a refusal (`tool-not-visible`), never an
    /// implicit grant.
    pub visible_to: BTreeSet<String>,
}

impl ToolDefinition {
    /// Whether the given orchestration role may invoke this tool.
    #[must_use]
    pub fn visible_to(&self, role: &str) -> bool {
        self.visible_to.contains(role)
    }

    /// The internal tool names a subagent mechanism allows (empty for
    /// command tools). Ordering follows the enum for stable prompts.
    #[must_use]
    pub fn internal_tool_names(&self) -> Vec<&'static str> {
        match &self.mechanism {
            ToolMechanism::Command { .. } => Vec::new(),
            ToolMechanism::Subagent { internal_tools, .. } => {
                let mut names: Vec<&'static str> =
                    internal_tools.iter().map(|t| t.tool_name()).collect();
                names.sort_unstable();
                names
            }
        }
    }

    /// The model-facing one-line description of the tool, listed in the
    /// system prompt. Static, config-derived only — no prose interpolation
    /// of untrusted content.
    #[must_use]
    pub fn prompt_line(&self) -> String {
        let shape: String = match &self.mechanism {
            ToolMechanism::Command { .. } => "run a fixed configured command".to_string(),
            ToolMechanism::Subagent { internal_tools, .. } => {
                let tools: Vec<&str> = internal_tools.iter().map(|t| t.tool_name()).collect();
                format!("spawn a scoped sub-session (tools: {})", tools.join(", "))
            }
        };
        format!(
            "{{\"tool\": \"{}\"}} — declared tool: {shape}; question (optional, capped)",
            self.id
        )
    }
}

/// Execution-time policy for one worker run: which internal tools and
/// declared tools the loop may dispatch, and the sub-session depth. Built
/// once per run from the [`WorkerConfig`](crate::result::WorkerConfig).
///
/// T1 note: the policy is carried and rejected-through (the refusal
/// receipts exercise the shape); the loop and the sub-session runtime
/// consume its fields in T2/T3. The `dead_code` expectations name exactly
/// that landing.
#[derive(Clone, Debug)]
pub(crate) struct ToolPolicy {
    /// Declared tools available to this run (already filtered to the run's
    /// role by the caller that built the config).
    #[allow(
        dead_code,
        reason = "carried for the T2 executor dispatch + T4 prompt listing"
    )]
    pub(crate) declared: Vec<ToolDefinition>,
    /// Internal tools this run's executor may dispatch. Populated for
    /// sub-session runs from the spawning tool definition's allowlist;
    /// empty for top-level runs, whose built-in tools are the bootstrap
    /// four (issue #535, T3: the allowlist is the sub-session's entire
    /// execution authority).
    pub(crate) allowed_internal: std::collections::BTreeSet<InternalTool>,
    /// Sub-session depth of this run: 0 for a top-level worker, 1 inside a
    /// sub-session. Declared-tool dispatch is refused at depth >= 1.
    #[allow(dead_code, reason = "carried for the T3 depth gate in run.rs")]
    pub(crate) subsession_depth: u8,
}

impl ToolPolicy {
    /// A policy that refuses every declared-tool dispatch (built-ins only).
    /// The bootstrap default: no `[tools]` configuration, no depth.
    #[must_use]
    pub(crate) fn built_ins_only() -> Self {
        Self {
            declared: Vec::new(),
            allowed_internal: std::collections::BTreeSet::new(),
            subsession_depth: 0,
        }
    }

    /// Looks up a declared tool by id.
    #[allow(dead_code, reason = "consumed by the T2 executor dispatch")]
    pub(crate) fn find(&self, tool_id: &str) -> Option<&ToolDefinition> {
        self.declared.iter().find(|tool| tool.id == tool_id)
    }
}

/// Validates a declared tool id: 1..=64 chars, ASCII lowercase letters,
/// digits, `-` or `_`, starting with a letter or digit — the same shape
/// rules as role ids, so a tool id can never carry a path or key separator.
#[must_use]
pub fn is_valid_tool_id(tool_id: &str) -> bool {
    let len = tool_id.chars().count();
    if len == 0 || len > MAX_TOOL_ID_CHARS {
        return false;
    }
    let mut chars = tool_id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Reserves the built-in tool names and the §9.39 coordinator tool names: a
/// declared tool can never shadow a protocol name.
#[must_use]
pub fn is_reserved_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "read_file" | "write_file" | "search" | "bash" | "finish"
    ) || matches!(
        tool_id,
        "board.query"
            | "board.move"
            | "decision.record"
            | "router.consult"
            | "worker.delegate"
            | "budget.check"
            | "capability.check"
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn tool_id_shape_rules() {
        assert!(is_valid_tool_id("explain-clippy"));
        assert!(is_valid_tool_id("review_diff"));
        assert!(is_valid_tool_id("a1"));
        assert!(!is_valid_tool_id(""));
        assert!(!is_valid_tool_id("-leading"));
        assert!(!is_valid_tool_id("has.dot"));
        assert!(!is_valid_tool_id("has/slash"));
        assert!(!is_valid_tool_id("UPPER"));
        assert!(!is_valid_tool_id(&"x".repeat(MAX_TOOL_ID_CHARS + 1)));
    }

    #[test]
    fn protocol_and_coordinator_names_are_reserved() {
        for reserved in ["bash", "finish", "read_file", "write_file", "search"] {
            assert!(is_reserved_tool_id(reserved), "{reserved} must be reserved");
        }
        for reserved in ["board.query", "worker.delegate", "decision.record"] {
            assert!(is_reserved_tool_id(reserved), "{reserved} must be reserved");
        }
        assert!(!is_reserved_tool_id("explain-clippy"));
    }

    #[test]
    fn empty_visibility_is_a_refusal_for_every_role() {
        let tool = ToolDefinition {
            id: "t".to_string(),
            mechanism: ToolMechanism::Command { argv: vec![] },
            budget: ToolBudget::bootstrap_defaults(),
            visible_to: BTreeSet::new(),
        };
        assert!(!tool.visible_to("implement"));
        assert!(!tool.visible_to("review"));
    }

    #[test]
    fn prompt_line_lists_internal_tools() {
        let tool = ToolDefinition {
            id: "explore-q".to_string(),
            mechanism: ToolMechanism::Subagent {
                role: "explore".to_string(),
                internal_tools: BTreeSet::from([InternalTool::ReadFile, InternalTool::Search]),
                instructions: None,
            },
            budget: ToolBudget::bootstrap_defaults(),
            visible_to: BTreeSet::from(["implement".to_string()]),
        };
        let line = tool.prompt_line();
        assert!(line.contains("explore-q"));
        assert!(line.contains("read_file"));
        assert!(line.contains("search"));
    }

    #[test]
    fn policy_find_matches_by_id() {
        let policy = ToolPolicy {
            declared: vec![command_tool("explain-clippy")],
            allowed_internal: BTreeSet::new(),
            subsession_depth: 0,
        };
        assert!(policy.find("explain-clippy").is_some());
        assert!(policy.find("nope").is_none());
    }

    #[test]
    fn built_ins_only_refuses_every_declared_tool() {
        let policy = ToolPolicy::built_ins_only();
        assert!(policy.declared.is_empty());
        assert_eq!(policy.subsession_depth, 0);
        assert!(policy.find("explain-clippy").is_none());
    }

    fn command_tool(id: &str) -> ToolDefinition {
        ToolDefinition {
            id: id.to_string(),
            mechanism: ToolMechanism::Command {
                argv: vec!["cargo".to_string(), "clippy".to_string()],
            },
            budget: ToolBudget::bootstrap_defaults(),
            visible_to: BTreeSet::from(["implement".to_string()]),
        }
    }
}

//! The internal-tool executor and its receipts.
//!
//! The built-in internal tools are `read_file`, `search`, `bash`,
//! `write_file`, and `think` (spec `20-harness-worker.md` §9.48). Anything
//! else is refused before reaching this executor (see `action.rs`). Every
//! dispatch produces a [`ToolReceipt`] — the per-call record required by
//! the issue acceptance criteria — carrying static reason codes only: no
//! tool arguments, command lines, or captured output (spec
//! `40-arbitraitor-integration.md` §9.23.4).
//!
//! `bash` crosses the Arbitraitor boundary through the
//! [`crate::mediator::BashMediator`] seam (issue #311); see `mediator.rs`.
//! `think` performs no mediated operation at all: zero I/O, nothing for
//! Arbitraitor to mediate (§9.48.7) — it appends to the session's own
//! thought state only.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::action::WorkerAction;
use crate::mediator::{BashMediator, MediationError};
use crate::paths::resolve_confined;
use crate::search::{search_files, truncate_chars};
use crate::think::{
    MAX_THOUGHTS_PER_SEQUENCE, REASON_BRANCHING_NOT_SUPPORTED, REASON_SECRET_SHAPED_CONTENT,
    REASON_THOUGHT_BUDGET_EXHAUSTED, REASON_THOUGHT_EMPTY, REASON_THOUGHT_TOO_LARGE,
    ReasoningRecord,
};
use crate::tooldef::InternalTool;

/// Read size cap for `read_file` (bytes).
const MAX_READ_BYTES: u64 = 1024 * 1024;
/// Write size cap for `write_file` content (bytes).
const MAX_WRITE_BYTES: usize = 1024 * 1024;
/// Observation cap per captured stream / file body fed back to the model.
const MAX_OBSERVATION_CHARS: usize = 8 * 1024;
/// Per-thought size cap (chars, §9.48.3): the 8 KiB observation idiom.
pub(crate) const MAX_THOUGHT_CHARS: usize = 8 * 1024;

/// Appended when an observation is cut to a byte cap.
const TRUNCATION_MARKER: &str = "\n[truncated]";

/// Per-tool-call record embedded in the run result.
///
/// Carries no arguments or captured output: `reason` is a static code,
/// `exit_code` is the mediated interpreter's status, and `tool` is either one
/// of the four known names or the sanitized, truncated requested name from a
/// refused call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolReceipt {
    /// 1-based call order within the run.
    pub sequence: u64,
    /// Tool name (known tool, or sanitized requested name for refusals).
    pub tool: String,
    /// Whether the call named a known, well-shaped tool.
    pub admitted: bool,
    /// Outcome class: `completed`, `refused`, or `failed`.
    pub outcome: &'static str,
    /// Static reason code for refusals/failures.
    pub reason: Option<&'static str>,
    /// Mediated interpreter exit code (`bash` only).
    pub exit_code: Option<i32>,
}

/// One dispatched tool turn: the receipt plus the model-facing observation.
pub(crate) struct ToolTurn {
    /// Observation text fed back to the model (untrusted content,
    /// marker-wrapped by the caller's prompt discipline).
    pub(crate) observation: String,
    /// Set when the mediation boundary refused or failed: the loop converts
    /// this into a run-level typed failure (fail closed; re-planning cannot
    /// restore a missing sandbox control).
    pub(crate) mediation_failure: Option<&'static str>,
}

/// Dispatches the four tools against a canonicalized worktree root and
/// records receipts and untrusted writes.
pub(crate) struct ToolExecutor<'a> {
    pub(crate) root: &'a Path,
    bash: &'a dyn BashMediator,
    pub(crate) receipts: Vec<ToolReceipt>,
    untrusted_writes: Vec<String>,
    /// Declared-tool admission policy (issue #535, T1). `built_ins_only()`
    /// for the bootstrap config; populated from the run's tool definitions
    /// when `[tools]` configuration resolves.
    #[allow(dead_code, reason = "carried for the T2 command/T3 subagent dispatch")]
    policy: crate::tooldef::ToolPolicy,
    /// The session's `think`-tool state (§9.48.3 budgets, §9.48.8 sequence
    /// ids). Per-executor, i.e. per-run at depth 0.
    thoughts: crate::think::ThoughtChain,
    /// Audit metadata of every accepted reasoning step (§9.48.8): the run
    /// result carries them so the caller can persist `agent reasoning`
    /// events; text rides only when payload recording is enabled.
    reasoning_records: Vec<ReasoningRecord>,
}

impl<'a> ToolExecutor<'a> {
    /// Creates an executor for one run.
    pub(crate) fn new(root: &'a Path, bash: &'a dyn BashMediator) -> Self {
        Self {
            root,
            bash,
            receipts: Vec::new(),
            untrusted_writes: Vec::new(),
            policy: crate::tooldef::ToolPolicy::built_ins_only(),
            thoughts: crate::think::ThoughtChain::new(),
            reasoning_records: Vec::new(),
        }
    }

    /// The audit metadata of every accepted reasoning step, in acceptance
    /// order (§9.48.8 persistence source).
    pub(crate) fn reasoning_records(&self) -> &[ReasoningRecord] {
        &self.reasoning_records
    }

    /// Attaches the run's declared-tool policy (config-resolved tools and
    /// the sub-session depth). Without this, every declared-tool dispatch
    /// is refused as `tool-not-allowed` — the behavior-neutral default.
    #[allow(dead_code, reason = "wired into the run loop by T2/T3")]
    pub(crate) fn with_policy(mut self, policy: crate::tooldef::ToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The attached declared-tool policy (the run loop and sub-session
    /// runtime consult it for admission and prompt listing).
    #[allow(dead_code, reason = "wired into the T4 prompt listing")]
    pub(crate) fn policy(&self) -> &crate::tooldef::ToolPolicy {
        &self.policy
    }

    /// Records one parent-side receipt for a completed subagent-tool
    /// dispatch (issue #535, T3): the run's receipt stream stays complete
    /// even though the sub-session runs outside the executor.
    pub(crate) fn record_subagent_completion(&mut self, tool_id: &str, completed: bool) {
        if completed {
            self.push_receipt(tool_id, true, "completed", None, None);
        } else {
            self.push_receipt(tool_id, true, "failed", Some("subsession-failed"), None);
        }
    }

    /// Records one parent-side refusal receipt for a subagent dispatch
    /// refused before any run existed.
    pub(crate) fn record_subagent_refusal(&mut self, tool_id: &str, reason: &'static str) {
        self.push_receipt(tool_id, true, "refused", Some(reason), None);
    }

    /// Consumes the executor, returning all receipts and untrusted writes.
    pub(crate) fn into_records(self) -> (Vec<ToolReceipt>, Vec<String>) {
        (self.receipts, self.untrusted_writes)
    }

    pub(crate) fn untrusted_writes(&self) -> &[String] {
        &self.untrusted_writes
    }

    /// Records one path written INSIDE a sub-session (CR finding #2): the
    /// child executor's writes must reach the parent's untrusted-output
    /// pipeline — dropped writes would hide mutations of the shared worktree
    /// from the delivery seam.
    pub(crate) fn record_child_write(&mut self, path: &str) {
        self.untrusted_writes.push(path.to_string());
    }

    /// Dispatches one parsed action. `finish` and unknown-tool refusals never
    /// reach this method — the loop records those receipts itself.
    pub(crate) async fn dispatch(&mut self, action: &WorkerAction) -> ToolTurn {
        if let Some(turn) = self.admit_internal(action) {
            return turn;
        }
        match action {
            WorkerAction::ReadFile { path } => self.read_file(path),
            WorkerAction::Search { pattern, path } => self.search(pattern, path.as_deref()),
            WorkerAction::WriteFile { path, content } => self.write_file(path, content),
            WorkerAction::Bash { script } => self.bash(script).await,
            WorkerAction::Think {
                thought,
                thought_number,
                total_thoughts,
                next_thought_needed,
                is_revision,
                revises_thought,
                branch_from_thought,
                branch_id,
            } => self.think(
                thought,
                *thought_number,
                *total_thoughts,
                *next_thought_needed,
                *is_revision,
                *revises_thought,
                branch_from_thought.is_some() || branch_id.is_some(),
            ),
            WorkerAction::DeclaredTool { tool_id, .. } => self.dispatch_declared(tool_id).await,
            WorkerAction::Finish { .. } => self.record_refusal("finish", "misrouted-finish"),
        }
    }

    /// Admits one built-in internal tool against the run's policy before
    /// dispatch (CR finding #2, CWE-863: the allowlist is enforced HERE, in
    /// the executor, not merely carried). Returns `Some(refusal)` when the
    /// dispatch is not admitted.
    ///
    /// A top-level run (depth 0) keeps the bootstrap four-tool surface. A
    /// sub-session (depth ≥ 1) may dispatch ONLY the tools carved from the
    /// spawning tool definition's allowlist — and `write_file` is not part
    /// of the grantable vocabulary at all (core's `ResolvedInternalTool`
    /// rejects it), so a sub-session can never mutate the (shared parent)
    /// worktree through the executor.
    fn admit_internal(&mut self, action: &WorkerAction) -> Option<ToolTurn> {
        // The §9.48.2 trusted-layer kill-switch applies at EVERY depth:
        // `enabled = false` removes the tool from the run's surface. Depth 0
        // otherwise bypasses the internal admission gate (the bootstrap
        // surface is unconditional).
        if !self.policy.think_enabled && matches!(action, WorkerAction::Think { .. }) {
            return Some(self.push_refusal("think", true, "tool-not-allowed"));
        }
        if self.policy.subsession_depth == 0 {
            return None;
        }
        // Only the four built-ins route through the internal admission gate;
        // declared tools keep their own audited path (depth gate first, then
        // policy lookup — do not pre-empt it here).
        match internal_action_of(action) {
            InternalAction::NotInternal => None,
            InternalAction::NeverAdmissible(name) => {
                Some(self.push_refusal(name, true, "tool-not-allowed"))
            }
            InternalAction::Allowed(tool) => {
                // The §9.48.2 trusted-layer kill-switch removes `think` at
                // every depth; the allowlist governs the other internal
                // tools (and think, inside a sub-session).
                if !self.policy.think_enabled && tool == InternalTool::Think {
                    return Some(self.push_refusal(tool.tool_name(), true, "tool-not-allowed"));
                }
                if self.policy.allowed_internal.contains(&tool) {
                    None
                } else {
                    Some(self.push_refusal(tool.tool_name(), true, "tool-not-allowed"))
                }
            }
        }
    }

    /// Admits and dispatches one declared-tool call (issue #535, T2).
    /// Admission order is the audited surface: existence in the run's
    /// policy, then sub-session depth (sub-sessions cannot spawn), then
    /// mechanism execution. Every refusal is receipted with a static reason.
    async fn dispatch_declared(&mut self, tool_id: &str) -> ToolTurn {
        if self.policy.subsession_depth > 0 {
            return self.declared_refusal(tool_id, "subsession-depth-exceeded");
        }
        // The mechanism is cloned (a small owned value: fixed argv or the
        // sub-session spec) to release the policy borrow across the await.
        let Some(mechanism) = self.policy.find(tool_id).map(|tool| tool.mechanism.clone()) else {
            return self.declared_refusal(tool_id, "tool-not-allowed");
        };
        match &mechanism {
            crate::tooldef::ToolMechanism::Command { argv } => {
                self.declared_command(tool_id, argv).await
            }
            crate::tooldef::ToolMechanism::Subagent { .. } => {
                self.declared_refusal(tool_id, "declared-tool-execution-unwired")
            }
        }
    }

    /// Records an admitted-but-refused declared-tool dispatch (T1: the
    /// execution mechanisms land in T2/T3; the action protocol and refusal
    /// shape land here so the surface is stable).
    fn declared_refusal(&mut self, tool_id: &str, reason: &'static str) -> ToolTurn {
        self.push_receipt(tool_id, true, "refused", Some(reason), None);
        ToolTurn {
            observation: format!("tool call refused: {reason}"),
            mediation_failure: None,
        }
    }

    /// Dispatches a declared `command` tool: shell-quotes the fixed argv
    /// (quoting is Orchestraitor's code, never config interpolation) and
    /// runs it through the SAME mediated bash seam as the built-in `bash`
    /// tool — one mediation path, no second executor (plan A.1). The
    /// observation is byte-capped at the tool's `budget.max_result_bytes`
    /// (the cap the budget documents for "captured output for command
    /// tools"), so an operator-set bound actually bounds the context feed.
    async fn declared_command(&mut self, tool_id: &str, argv: &[String]) -> ToolTurn {
        let script = quote_argv(argv);
        // The configured wall-clock bound is the enforced bound (never a
        // silent drop): a command that runs past it is a typed refusal, the
        // same static-reason shape as every other declared-tool refusal.
        let bound = self
            .policy
            .find(tool_id)
            .and_then(|tool| tool.budget.wall_clock_secs)
            .map(std::time::Duration::from_secs);
        let mut turn = match bound {
            Some(bound) => match tokio::time::timeout(bound, self.bash(&script)).await {
                Ok(turn) => turn,
                Err(_elapsed) => {
                    return self.declared_refusal(tool_id, "declared-tool-timeout");
                }
            },
            None => self.bash(&script).await,
        };
        // Re-label the receipt: the mediated bash receipt names `bash`; the
        // caller must see the declared tool id.
        if let Some(receipt) = self.receipts.last_mut()
            && receipt.tool == "bash"
        {
            receipt.tool = tool_id.to_string();
        }
        let cap = self.policy.find(tool_id).map_or(usize::MAX, |tool| {
            usize::try_from(tool.budget.max_result_bytes).unwrap_or(usize::MAX)
        });
        if turn.observation.len() > cap {
            // Reserve marker headroom: the WHOLE observation (cut body plus
            // marker) stays within the cap, mirroring
            // `render_subsession_summary`.
            turn.observation = crate::search::truncate_bytes(
                &turn.observation,
                cap.saturating_sub(TRUNCATION_MARKER.len()),
            );
        }
        turn
    }

    /// Records a refusal issued before dispatch (e.g. an unknown tool
    /// request) and returns the model-facing feedback text.
    pub(crate) fn record_refusal(&mut self, tool: &str, reason: &'static str) -> ToolTurn {
        self.push_receipt(tool, false, "refused", Some(reason), None);
        ToolTurn {
            observation: format!("tool call refused: {reason}"),
            mediation_failure: None,
        }
    }

    fn push_receipt(
        &mut self,
        tool: &str,
        admitted: bool,
        outcome: &'static str,
        reason: Option<&'static str>,
        exit_code: Option<i32>,
    ) {
        self.receipts.push(ToolReceipt {
            sequence: u64::try_from(self.receipts.len()).unwrap_or(u64::MAX) + 1,
            tool: tool.to_string(),
            admitted,
            outcome,
            reason,
            exit_code,
        });
    }

    /// Resolves a model-supplied path, converting confinement refusals into
    /// receipted refusals naming the dispatching tool.
    fn resolve(&mut self, tool: &'static str, path: &str) -> Result<PathBuf, ToolTurn> {
        resolve_confined(self.root, path).map_err(|rejection| {
            self.push_receipt(tool, false, "refused", Some(rejection.reason_code()), None);
            ToolTurn {
                observation: format!("tool call refused: {}", rejection.reason_code()),
                mediation_failure: None,
            }
        })
    }

    fn admitted_refusal(&mut self, tool: &'static str, reason: &'static str) -> ToolTurn {
        self.push_receipt(tool, true, "refused", Some(reason), None);
        ToolTurn {
            observation: format!("tool call refused: {reason}"),
            mediation_failure: None,
        }
    }

    /// Records an allowlist refusal with a computed (non-static) tool name:
    /// the same receipt shape as `admitted_refusal`, for admission checks
    /// that know the tool name only as a `&str`.
    fn push_refusal(&mut self, tool: &str, admitted: bool, reason: &'static str) -> ToolTurn {
        self.push_receipt(tool, admitted, "refused", Some(reason), None);
        ToolTurn {
            observation: format!("tool call refused: {reason}"),
            mediation_failure: None,
        }
    }

    fn read_file(&mut self, path: &str) -> ToolTurn {
        let resolved = match self.resolve("read_file", path) {
            Ok(resolved) => resolved,
            Err(turn) => return turn,
        };
        let Ok(metadata) = fs::metadata(&resolved) else {
            return self.admitted_refusal("read_file", "not-a-file");
        };
        if !metadata.is_file() {
            return self.admitted_refusal("read_file", "not-a-file");
        }
        if metadata.len() > MAX_READ_BYTES {
            return self.admitted_refusal("read_file", "file-too-large");
        }
        let Ok(bytes) = fs::read(&resolved) else {
            return self.admitted_refusal("read_file", "io");
        };
        let Ok(content) = String::from_utf8(bytes) else {
            return self.admitted_refusal("read_file", "non-utf8");
        };
        self.push_receipt("read_file", true, "completed", None, None);
        ToolTurn {
            observation: format!(
                "[read_file {path}]\n{}",
                truncate_chars(&content, MAX_OBSERVATION_CHARS)
            ),
            mediation_failure: None,
        }
    }

    fn search(&mut self, pattern: &str, path: Option<&str>) -> ToolTurn {
        let base = match path {
            Some(path) => match self.resolve("search", path) {
                Ok(resolved) => resolved,
                Err(turn) => return turn,
            },
            None => self.root.to_path_buf(),
        };
        if !base.is_dir() {
            return self.admitted_refusal("search", "search-root-invalid");
        }
        let matches = search_files(&base, self.root, pattern);
        self.push_receipt("search", true, "completed", None, None);
        let observation = if matches.is_empty() {
            "[search] no matches".to_string()
        } else {
            format!(
                "[search] {} match(es)\n{}",
                matches.len(),
                truncate_chars(&matches.join("\n"), MAX_OBSERVATION_CHARS)
            )
        };
        ToolTurn {
            observation,
            mediation_failure: None,
        }
    }

    fn write_file(&mut self, path: &str, content: &str) -> ToolTurn {
        if content.len() > MAX_WRITE_BYTES {
            return self.admitted_refusal("write_file", "content-too-large");
        }
        let resolved = match self.resolve("write_file", path) {
            Ok(resolved) => resolved,
            Err(turn) => return turn,
        };
        let parent_ok = resolved
            .parent()
            .is_none_or(|parent| fs::create_dir_all(parent).is_ok());
        if !parent_ok || fs::write(&resolved, content).is_err() {
            return self.admitted_refusal("write_file", "io");
        }
        // All worker output begins untrusted (spec §9.14): the written path is
        // recorded for the delivery seam; classification and promotion stay
        // with Arbitraitor and the delivery lane.
        self.untrusted_writes.push(path.to_string());
        self.push_receipt("write_file", true, "completed", None, None);
        ToolTurn {
            observation: format!("[write_file {path}] wrote {} bytes", content.len()),
            mediation_failure: None,
        }
    }

    /// The built-in `think` tool (spec `20-harness-worker.md` §9.48): one
    /// receipted deliberation step. Zero I/O — nothing for Arbitraitor to
    /// mediate (§9.48.7); the only effects are the executor's thought state,
    /// the receipt, and the audit record surfaced via
    /// [`Self::reasoning_records`] for the caller to persist as
    /// `agent reasoning` events (§9.48.8).
    ///
    /// Refusal order is the audited surface (§9.48.4 before persistence of
    /// any text; budgets before ordering so an exhausted run cannot be
    /// probed for ordering state):
    /// branching-reserved → size → budget/ordering → secret-shape.
    #[expect(
        clippy::too_many_arguments,
        reason = "the §9.48.1 parameter surface is one audited contract; grouping it into a struct would hide which fields the refusal order consumes"
    )]
    fn think(
        &mut self,
        thought: &str,
        thought_number: u32,
        total_thoughts: u32,
        next_thought_needed: bool,
        is_revision: bool,
        revises_thought: Option<u32>,
        branching_supplied: bool,
    ) -> ToolTurn {
        // Reserved v2 branching fields (§9.48.5): parsing never fails on
        // their presence; the SUPPLY of either is a typed refusal.
        if branching_supplied {
            return self.think_refusal(
                REASON_BRANCHING_NOT_SUPPORTED,
                thought,
                thought_number,
                total_thoughts,
                next_thought_needed,
                is_revision,
                revises_thought,
            );
        }
        // Per-thought size cap (§9.48.3). The scan measures the full input
        // regardless of size, so the refusal carries `thought_chars`.
        let thought_chars = thought.chars().count();
        if thought.is_empty() {
            return self.think_refusal(
                REASON_THOUGHT_EMPTY,
                thought,
                thought_number,
                total_thoughts,
                next_thought_needed,
                is_revision,
                revises_thought,
            );
        }
        if thought_chars > MAX_THOUGHT_CHARS {
            return self.think_refusal(
                REASON_THOUGHT_TOO_LARGE,
                thought,
                thought_number,
                total_thoughts,
                next_thought_needed,
                is_revision,
                revises_thought,
            );
        }
        // Budget + ordering rules (§9.48.1, §9.48.3). The sequence-ceiling
        // observation guides the agent to conclude now and finish.
        if let Err(reason) =
            self.thoughts
                .validate(thought_number, total_thoughts, is_revision, revises_thought)
        {
            let mut turn = self.think_refusal(
                reason,
                thought,
                thought_number,
                total_thoughts,
                next_thought_needed,
                is_revision,
                revises_thought,
            );
            if reason == REASON_THOUGHT_BUDGET_EXHAUSTED
                && thought_number > MAX_THOUGHTS_PER_SEQUENCE
            {
                turn.observation = format!(
                    "[think refused: {reason}] the sequence ceiling of {MAX_THOUGHTS_PER_SEQUENCE} thoughts is reached: conclude now, then finish the task"
                );
            }
            return turn;
        }
        // Secret-shaped thought text is REFUSED, not redacted (§9.48.4):
        // this check runs before any event carrying `thought_text` could be
        // produced — a secret-shaped thought's text is never written, and
        // the refusal record below carries only the validation-time char
        // count.
        if orchestraitor_core::contains_secret_shaped(thought) {
            return self.think_refusal(
                REASON_SECRET_SHAPED_CONTENT,
                thought,
                thought_number,
                total_thoughts,
                next_thought_needed,
                is_revision,
                revises_thought,
            );
        }
        // Accepted: append to the chain and record the audit metadata.
        self.thoughts
            .accept(thought_number, next_thought_needed, is_revision);
        let record = ReasoningRecord {
            accepted: true,
            refusal_reason: None,
            thought_number,
            total_thoughts,
            next_thought_needed,
            is_revision,
            revises_thought: if is_revision { revises_thought } else { None },
            thought_chars,
            sequence_id: self.thoughts.sequence_id(),
            // Consent gate at the SOURCE (§9.48.8; spec 50 §20.4.2): the
            // text never enters the record — and therefore no serializer,
            // run result, or event payload — unless payload recording is
            // enabled. The model still holds the thought in its context;
            // only the persisted copy is gated.
            thought_text: self.policy.record_payloads.then(|| thought.to_string()),
        };
        self.reasoning_records.push(record);
        self.push_receipt("think", true, "completed", None, None);
        // Output is a compact state echo — an audit line, NEVER the thought
        // text (§9.48.1; the model already holds its own thought in context).
        let sequence = self.thoughts.sequence_id();
        ToolTurn {
            observation: format!(
                "[think {thought_number}/{total_thoughts} next={next_thought_needed}] sequence={sequence} thoughts={} budget={MAX_THOUGHTS_PER_SEQUENCE}",
                self.thoughts.run_count()
            ),
            mediation_failure: None,
        }
    }

    /// Records one receipted `think` refusal: the receipt plus an
    /// observation carrying the typed reason code. The thought's TEXT never
    /// enters the record or the observation — only the validation-time
    /// `thought_chars` count (§9.48.4). A refusal never ends the run and
    /// never raises an exception into the loop (§9.48.1).
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the §9.48.1 parameter surface so the refusal record is complete"
    )]
    fn think_refusal(
        &mut self,
        reason: &'static str,
        thought: &str,
        thought_number: u32,
        total_thoughts: u32,
        next_thought_needed: bool,
        is_revision: bool,
        revises_thought: Option<u32>,
    ) -> ToolTurn {
        self.reasoning_records.push(ReasoningRecord {
            accepted: false,
            refusal_reason: Some(reason),
            thought_number,
            total_thoughts,
            next_thought_needed,
            is_revision,
            revises_thought: if is_revision { revises_thought } else { None },
            thought_chars: thought.chars().count(),
            sequence_id: self.thoughts.sequence_id(),
            // The refusal event never contains the rejected thought
            // (§9.48.4): text stays `None` for every refusal code.
            thought_text: None,
        });
        self.push_receipt("think", true, "refused", Some(reason), None);
        ToolTurn {
            observation: format!("[think refused: {reason}]"),
            mediation_failure: None,
        }
    }

    async fn bash(&mut self, script: &str) -> ToolTurn {
        match self.bash.run_bash(script).await {
            Ok(run) => {
                let exit_text = run
                    .exit_code
                    .map_or_else(|| "signal".to_string(), |code| code.to_string());
                self.push_receipt("bash", true, "completed", None, run.exit_code);
                ToolTurn {
                    observation: format!(
                        "[bash exit {exit_text}]\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        truncate_chars(
                            &String::from_utf8_lossy(&run.stdout),
                            MAX_OBSERVATION_CHARS
                        ),
                        truncate_chars(
                            &String::from_utf8_lossy(&run.stderr),
                            MAX_OBSERVATION_CHARS
                        ),
                    ),
                    mediation_failure: None,
                }
            }
            Err(error) => {
                let reason = mediation_reason(&error);
                self.push_receipt("bash", true, "failed", Some(reason), None);
                ToolTurn {
                    observation: format!("mediated bash failed closed: {reason}"),
                    mediation_failure: Some(reason),
                }
            }
        }
    }
}

/// How a parsed action classifies for sub-session internal admission.
enum InternalAction {
    /// An allowlistable internal tool (checked against the carved set).
    Allowed(InternalTool),
    /// `write_file`: no `InternalTool` variant exists, so inside a
    /// sub-session it is never admissible (the shared parent worktree must
    /// not be mutable through a child).
    NeverAdmissible(&'static str),
    /// Not an internal built-in (declared tools, finish): keeps its own
    /// audited dispatch path.
    NotInternal,
}

/// Classifies a parsed action for sub-session internal admission.
fn internal_action_of(action: &WorkerAction) -> InternalAction {
    match action {
        WorkerAction::ReadFile { .. } => InternalAction::Allowed(InternalTool::ReadFile),
        WorkerAction::Search { .. } => InternalAction::Allowed(InternalTool::Search),
        WorkerAction::Bash { .. } => InternalAction::Allowed(InternalTool::Bash),
        WorkerAction::Think { .. } => InternalAction::Allowed(InternalTool::Think),
        WorkerAction::WriteFile { .. } => InternalAction::NeverAdmissible("write_file"),
        WorkerAction::DeclaredTool { .. } | WorkerAction::Finish { .. } => {
            InternalAction::NotInternal
        }
    }
}

/// Shell-quotes one argv into a single `exec` line. Every argument is
/// single-quoted with embedded `'` escaped — the quoting lives HERE, in
/// Orchestraitor code, never in config text; adversarial property tests
/// cover metacharacter-bearing argv (spec §21.1).
#[must_use]
pub(crate) fn quote_argv(argv: &[String]) -> String {
    let mut line = String::from("exec");
    for arg in argv {
        line.push(' ');
        line.push('\'');
        for c in arg.chars() {
            if c == '\'' {
                // End the quoted segment, escape the quote, reopen.
                line.push_str("'\\''");
            } else {
                line.push(c);
            }
        }
        line.push('\'');
    }
    line
}

/// Maps a mediation failure to a static, log-safe reason code.
fn mediation_reason(error: &MediationError) -> &'static str {
    match error {
        MediationError::UnsupportedPlatform { .. } => "mediation-unsupported-platform",
        MediationError::UnavailableControls { .. } => "mediation-unavailable-controls",
        MediationError::Context { .. } => "mediation-context",
        MediationError::Bash { .. } => "mediation-bash",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::mediator::{BashMediator, MediatedRun};
    use async_trait::async_trait;

    /// `MediationError` is not `Clone`, so the fixture stores a mode and
    /// builds the outcome per call.
    enum FixtureMode {
        Ok,
        Refused,
    }

    struct FixtureBash {
        mode: FixtureMode,
    }

    #[async_trait]
    impl BashMediator for FixtureBash {
        async fn run_bash(&self, _script: &str) -> Result<MediatedRun, MediationError> {
            match self.mode {
                FixtureMode::Ok => Ok(MediatedRun {
                    exit_code: Some(0),
                    stdout: b"hello\n".to_vec(),
                    stderr: Vec::new(),
                }),
                FixtureMode::Refused => Err(MediationError::Context {
                    reason: "test-fixture-mediation-unavailable",
                }),
            }
        }
    }

    fn fixture_bash_ok() -> FixtureBash {
        FixtureBash {
            mode: FixtureMode::Ok,
        }
    }

    #[tokio::test]
    async fn write_then_read_round_trips_through_confined_paths() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);

        let _write = executor
            .dispatch(&WorkerAction::WriteFile {
                path: "src/note.txt".to_string(),
                content: "hello worker".to_string(),
            })
            .await;
        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");

        let read = executor
            .dispatch(&WorkerAction::ReadFile {
                path: "src/note.txt".to_string(),
            })
            .await;
        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");
        assert!(read.observation.contains("hello worker"));

        let (receipts, writes) = executor.into_records();
        assert_eq!(receipts.len(), 2);
        assert_eq!(writes, ["src/note.txt"]);
    }

    #[tokio::test]
    async fn write_escape_is_refused_and_nothing_is_written() {
        // spec 50-contracts-data.md §21.4: assert the forbidden effect did NOT
        // happen — the file must not exist, not merely an error surfaced.
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside.txt");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(&worktree, &bash);

        let _turn = executor
            .dispatch(&WorkerAction::WriteFile {
                path: "../outside.txt".to_string(),
                content: "escape".to_string(),
            })
            .await;

        assert_eq!(executor.receipts.last().unwrap().outcome, "refused");
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some("path-escape")
        );
        assert!(!outside.exists(), "escaping write must not materialize");
        assert_eq!(executor.into_records().1, [] as [String; 0]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_through_planted_symlink_is_refused_and_not_followed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside.txt");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        symlink(&outside, worktree.join("planted.txt")).unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(&worktree, &bash);

        let _turn = executor
            .dispatch(&WorkerAction::WriteFile {
                path: "planted.txt".to_string(),
                content: "escape".to_string(),
            })
            .await;

        assert_eq!(executor.receipts.last().unwrap().outcome, "refused");
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some("path-symlink")
        );
        assert!(!outside.exists(), "symlink write must not be followed");
    }

    #[tokio::test]
    async fn bash_dispatches_through_the_mediator_and_records_exit_code() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);

        let turn = executor
            .dispatch(&WorkerAction::Bash {
                script: "echo hello".to_string(),
            })
            .await;

        assert_eq!(executor.receipts.last().unwrap().tool, "bash");
        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");
        assert_eq!(executor.receipts.last().unwrap().exit_code, Some(0));
        assert!(turn.observation.contains("hello"));
        assert!(turn.mediation_failure.is_none());
    }

    #[tokio::test]
    async fn bash_mediation_refusal_is_typed_and_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let bash = FixtureBash {
            mode: FixtureMode::Refused,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash);

        let turn = executor
            .dispatch(&WorkerAction::Bash {
                script: "true".to_string(),
            })
            .await;

        assert_eq!(executor.receipts.last().unwrap().outcome, "failed");
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some("mediation-context")
        );
        assert_eq!(turn.mediation_failure, Some("mediation-context"));
    }

    #[tokio::test]
    async fn search_finds_matches_in_sorted_order_and_skips_git() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("b")).unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join("a.txt"), "needle one\n").unwrap();
        fs::write(temp.path().join("b/c.txt"), "needle two\n").unwrap();
        fs::write(temp.path().join(".git/config"), "needle hidden\n").unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);

        let turn = executor
            .dispatch(&WorkerAction::Search {
                pattern: "needle".to_string(),
                path: None,
            })
            .await;

        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");
        assert!(turn.observation.contains("a.txt:1: needle one"));
        assert!(turn.observation.contains("b/c.txt:1: needle two"));
        assert!(
            !turn.observation.contains("hidden"),
            "search must not descend into .git"
        );
        let a_pos = turn.observation.find("a.txt").unwrap();
        let c_pos = turn.observation.find("b/c.txt").unwrap();
        assert!(a_pos < c_pos, "matches must be in sorted path order");
    }

    fn command_tool(id: &str) -> crate::tooldef::ToolDefinition {
        crate::tooldef::ToolDefinition {
            id: id.to_string(),
            mechanism: crate::tooldef::ToolMechanism::Command {
                argv: vec!["cargo".to_string(), "clippy".to_string()],
            },
            budget: crate::tooldef::ToolBudget::bootstrap_defaults(),
            visible_to: std::collections::BTreeSet::from(["implement".to_string()]),
            effort: None,
            max_summary_bytes: None,
            structured_summary: None,
        }
    }

    #[tokio::test]
    async fn declared_tool_without_policy_is_refused_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);

        let turn = executor
            .dispatch(&WorkerAction::DeclaredTool {
                tool_id: "explain-clippy".to_string(),
                question: None,
            })
            .await;

        assert!(turn.observation.contains("tool-not-allowed"));
        assert!(turn.mediation_failure.is_none());
        let receipt = executor.receipts.last().unwrap();
        assert_eq!(receipt.tool, "explain-clippy");
        assert!(receipt.admitted);
        assert_eq!(receipt.outcome, "refused");
    }

    #[tokio::test]
    async fn declared_command_tool_executes_through_the_mediator() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let policy = crate::tooldef::ToolPolicy {
            declared: vec![command_tool("explain-clippy")],
            allowed_internal: std::collections::BTreeSet::new(),
            subsession_depth: 0,
            think_enabled: true,
            record_payloads: true,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash).with_policy(policy);

        // T2: the command mechanism runs through the SAME mediated seam as
        // the built-in bash tool, receipted under the declared tool id.
        let turn = executor
            .dispatch(&WorkerAction::DeclaredTool {
                tool_id: "explain-clippy".to_string(),
                question: None,
            })
            .await;
        assert!(turn.observation.contains("hello"));
        assert!(turn.mediation_failure.is_none());
        let receipt = executor.receipts.last().unwrap();
        assert_eq!(receipt.tool, "explain-clippy");
        assert_eq!(receipt.outcome, "completed");
        assert_eq!(receipt.exit_code, Some(0));
    }

    #[tokio::test]
    async fn declared_tool_unknown_to_policy_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let policy = crate::tooldef::ToolPolicy {
            declared: vec![command_tool("explain-clippy")],
            allowed_internal: std::collections::BTreeSet::new(),
            subsession_depth: 0,
            think_enabled: true,
            record_payloads: true,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash).with_policy(policy);

        let turn = executor
            .dispatch(&WorkerAction::DeclaredTool {
                tool_id: "nope".to_string(),
                question: None,
            })
            .await;
        assert!(turn.observation.contains("tool-not-allowed"));
        let receipt = executor.receipts.last().unwrap();
        assert_eq!(receipt.tool, "nope");
        assert_eq!(receipt.outcome, "refused");
    }

    #[tokio::test]
    async fn declared_tool_inside_subsession_is_depth_refused() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let policy = crate::tooldef::ToolPolicy {
            declared: vec![command_tool("explain-clippy")],
            allowed_internal: std::collections::BTreeSet::new(),
            subsession_depth: 1,
            think_enabled: true,
            record_payloads: true,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash).with_policy(policy);

        // Depth-1 negative (issue #535): a sub-session can never dispatch a
        // declared tool — no nested spawns, structurally.
        let turn = executor
            .dispatch(&WorkerAction::DeclaredTool {
                tool_id: "explain-clippy".to_string(),
                question: None,
            })
            .await;
        assert!(turn.observation.contains("subsession-depth-exceeded"));
        assert!(executor.policy().find("explain-clippy").is_some());
    }

    #[test]
    fn quote_argv_is_injection_safe() {
        // Adversarial argv (spec §21.1 negative): metacharacters, quotes,
        // command substitution, semicolons, newlines. The strongest check
        // is the round-trip test below; this one asserts the per-argument
        // structure: each argument contributes exactly one opening quote,
        // and the ONLY `'` sequences inside segments are the escape.
        let argv = vec![
            "echo".to_string(),
            "it's".to_string(),
            "$(rm -rf /)".to_string(),
            "a; b".to_string(),
            "line1\nline2".to_string(),
            "'quoted'".to_string(),
        ];
        let script = quote_argv(&argv);
        assert!(script.starts_with("exec "));
        // Reconstruct the argv by shell-quoting each argument independently
        // and comparing: the joined script must equal quote_argv applied to
        // each piece — i.e. quoting is per-argument, never cross-argument.
        let args_part = script.strip_prefix("exec ").unwrap();
        let mut cursor = 0usize;
        for (index, arg) in argv.iter().enumerate() {
            let expected_segment = {
                let mut one = String::new();
                one.push('\'');
                for c in arg.chars() {
                    if c == '\'' {
                        one.push_str("'\\''");
                    } else {
                        one.push(c);
                    }
                }
                one.push('\'');
                one
            };
            let rest = &args_part[cursor..];
            let Some(offset) = rest.find(&expected_segment) else {
                panic!("argument {index} ({arg:?}) not found as a quoted segment in: {script}");
            };
            cursor += offset + expected_segment.len();
            // Exactly one separator space follows each non-final segment.
            if index + 1 < argv.len() {
                assert_eq!(
                    args_part.get(cursor..cursor + 1),
                    Some(" "),
                    "arguments must be space-separated: {script}"
                );
                cursor += 1;
            }
        }
        assert_eq!(
            cursor,
            args_part.len(),
            "script has trailing content: {script}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn quote_argv_round_trips_through_a_real_shell() {
        // The strongest negative: the forbidden effect did NOT happen. Run
        // the quoted argv through the system shell and observe the exact
        // argument echo — a quoting bug would splice arguments or execute.
        let argv = vec![
            "printf".to_string(),
            "%s\n".to_string(),
            "it's; $(echo pwned) `echo pwned`".to_string(),
        ];
        let script = quote_argv(&argv);
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout, "it's; $(echo pwned) `echo pwned`\n",
            "argv must survive the shell as ONE literal argument"
        );
    }

    #[test]
    fn built_ins_only_policy_is_the_default() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let executor = ToolExecutor::new(temp.path(), &bash);
        assert!(executor.policy().declared.is_empty());
        assert_eq!(executor.policy().subsession_depth, 0);
    }

    // §9.48 think-tool tests: contract, budgets, secret-shape refusal
    // (negative + adversarial per spec 50 §21.4), branching reservation.

    fn think_action(
        thought: &str,
        thought_number: u32,
        total_thoughts: u32,
        next: bool,
    ) -> WorkerAction {
        WorkerAction::Think {
            thought: thought.to_string(),
            thought_number,
            total_thoughts,
            next_thought_needed: next,
            is_revision: false,
            revises_thought: None,
            branch_from_thought: None,
            branch_id: None,
        }
    }

    #[tokio::test]
    async fn think_accepts_a_linear_chain_and_returns_compact_state() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);

        let turn = executor
            .dispatch(&think_action("first step: read the spec", 1, 3, true))
            .await;
        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");
        assert_eq!(executor.receipts.last().unwrap().tool, "think");
        // Compact state echo — never the thought text (§9.48.1).
        assert_eq!(
            turn.observation,
            "[think 1/3 next=true] sequence=1 thoughts=1 budget=256"
        );

        executor
            .dispatch(&think_action("second step", 2, 3, true))
            .await;
        executor
            .dispatch(&think_action("final step", 3, 3, false))
            .await;
        assert_eq!(executor.receipts.len(), 3);
        // Consent gate at the source (§9.48.8; spec 50 §20.4.2): the
        // default-off executor persists structure, never text.
        assert_eq!(executor.reasoning_records().len(), 3);
        let record = &executor.reasoning_records()[2];
        assert!(record.accepted);
        assert_eq!(record.refusal_reason, None);
        assert_eq!(record.thought_text, None);
        assert_eq!(record.thought_chars, "final step".len());
        assert_eq!(record.sequence_id, 1);
    }

    #[tokio::test]
    async fn think_text_rides_the_record_only_under_payload_consent() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let policy = crate::tooldef::ToolPolicy {
            declared: Vec::new(),
            allowed_internal: std::collections::BTreeSet::new(),
            subsession_depth: 0,
            think_enabled: true,
            record_payloads: true,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash).with_policy(policy);
        executor
            .dispatch(&think_action("consented step", 1, 3, true))
            .await;
        let record = executor.reasoning_records().first().unwrap();
        assert!(record.accepted);
        assert_eq!(record.refusal_reason, None);
        assert_eq!(record.thought_text.as_deref(), Some("consented step"));
    }

    #[tokio::test]
    async fn think_refuses_regression_and_receipts_typed_reason() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        executor
            .dispatch(&think_action("step one", 1, 3, true))
            .await;

        let turn = executor
            .dispatch(&think_action("regressed", 1, 3, true))
            .await;
        let receipt = executor.receipts.last().unwrap();
        assert_eq!(receipt.outcome, "refused");
        assert_eq!(
            receipt.reason,
            Some(crate::think::REASON_THOUGHT_NUMBER_REGRESSED)
        );
        assert_eq!(
            turn.observation,
            "[think refused: thought-number-regressed]"
        );
        // The refusal record carries metadata, never the rejected text.
        let record = executor.reasoning_records().last().unwrap();
        assert_eq!(record.thought_text, None);
        assert_eq!(record.thought_chars, "regressed".len());
    }

    #[tokio::test]
    async fn think_refuses_empty_thought() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        let turn = executor.dispatch(&think_action("", 1, 3, true)).await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some(crate::think::REASON_THOUGHT_EMPTY)
        );
        assert_eq!(turn.observation, "[think refused: thought-empty]");
    }

    #[tokio::test]
    async fn think_refuses_oversized_thought_with_char_count() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        let oversized = "x".repeat(MAX_THOUGHT_CHARS + 1);
        let turn = executor
            .dispatch(&think_action(&oversized, 1, 3, true))
            .await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some(crate::think::REASON_THOUGHT_TOO_LARGE)
        );
        // The observation is the refusal line only — never the oversized text.
        assert_eq!(turn.observation, "[think refused: thought-too-large]");
        let record = executor.reasoning_records().last().unwrap();
        assert_eq!(record.thought_chars, MAX_THOUGHT_CHARS + 1);
        assert_eq!(record.thought_text, None);
    }

    #[tokio::test]
    async fn think_refuses_above_sequence_ceiling_with_concluding_observation() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        let turn = executor
            .dispatch(&think_action("way past the ceiling", 257, 300, true))
            .await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some(crate::think::REASON_THOUGHT_BUDGET_EXHAUSTED)
        );
        // The refusal observation guides the agent to conclude (§9.48.3).
        assert!(turn.observation.contains("conclude now"));
        assert!(turn.observation.contains("256"));
    }

    #[tokio::test]
    async fn think_revision_requires_known_target_and_reopens_closed_sequence() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        // Revision of an unknown target is refused.
        let turn = executor
            .dispatch(&WorkerAction::Think {
                thought: "revising nothing".to_string(),
                thought_number: 1,
                total_thoughts: 3,
                next_thought_needed: true,
                is_revision: true,
                revises_thought: Some(2),
                branch_from_thought: None,
                branch_id: None,
            })
            .await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some(crate::think::REASON_REVISION_TARGET_UNKNOWN)
        );
        assert_eq!(turn.observation, "[think refused: revision-target-unknown]");

        executor
            .dispatch(&think_action("step one", 1, 3, true))
            .await;
        executor
            .dispatch(&think_action("step two", 2, 3, false))
            .await;
        // A revision re-opens the closed sequence.
        let turn = executor
            .dispatch(&WorkerAction::Think {
                thought: "step two reconsidered".to_string(),
                thought_number: 2,
                total_thoughts: 3,
                next_thought_needed: true,
                is_revision: true,
                revises_thought: Some(2),
                branch_from_thought: None,
                branch_id: None,
            })
            .await;
        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");
        let record = executor.reasoning_records().last().unwrap();
        assert!(record.is_revision);
        assert_eq!(record.revises_thought, Some(2));
        assert!(turn.observation.contains("next=true"));
        // A non-revision call with a `revises_thought` value persists null.
        executor
            .dispatch(&think_action("step three", 3, 3, false))
            .await;
        assert_eq!(
            executor.reasoning_records().last().unwrap().revises_thought,
            None
        );
    }

    #[tokio::test]
    async fn think_closed_sequence_then_new_one_starts_fresh_and_budget_counts() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        executor
            .dispatch(&think_action("seq 1 step 1", 1, 2, false))
            .await;
        executor
            .dispatch(&think_action("seq 2 step 1", 1, 2, true))
            .await;
        let record = executor.reasoning_records().last().unwrap();
        assert_eq!(record.sequence_id, 2);
        assert_eq!(
            executor.receipts.last().unwrap().outcome,
            "completed",
            "a fresh sequence after a closed one is accepted (§9.48.3)"
        );
    }

    #[tokio::test]
    async fn think_secret_shaped_thoughts_are_refused_not_redacted() {
        // Adversarial §9.23.4 shapes (spec 50 §21.4: assert the forbidden
        // effect — persistence of the secret — did NOT happen).
        let secret_thoughts = [
            ("sk", "the key is sk-proj-abcdefghijklmnop for the api"),
            (
                "secret-uri",
                "resolve secret://env/NEURALWATT_API_KEY before calling",
            ),
            (
                "bearer",
                "send Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig",
            ),
            ("github", "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop"),
            ("header", "x-api-key: abcdefghijklmnopqrstuvwxyz123456"),
            ("hex", "digest a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6 matches"),
            (
                "base64",
                "signature dGhpc2lzYWJhc2U2NHNpZ25hdHVyZXZhbHVlMTIzNDU2Nzg5MA== here",
            ),
        ];
        for (label, secret) in secret_thoughts {
            let temp = tempfile::tempdir().unwrap();
            let bash = fixture_bash_ok();
            let mut executor = ToolExecutor::new(temp.path(), &bash);
            let turn = executor.dispatch(&think_action(secret, 1, 3, true)).await;
            let receipt = executor.receipts.last().unwrap();
            assert_eq!(
                receipt.reason,
                Some(crate::think::REASON_SECRET_SHAPED_CONTENT),
                "shape `{label}` must be refused"
            );
            assert_eq!(turn.observation, "[think refused: secret-shaped-content]");
            // The forbidden effect: the secret text persisted NOWHERE —
            // not in the record, not in the observation.
            let record = executor.reasoning_records().last().unwrap();
            assert_eq!(record.thought_text, None, "shape `{label}`");
            assert!(!turn.observation.contains(&secret[..8.min(secret.len())]));
        }
    }

    #[tokio::test]
    async fn think_prose_with_secret_prefixes_is_not_refused() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        let turn = executor
            .dispatch(&think_action(
                "the parser handles the sk- prefix; issue #597 covers the spec home",
                1,
                3,
                true,
            ))
            .await;
        assert_eq!(executor.receipts.last().unwrap().outcome, "completed");
        assert!(turn.observation.starts_with("[think 1/3"));
    }

    #[tokio::test]
    async fn think_branching_fields_are_refused_but_parse_cleanly() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let mut executor = ToolExecutor::new(temp.path(), &bash);
        // Parsing must NOT fail on the reserved fields' presence (§9.48.5).
        let action = WorkerAction::Think {
            thought: "branching".to_string(),
            thought_number: 1,
            total_thoughts: 3,
            next_thought_needed: true,
            is_revision: false,
            revises_thought: None,
            branch_from_thought: Some(1),
            branch_id: Some("b1".to_string()),
        };
        let turn = executor.dispatch(&action).await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some(crate::think::REASON_BRANCHING_NOT_SUPPORTED)
        );
        assert_eq!(
            turn.observation,
            "[think refused: branching-not-supported-in-v1]"
        );
        let record = executor.reasoning_records().last().unwrap();
        assert_eq!(record.thought_text, None);
    }

    #[tokio::test]
    async fn think_kill_switch_refuses_every_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let policy = crate::tooldef::ToolPolicy {
            declared: Vec::new(),
            allowed_internal: std::collections::BTreeSet::new(),
            subsession_depth: 0,
            think_enabled: false,
            record_payloads: true,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash).with_policy(policy);
        let turn = executor
            .dispatch(&think_action("any thought", 1, 3, true))
            .await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some("tool-not-allowed")
        );
        assert_eq!(turn.observation, "tool call refused: tool-not-allowed");
        assert!(
            executor.reasoning_records().is_empty(),
            "a killed switch refuses before any record is produced"
        );
    }

    #[tokio::test]
    async fn think_inside_subsession_requires_allowlist() {
        let temp = tempfile::tempdir().unwrap();
        let bash = fixture_bash_ok();
        let policy = crate::tooldef::ToolPolicy {
            declared: Vec::new(),
            // A sub-session allowlist carved WITHOUT think: the depth rule
            // (§9.48.2 ring 3) keeps the tool refused.
            allowed_internal: std::collections::BTreeSet::from([
                InternalTool::ReadFile,
                InternalTool::Search,
            ]),
            subsession_depth: 1,
            think_enabled: true,
            record_payloads: true,
        };
        let mut executor = ToolExecutor::new(temp.path(), &bash).with_policy(policy);
        let turn = executor
            .dispatch(&think_action("sub-session thought", 1, 3, true))
            .await;
        assert_eq!(
            executor.receipts.last().unwrap().reason,
            Some("tool-not-allowed")
        );
        assert_eq!(turn.observation, "tool call refused: tool-not-allowed");
    }
}

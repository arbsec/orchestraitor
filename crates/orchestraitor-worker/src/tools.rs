//! The four-tool executor and its receipts.
//!
//! Exactly four tools exist: `read_file`, `search`, `bash`, `write_file`.
//! Anything else is refused before reaching this executor (see
//! `action.rs`). Every dispatch produces a [`ToolReceipt`] — the per-call
//! record required by the issue acceptance criteria — carrying static reason
//! codes only: no tool arguments, command lines, or captured output (spec
//! `40-arbitraitor-integration.md` §9.23.4).
//!
//! `bash` crosses the Arbitraitor boundary through the
//! [`crate::mediator::BashMediator`] seam (issue #311); see `mediator.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::action::WorkerAction;
use crate::mediator::{BashMediator, MediationError};
use crate::paths::resolve_confined;
use crate::search::{search_files, truncate_chars};
use crate::tooldef::InternalTool;

/// Read size cap for `read_file` (bytes).
const MAX_READ_BYTES: u64 = 1024 * 1024;
/// Write size cap for `write_file` content (bytes).
const MAX_WRITE_BYTES: usize = 1024 * 1024;
/// Observation cap per captured stream / file body fed back to the model.
const MAX_OBSERVATION_CHARS: usize = 8 * 1024;
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
    root: &'a Path,
    bash: &'a dyn BashMediator,
    receipts: Vec<ToolReceipt>,
    untrusted_writes: Vec<String>,
    /// Declared-tool admission policy (issue #535, T1). `built_ins_only()`
    /// for the bootstrap config; populated from the run's tool definitions
    /// when `[tools]` configuration resolves.
    #[allow(dead_code, reason = "carried for the T2 command/T3 subagent dispatch")]
    policy: crate::tooldef::ToolPolicy,
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
        }
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

    /// The most recent receipt, if any dispatch has happened (the
    /// guardrails' churn window observes it per turn).
    pub(crate) fn last_receipt(&self) -> Option<&ToolReceipt> {
        self.receipts.last()
    }

    /// The canonicalized worktree root the fingerprint reads.
    pub(crate) fn root_path(&self) -> &'a Path {
        self.root
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
}

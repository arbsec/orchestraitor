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

/// Read size cap for `read_file` (bytes).
const MAX_READ_BYTES: u64 = 1024 * 1024;
/// Write size cap for `write_file` content (bytes).
const MAX_WRITE_BYTES: usize = 1024 * 1024;
/// Observation cap per captured stream / file body fed back to the model.
const MAX_OBSERVATION_CHARS: usize = 8 * 1024;

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
}

impl<'a> ToolExecutor<'a> {
    /// Creates an executor for one run.
    pub(crate) fn new(root: &'a Path, bash: &'a dyn BashMediator) -> Self {
        Self {
            root,
            bash,
            receipts: Vec::new(),
            untrusted_writes: Vec::new(),
        }
    }

    /// Consumes the executor, returning all receipts and untrusted writes.
    pub(crate) fn into_records(self) -> (Vec<ToolReceipt>, Vec<String>) {
        (self.receipts, self.untrusted_writes)
    }

    pub(crate) fn untrusted_writes(&self) -> &[String] {
        &self.untrusted_writes
    }

    /// Dispatches one parsed action. `finish` and unknown-tool refusals never
    /// reach this method — the loop records those receipts itself.
    pub(crate) async fn dispatch(&mut self, action: &WorkerAction) -> ToolTurn {
        match action {
            WorkerAction::ReadFile { path } => self.read_file(path),
            WorkerAction::Search { pattern, path } => self.search(pattern, path.as_deref()),
            WorkerAction::WriteFile { path, content } => self.write_file(path, content),
            WorkerAction::Bash { script } => self.bash(script).await,
            WorkerAction::Finish { .. } => self.record_refusal("finish", "misrouted-finish"),
        }
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
        assert!(executor.into_records().1.is_empty());
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
}

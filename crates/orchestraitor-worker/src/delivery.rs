//! Delivery seam for completed worker runs.
//!
//! The worker loop hands a completed task to a [`DeliverySink`]; the thin
//! slice ships [`PendingDeliverySink`], which reports every delivery as
//! pending. The real delivery path (worktree/commit/push/PR) is a separate
//! lane and wires in behind this trait — the loop never performs delivery
//! itself.

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

/// Input handed to the delivery seam for one completed task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryRequest {
    /// Completed task id.
    pub task_id: String,
    /// Model-authored completion summary.
    pub summary: String,
    /// Worktree-relative paths the worker wrote. All worker output begins
    /// untrusted (spec `40-arbitraitor-integration.md` §9.14); classification,
    /// scanning, and promotion authorization stay with Arbitraitor and the
    /// delivery lane.
    pub untrusted_writes: Vec<String>,
    /// Absolute path of the task worktree the run operated in (issue #656):
    /// the delivery sink stages this worktree's committed state onto the
    /// delivery branch. `None` keeps the pre-#656 bootstrap shape (tests,
    /// fixture sources without a filesystem worktree).
    pub worktree: Option<std::path::PathBuf>,
    /// The local branch whose committed tree carries the run's work
    /// (`orc-loop/<task-id>` for the loop/daemon starters; the ambient HEAD
    /// branch for one-shot `orc worker run`). The App-signed landing lands
    /// THIS branch's tree; dirty working-copy state never lands (the
    /// push-branch primitive lands committed content only).
    pub worktree_branch: Option<String>,
}

/// Delivery result reported back in the structured run result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DeliveryOutcome {
    /// A pull request was opened for the task.
    PrOpened {
        /// PR reference (URL or id) from the delivery path.
        reference: String,
    },
    /// Delivery is pending (the delivery path is not wired in this slice).
    Pending {
        /// Static reason code.
        reason: &'static str,
    },
}

/// Delivery failures carry static reason codes only (spec §9.23.4).
#[derive(Debug, Error)]
#[error("delivery failed: {reason}")]
pub struct DeliveryError {
    /// Static reason code.
    pub reason: &'static str,
}

/// Hand-off seam from a completed worker run to the delivery path.
#[async_trait]
pub trait DeliverySink: Send + Sync {
    /// Delivers one completed task.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError`] when the delivery path rejects or fails; the
    /// loop converts this into a typed run failure.
    async fn deliver(&self, request: &DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError>;
}

/// Bootstrap delivery sink: the delivery path is not wired in this slice, so
/// every completed task is reported pending (never silently dropped).
pub struct PendingDeliverySink;

#[async_trait]
impl DeliverySink for PendingDeliverySink {
    async fn deliver(&self, request: &DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        let _ = request;
        Ok(DeliveryOutcome::Pending {
            reason: "delivery-path-not-wired",
        })
    }
}

/// Static reason codes for the push-branch delivery path (spec §9.23.4 —
/// log-safe, no paths or command lines leak into the codes themselves).
/// Typed reason: the run carried no filesystem worktree/branch context
/// (a fixture-shaped run), so there is nothing landable.
pub const DELIVERY_NO_WORKTREE: &str = "delivery-no-worktree";
/// Typed reason: staging/committing the worktree state (or verifying the
/// checked-out branch) failed.
pub const DELIVERY_BRANCH_STATE_FAILED: &str = "delivery-branch-state-failed";
/// Typed reason: the `orc github push-branch` landing failed (an empty
/// diff reports `delivery-empty-diff` instead).
pub const DELIVERY_PUSH_FAILED: &str = "delivery-push-failed";
/// Typed reason: the PR-creation step (labeling-chained) failed or
/// returned no PR reference.
pub const DELIVERY_PR_OPEN_FAILED: &str = "delivery-pr-open-failed";
/// Typed reason: the landed tree is identical to the base — nothing to
/// deliver (the landing primitive's benign no-op, surfaced as typed).
pub const DELIVERY_EMPTY_DIFF: &str = "delivery-empty-diff";
/// Typed reason: the configured `owner/name` repo slug is malformed
/// (startup-shape validation, review gen-3).
pub const DELIVERY_REPO_INVALID: &str = "delivery-repo-invalid";
/// Typed reason: the configured `orc` binary does not exist (startup-shape
/// validation, review gen-3).
pub const DELIVERY_ORC_BIN_MISSING: &str = "delivery-orc-bin-missing";

/// Bounded wait for one `orc` child invocation (landing or PR): a hung
/// child is killed and reported typed — never blocks the run (review
/// gen-3 minor).
const ORC_CHILD_TIMEOUT_SECS: u64 = 300;

/// Real delivery sink (issue #656): hands a completed run's committed
/// worktree state to `orc github push-branch` (the App-signed landing
/// primitive, #491) and then opens a PR through `orc github
/// gh-env-pr-create-chain` (the sanctioned labeling-chained `gh pr create`
/// path). The sink is CLI-coupled by construction — it shells out to the
/// `orc` binary — so it lives behind the same [`DeliverySink`] trait the
/// loop consumes; the loop never performs delivery itself.
///
/// Failure posture: every failure is a typed [`DeliveryError`] with a
/// static reason code; a push failure is a typed delivery outcome, never a
/// silent drop, and never blocks the loop's success accounting silently
/// (the worker converts a delivery error into the typed
/// `DeliveryFailed` run failure).
///
/// Untrusted-write classification/promotion stays with Arbitraitor (spec
/// §9.14; the module doc's boundary holds — this sink performs no
/// classification, only mechanical landing of the worktree's COMMITTED
/// state through the existing mediation-free push primitive).
pub struct PushBranchDeliverySink {
    /// Path to the `orc` binary (the App-signed landing primitive's
    /// entrypoint).
    orc_bin: std::path::PathBuf,
    /// The PR's base branch (the branch the delivery branch was cut from;
    /// `main` for the MVP loop).
    base_branch: String,
    /// The `owner/name` repository the delivery lands in (resolved once at
    /// starter construction from the board/project configuration).
    repo: String,
}

impl PushBranchDeliverySink {
    /// Builds the sink for one loop/daemon starter: `orc_bin` is the
    /// resolved `orc` binary path, `repo` the `owner/name` slug, and
    /// `base_branch` the PR base (default `main`).
    ///
    /// # Errors
    ///
    /// Returns a typed [`DeliveryError`] when the repo slug is not a
    /// non-empty `owner/name` pair ([`DELIVERY_REPO_INVALID`]) or the
    /// `orc` binary does not exist ([`DELIVERY_ORC_BIN_MISSING`]) —
    /// startup-shape validation, never a mid-task discovery.
    pub fn new(
        orc_bin: std::path::PathBuf,
        repo: String,
        base_branch: String,
    ) -> Result<Self, DeliveryError> {
        // Startup-shape validation (review gen-1/2/3 findings): a malformed
        // repo slug or a missing orc binary must fail HERE — typed, before
        // any task runs — never as a per-task `delivery-push-failed` after
        // completion.
        let mut parts = repo.split('/');
        let (owner, name) = match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => {
                (owner, name)
            }
            _ => {
                return Err(DeliveryError {
                    reason: DELIVERY_REPO_INVALID,
                });
            }
        };
        if !orc_bin.is_file() {
            return Err(DeliveryError {
                reason: DELIVERY_ORC_BIN_MISSING,
            });
        }
        let _ = (owner, name);
        Ok(Self {
            orc_bin,
            base_branch,
            repo,
        })
    }

    /// Runs the App-signed landing step (`orc github push-branch`) for one
    /// delivery request: the worktree's committed tree lands as ONE signed
    /// commit on `orc-delivery/<task-id>`. Returns the typed reason on
    /// failure (an empty diff reports [`DELIVERY_EMPTY_DIFF`]).
    ///
    /// # Errors
    ///
    /// Returns the static reason code of the failed step.
    async fn push_branch_landing(
        &self,
        worktree: &std::path::Path,
        branch: &str,
        remote_branch: &str,
        request: &DeliveryRequest,
    ) -> Result<(), &'static str> {
        let message = format!("feat(loop): land task {task}", task = request.task_id);
        let mut landing = tokio::process::Command::new(&self.orc_bin);
        landing
            .current_dir(worktree)
            .args(["github", "push-branch"])
            .args(["--branch", branch])
            .args(["--remote-branch", remote_branch])
            .args(["--base", &self.base_branch])
            .args(["--message", &message])
            .args(["--owner", self.repo.split('/').next().unwrap_or_default()])
            .args(["--repo", self.repo.split('/').nth(1).unwrap_or_default()]);
        // A hung child must never block the run: bounded wait, killed on
        // drop (review gen-3 minor).
        landing.kill_on_drop(true);
        let landing_output = tokio::time::timeout(
            std::time::Duration::from_secs(ORC_CHILD_TIMEOUT_SECS),
            landing.output(),
        )
        .await
        .map_err(|_| DELIVERY_PUSH_FAILED)?
        .map_err(|_| DELIVERY_PUSH_FAILED)?;
        if !landing_output.status.success() {
            // An empty diff is a benign no-op (the worktree committed
            // nothing beyond the base): typed, not a silent drop, but the
            // PR would be empty — report the typed outcome.
            let stderr = String::from_utf8_lossy(&landing_output.stderr);
            if stderr.contains("nothing to land") {
                return Err(DELIVERY_EMPTY_DIFF);
            }
            return Err(DELIVERY_PUSH_FAILED);
        }
        Ok(())
    }

    /// Runs the PR-creation step through `orc github
    /// gh-env-pr-create-chain` (the sanctioned labeling-chained `gh pr
    /// create` path) and returns the created PR's URL.
    ///
    /// # Errors
    ///
    /// Returns [`DELIVERY_PR_OPEN_FAILED`] when the child fails or returns
    /// no URL.
    async fn open_pr(
        &self,
        worktree: &std::path::Path,
        remote_branch: &str,
        request: &DeliveryRequest,
    ) -> Result<String, &'static str> {
        let title = format!("feat(loop): task {task}", task = request.task_id);
        let mut pr = tokio::process::Command::new(&self.orc_bin);
        pr.current_dir(worktree).args([
            "github",
            "gh-env-pr-create-chain",
            &self.repo,
            "--",
            "gh",
            "pr",
            "create",
            "--repo",
            &self.repo,
            "--head",
            remote_branch,
            "--base",
            &self.base_branch,
            "--title",
            &title,
            "--body",
            &request.summary,
        ]);
        pr.kill_on_drop(true);
        let pr_output = tokio::time::timeout(
            std::time::Duration::from_secs(ORC_CHILD_TIMEOUT_SECS),
            pr.output(),
        )
        .await
        .map_err(|_| DELIVERY_PR_OPEN_FAILED)?
        .map_err(|_| DELIVERY_PR_OPEN_FAILED)?;
        if !pr_output.status.success() {
            // Idempotent retry (review gen-2/3 findings): an existing open
            // PR for this delivery branch IS the previous delivery's
            // success — return its reference (a rerun that already landed
            // its commit must not fail the run). The reference is fetched
            // read-only via `gh pr view`; a lookup failure falls back to a
            // branch-identifying reference so the outcome is still
            // PrOpened-shaped and never a duplicate-PR failure.
            let stderr = String::from_utf8_lossy(&pr_output.stderr);
            if stderr.contains("already exists") {
                let mut lookup = tokio::process::Command::new(&self.orc_bin);
                lookup.current_dir(worktree).args([
                    "github",
                    "gh-env",
                    "--",
                    "gh",
                    "pr",
                    "view",
                    remote_branch,
                    "--repo",
                    &self.repo,
                    "--json",
                    "url,state",
                    "--jq",
                    "\"\\(.url) \\(.state)\"",
                ]);
                lookup.kill_on_drop(true);
                let lookup_output = tokio::time::timeout(
                    std::time::Duration::from_secs(ORC_CHILD_TIMEOUT_SECS),
                    lookup.output(),
                )
                .await
                .map_err(|_| DELIVERY_PR_OPEN_FAILED)?
                .map_err(|_| DELIVERY_PR_OPEN_FAILED)?;
                // The lookup's exit status gates the PrOpened report (review
                // gen-5 major): a failed lookup is the documented
                // `delivery-pr-open-failed`, never a fabricated reference —
                // and "already exists" text can also come from other errors
                // (e.g. an existing branch), so a non-confirmed PR is never
                // reported as opened.
                if !lookup_output.status.success() {
                    return Err(DELIVERY_PR_OPEN_FAILED);
                }
                let url = String::from_utf8_lossy(&lookup_output.stdout)
                    .trim()
                    .to_string();
                // Only an OPEN PR counts (review gen-6 minor): `gh pr view
                // <branch>` can resolve a CLOSED or MERGED PR for the same
                // head branch — a stale reference would report `pr-opened`
                // with no open PR.
                let mut parts = url.split_whitespace();
                let (Some(reference), Some(state)) = (parts.next(), parts.next()) else {
                    return Err(DELIVERY_PR_OPEN_FAILED);
                };
                if state != "OPEN" {
                    return Err(DELIVERY_PR_OPEN_FAILED);
                }
                return Ok(reference.to_string());
            }
            return Err(DELIVERY_PR_OPEN_FAILED);
        }
        let pr_url = String::from_utf8_lossy(&pr_output.stdout)
            .trim()
            .to_string();
        if pr_url.is_empty() {
            return Err(DELIVERY_PR_OPEN_FAILED);
        }
        Ok(pr_url)
    }
}

#[async_trait]
impl DeliverySink for PushBranchDeliverySink {
    async fn deliver(&self, request: &DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        // The run must carry a filesystem worktree + branch to land: fixture
        // sources without one are a typed failure, never a silent drop.
        let (Some(worktree), Some(branch)) = (&request.worktree, &request.worktree_branch) else {
            return Err(DeliveryError {
                reason: DELIVERY_NO_WORKTREE,
            });
        };
        let remote_branch = format!("orc-delivery/{task}", task = request.task_id);
        // 1. Commit the worktree state onto the delivery branch. The loop's
        //    worktrees carry UNCOMMITTED worker writes; push-branch lands a
        //    branch's COMMITTED tree, so stage and commit here (failures are
        //    typed; the working tree is left for the post-run prune).
        if let Err(reason) = commit_worktree_state(worktree, branch, request).await {
            return Err(DeliveryError { reason });
        }
        // 2. Land the branch tree as ONE App-signed commit (the #491
        //    primitive; required-enforcement gates run inside orc).
        self.push_branch_landing(worktree, branch, &remote_branch, request)
            .await
            .map_err(|reason| DeliveryError { reason })?;
        // 3. Open the PR through the sanctioned labeling-chained path (the
        //    ONLY `gh pr create` route in required enforcement).
        let pr_url = self
            .open_pr(worktree, &remote_branch, request)
            .await
            .map_err(|reason| DeliveryError { reason })?;
        Ok(DeliveryOutcome::PrOpened { reference: pr_url })
    }
}

/// Commits the worktree's current state onto the delivery branch (issue
/// #656). Ordering and safety (review gen-2/3 majors):
///
/// 1. BRANCH FIRST: the checked-out branch is verified BEFORE any mutation
///    — a drifted worktree is a typed refusal, never a commit onto the
///    wrong branch.
/// 2. STAGE ALL: `git add -A` in every delivery-armed run (review gen-5
///    major): the mediated `bash` tool can edit, create, and DELETE files
///    beyond the recorded `untrusted_writes` list, so scoped staging would
///    silently drop part of the work. The one-shot paths are safe because
///    their startup gate refuses a DIRTY checkout or the base branch
///    itself — every change after startup comes from the run.
/// 3. EXPLICIT IDENTITY: the commit pins `user.name`/`user.email` via
///    `-c` (the service-identity bot attribution the loop environment
///    provides; `git commit` fails typed on a host with no ambient git
///    identity otherwise). The App-signed landing path re-signs the landing
///    commit on the remote, so local attribution is bookkeeping only.
/// 4. ENV SCRUBBED: `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` are removed
///    (the loop runner's `git` idiom) so a hostile ambient environment
///    cannot redirect the operations.
///
/// The git steps are synchronous and short (local file operations); they
/// run under `spawn_blocking` from the async `deliver` so a runtime worker
/// thread is never blocked.
///
/// Every failure maps to a static reason code.
async fn commit_worktree_state(
    worktree: &std::path::Path,
    branch: &str,
    request: &DeliveryRequest,
) -> Result<(), &'static str> {
    // BRANCH FIRST (before any mutation).
    let current = git_async(worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    if !current.status.success() {
        return Err(DELIVERY_BRANCH_STATE_FAILED);
    }
    let current_branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
    if current_branch != branch {
        return Err(DELIVERY_BRANCH_STATE_FAILED);
    }
    // STAGING = `git add -A` in EVERY delivery-armed run (review gen-5
    // major, superseding the gen-3/4 scoped-staging split): the mediated
    // `bash` tool can edit, create, and DELETE files that are not on the
    // recorded `untrusted_writes` list, so scoped staging silently drops
    // part of the work (and a deleted-but-recorded path makes
    // `git add -- <path>` fail). The one-shot paths are safe: their
    // startup gate refuses a DIRTY checkout, so every change after startup
    // comes from the run itself. The loop/daemon worktrees are task-owned
    // by construction.
    let add = git_async(worktree, &["add", "-A"]).await?;
    if !add.status.success() {
        return Err(DELIVERY_BRANCH_STATE_FAILED);
    }
    // Commit with the pinned identity. "nothing to commit" is the empty
    // diff — the landing reports the typed empty-diff outcome.
    let commit = git_async(
        worktree,
        &[
            "-c",
            "user.name=arbsec-agent[bot]",
            "-c",
            "user.email=334074867+arbsec-agent[bot]@users.noreply.github.com",
            "commit",
            "-m",
            &format!("feat(loop): task {} work", request.task_id),
        ],
    )
    .await?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        if !stderr.contains("nothing to commit") {
            return Err(DELIVERY_BRANCH_STATE_FAILED);
        }
    }
    Ok(())
}

/// One short synchronous git invocation, run under `spawn_blocking` (a
/// local file operation must never block a runtime worker thread — review
/// gen-3 minor). Environment repository-location variables are scrubbed.
async fn git_async(
    dir: &std::path::Path,
    args: &[&str],
) -> Result<std::process::Output, &'static str> {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new("git");
        command.current_dir(&dir);
        for variable in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            command.env_remove(variable);
        }
        command
            .args(&args)
            .output()
            .map_err(|_| DELIVERY_BRANCH_STATE_FAILED)
    })
    .await
    .map_err(|_| DELIVERY_BRANCH_STATE_FAILED)?
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;

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

/// The managed PR checklist (`.github/PULL_REQUEST_TEMPLATE.md`'s nine
/// `<!-- orc:* -->` markers, UNCHECKED) placed at the TOP of every delivery PR
/// body, AHEAD of the model-authored summary.
///
/// `convergence-status` FAILS CLOSED on a body with no managed marker
/// (`checklist_markers_missing`, arbsec/orchestraitor#788), and a delivery body
/// is the model-authored run summary — it never had the template applied, so
/// without this the loop's own PRs would be unmergeable by construction (#683:
/// a fail-closed rule no caller can satisfy is worse than the bug). The markers
/// land UNCHECKED because a checked box is a verified fact: the reviewing agent
/// ticks them with evidence. They are added unconditionally, never "only
/// when the summary carries none" — untrusted model output must not decide
/// whether the gate applies to itself (spec `40-arbitraitor-integration.md`
/// §6.1).
///
/// It comes FIRST because the summary is untrusted: a summary that leaves a
/// code fence OPEN would enclose anything appended after it, and every scanner
/// (`strip_fenced_code`, the shell fence engine) drops fenced regions as
/// examples — so a summary carrying one ticked marker plus an unclosed fence
/// could certify the gate for itself (review gen-3 HIGH). No fence in trusted
/// text can reach the markers placed ahead of untrusted text. The drift guard in
/// `delivery_tests` asserts these lines match the template.
const DELIVERY_PR_CHECKLIST: &str = "## Merge checklist\n\n\
- [ ] <!-- orc:issue --> Linked issue and specification requirements are satisfied\n\
- [ ] <!-- orc:service-identity --> All GitHub writes (PR creation, comments, review posts, merges, commits) attributed to arbsec-agent — no personal-account (@mekwall) writes (owner mandate; .agents/project/orchestraitor-workflow.md \"GitHub service identity\")\n\
- [ ] <!-- orc:tests --> Tests are added or updated\n\
- [ ] <!-- orc:security --> Security impact is reviewed\n\
- [ ] <!-- orc:docs --> Human-facing documentation is updated, or not required with justification\n\
- [ ] <!-- orc:rollback --> Rollback or recovery behavior is verified where applicable\n\
- [ ] <!-- orc:checks --> All required and non-optional checks pass\n\
- [ ] <!-- orc:review --> All noteworthy findings are resolved\n\
- [ ] <!-- orc:threads --> All actionable review threads are resolved\n\
\n";

/// Neutralises the two constructs an UNTRUSTED model summary could use to make
/// the composed PR body **INDETERMINATE** or to hide content from a reviewer.
///
/// The checklist gate refuses a body that carries a fence OPENER indented 1-3
/// spaces (`checklist_unparseable`, arbsec/orchestraitor#809), and that
/// condition is STICKY: it is not cleared when the region closes, because a
/// closer at a different container level ends the region earlier for the
/// renderer than for the scanner. A model summary that legitimately contains a
/// nested code fence (```` ``` ```` inside a list item, say) would therefore
/// make every delivery PR unmergeable by construction — and the summary is
/// untrusted content the LANE composed, so the lane owns that guarantee.
///
/// Two transformations, both minimal and both idempotent:
///
/// - a line indented 1-3 SPACES whose next characters are 3+ backticks or 3+
///   tildes is **dedented to column 0**. That is exactly the remedy the gate's
///   own message names, and it makes the opener a determinate column-0 fence.
///   The test is a strict SUPERSET of the scanner's opener rule (which also
///   rejects a backtick in a backtick fence's info string), so no indented
///   opener can survive; dedenting a line that was not an opener renders the
///   same either way;
/// - a line whose first non-whitespace content is `<!--` (at most 3 leading
///   spaces, and no leading tab) has its `<` escaped to `&lt;`, so it renders
///   as the literal text `<!--` and opens no HTML block. The nine trusted
///   markers precede the summary and a marker line's `<!--` is never
///   line-start, so the checklist reading stays determinate either way — but an
///   UNCLOSED `<!--` would swallow the rest of the summary and hide it from the
///   reviewer, which is content the lane must not let untrusted text suppress.
///   Escaping keeps every byte the model wrote VISIBLE; only the ability to
///   open a region is removed.
///
/// After this, the only region the summary can open is a COLUMN-0 fence: that
/// one is determinate, and because the trusted checklist leads the body it can
/// never enclose it.
///
/// The summary's LINE ENDINGS are normalised first (`CommonMark` §2.1): every
/// `\r\n` becomes `\n` and every remaining lone `\r` becomes `\n`. Both
/// production scanners normalise the same way before they read a body, so a
/// summary that uses lone-CR line endings must be seen by the neutraliser as
/// the same LINES the scanners will see. Splitting on `\n` alone left a
/// lone-CR summary as ONE line: its indented fence opener and its line-start
/// `<!--` survived neutralisation, and the composed PR then reported
/// `checklist_unparseable` (permanently unconvergeable) or hid the summary's
/// tail from the reviewer.
fn neutralise_summary(summary: &str) -> String {
    let normalized = summary.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    for line in normalized.split_inclusive('\n') {
        // Keep the terminator out of the tests, then put it back: an opener is
        // recognised from the line's content alone.
        let (content, newline) = match line.strip_suffix('\n') {
            Some(rest) => (rest, "\n"),
            None => (line, ""),
        };
        let indent = content.chars().take_while(|c| *c == ' ').count();
        let rest = content.get(indent..).unwrap_or_default();
        let indented_opener =
            (1..=3).contains(&indent) && (rest.starts_with("```") || rest.starts_with("~~~"));
        let comment_opener = indent <= 3 && !rest.starts_with('\t') && rest.starts_with("<!--");
        if indented_opener {
            out.push_str(rest);
        } else if comment_opener {
            out.push_str(&content[..indent]);
            out.push_str("&lt;");
            out.push_str(rest.get(1..).unwrap_or_default());
        } else {
            out.push_str(content);
        }
        out.push_str(newline);
    }
    out
}

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
        // The managed checklist rides the body and comes FIRST: the summary is
        // UNTRUSTED model output (spec `40-arbitraitor-integration.md` §6.1) and
        // may leave a code fence open. Appended after it, the trusted markers
        // would land INSIDE that fence, where every scanner strips them as an
        // example — a summary with one ticked marker plus an unclosed fence
        // would then pass the merge gate on its own (see DELIVERY_PR_CHECKLIST).
        //
        // The summary is also NEUTRALISED before it is composed (see
        // [`neutralise_summary`]): an indented fence opener in it would make the
        // composed body INDETERMINATE (`checklist_unparseable`) and a line-start
        // `<!--` would hide the rest of the summary from the reviewer. The
        // trusted block is composed here, never derived from the summary, so it
        // can never be reordered behind a region the summary creates.
        let body = format!(
            "{DELIVERY_PR_CHECKLIST}{}",
            neutralise_summary(&request.summary)
        );
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
            &body,
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

//! Error types for the delivery path. No error output ever carries tokens,
//! headers, or command stderr (spec `40-arbitraitor-integration.md` §9.23.4).

use std::fmt;

/// A failed `git` invocation on the delivery path.
///
/// `Display` and `Debug` render only the operation label and exit code:
/// command output is never surfaced because it can echo transport data.
pub struct GitError {
    pub(crate) operation: &'static str,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stderr: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.exit_code {
            Some(code) => write!(
                formatter,
                "git {} exited with status {code}",
                self.operation
            ),
            None => write!(formatter, "git {} could not be executed", self.operation),
        }
    }
}

impl fmt::Debug for GitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitError")
            .field("operation", &self.operation)
            .field("exit_code", &self.exit_code)
            .field("stderr", &"REDACTED")
            .finish()
    }
}

impl std::error::Error for GitError {}

/// Errors produced by the delivery path.
#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    /// Task branch name failed validation (`<type>/<slug>`).
    #[error("invalid task branch {branch:?}: {reason}")]
    InvalidBranch {
        /// Rejected branch name.
        branch: String,
        /// Why the name was rejected.
        reason: &'static str,
    },
    /// Base revision failed option-safety validation.
    #[error("invalid base revision {base_revision:?}: {reason}")]
    InvalidBaseRevision {
        /// Rejected revision string.
        base_revision: String,
        /// Why the revision was rejected.
        reason: &'static str,
    },
    /// Commit identity failed DCO-safe validation.
    #[error("invalid commit identity: {reason}")]
    InvalidIdentity {
        /// Why the identity was rejected.
        reason: &'static str,
    },
    /// Commit message was empty after trimming.
    #[error("commit message must not be empty")]
    EmptyCommitMessage,
    /// `git worktree add` failed.
    #[error("failed to provision worktree for branch {branch}")]
    ProvisionWorktree {
        /// Task branch being provisioned.
        branch: String,
        /// Underlying git failure (redacted).
        source: GitError,
    },
    /// Staging or inspecting the worktree failed.
    #[error("failed to stage changes in worktree for branch {branch}")]
    Stage {
        /// Task branch being committed.
        branch: String,
        /// Underlying git failure (redacted).
        source: GitError,
    },
    /// The worktree contained no changes to commit.
    #[error("nothing to commit on branch {branch}")]
    NothingToCommit {
        /// Task branch with a clean worktree.
        branch: String,
    },
    /// `git commit` failed.
    #[error("failed to commit on branch {branch}")]
    Commit {
        /// Task branch being committed.
        branch: String,
        /// Underlying git failure (redacted).
        source: GitError,
    },
    /// The injected provider could not supply a usable push credential.
    /// There is no ambient-auth fallback; the worktree is preserved.
    #[error("push credential unavailable")]
    Credential {
        /// Provider-side failure detail (secret-free by contract).
        source: CredentialError,
    },
    /// The remote refused the push as a non-fast-forward update.
    #[error("push of branch {branch} rejected by the remote (non-fast-forward)")]
    PushRejected {
        /// Task branch that was rejected.
        branch: String,
    },
    /// The remote denied the presented credential.
    #[error("push of branch {branch} failed authentication")]
    PushAuthFailed {
        /// Task branch that was denied.
        branch: String,
    },
    /// Any other push failure.
    #[error("failed to push branch {branch}")]
    PushFailed {
        /// Task branch being pushed.
        branch: String,
        /// Underlying git failure (redacted).
        source: GitError,
    },
    /// Draft pull request creation failed.
    #[error("failed to open draft pull request for {owner}/{repo}")]
    PullRequest {
        /// Repository owner.
        owner: String,
        /// Repository name.
        repo: String,
        /// Transport-side failure (secret-free by contract).
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Why a push credential could not be supplied. Implementations must never
/// place secret material in `reason`.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The credential is missing or could not be minted.
    #[error("credential unavailable: {reason}")]
    Unavailable {
        /// Secret-free explanation.
        reason: String,
    },
    /// The credential exists but is expired.
    #[error("credential expired: {reason}")]
    Expired {
        /// Secret-free explanation.
        reason: String,
    },
}

//! Value types for the delivery path.

use std::path::{Path, PathBuf};

use super::{DeliveryError, PullRequestHandle};

/// Caller-provided bot identity used as both author and committer (D11).
/// Constructing it validates the DCO-safe shape; invalid input is rejected
/// before any git mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitIdentity {
    name: String,
    email: String,
}

impl CommitIdentity {
    /// Validates and builds a commit identity.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError::InvalidIdentity`] when the name or email is
    /// empty, contains control characters or header metacharacters, or the
    /// email lacks a single `@` with non-empty local and domain parts.
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Result<Self, DeliveryError> {
        let name = name.into();
        let email = email.into();
        validate_identity_part(&name, true)?;
        validate_identity_part(&email, false)?;
        Ok(Self { name, email })
    }

    /// Display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Email address.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }
}

fn validate_identity_part(value: &str, is_name: bool) -> Result<(), DeliveryError> {
    if value.trim().is_empty() {
        return Err(DeliveryError::InvalidIdentity {
            reason: "name and email must not be empty",
        });
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '<' | '>'))
    {
        return Err(DeliveryError::InvalidIdentity {
            reason: "name and email must not contain control characters or angle brackets",
        });
    }
    if !is_name {
        let mut parts = value.split('@');
        let valid = matches!(
            (parts.next(), parts.next(), parts.next()),
            (Some(local), Some(domain), None) if !local.is_empty() && !domain.is_empty()
        );
        if !valid {
            return Err(DeliveryError::InvalidIdentity {
                reason: "email must contain a single @ with non-empty local and domain parts",
            });
        }
    }
    Ok(())
}

/// A provisioned per-task worktree on branch `<type>/<slug>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskWorktree {
    pub(crate) path: PathBuf,
    pub(crate) branch: String,
}

impl TaskWorktree {
    /// Worktree directory on disk.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Task branch checked out in the worktree.
    #[must_use]
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

/// Caller-supplied draft pull request content for
/// [`Delivery::deliver`](crate::Delivery::deliver).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftPullRequestSpec {
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Pull request title (Conventional Commits form, caller-enforced).
    pub title: String,
    /// Base branch the pull request targets.
    pub base_branch: String,
    /// Spec anchors recorded in the body (spec `10-orchestrator.md` §9.33.2).
    pub spec_anchors: Vec<String>,
    /// Evidence paths recorded in the body.
    pub evidence_paths: Vec<PathBuf>,
}

/// Inputs for one [`Delivery::deliver`](crate::Delivery::deliver) run over a
/// provisioned worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryRequest {
    /// Commit message (subject and body). The `Signed-off-by` DCO trailer is
    /// appended from the identity.
    pub commit_message: String,
    /// Bot author/committer identity.
    pub identity: CommitIdentity,
    /// Draft pull request content.
    pub pull_request: DraftPullRequestSpec,
}

/// Structured result of a completed delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryOutcome {
    /// Worktree the commit was created in (preserved on failure paths).
    pub worktree: TaskWorktree,
    /// Hex object id of the delivered commit.
    pub commit: String,
    /// Opened draft pull request.
    pub pull_request: PullRequestHandle,
}

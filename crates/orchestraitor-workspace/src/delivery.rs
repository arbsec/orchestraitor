//! Task-output delivery path: per-task worktree, DCO-signed commit, scoped
//! push, and draft pull request (issue #312; spec `20-harness-worker.md`
//! §9.4, `40-arbitraitor-integration.md` §6.2).
//!
//! The trusted controller owns every Git mutation on this path — a worktree
//! is not a sandbox, and the worker never sees `.git` of the main checkout.
//! Commit author/committer identity and the push credential are caller
//! inputs: the credential arrives through an injected
//! [`PushCredentialProvider`] (backed by the GitHub App installation-token
//! minting path in `orchestraitor-core`), never from ambient git
//! configuration, credential helpers, or the environment. A missing or
//! expired credential is a typed failure; the worktree is preserved for
//! retry and no ambient-auth fallback is attempted. Pull-request creation
//! goes through the injectable [`PullRequestTransport`] boundary, keeping
//! this crate free of HTTP I/O and tests free of network (spec
//! `50-contracts-data.md` §21.3).

mod error;
mod git;
mod transport;
mod types;

use std::path::{Path, PathBuf};

use secrecy::SecretString;

pub use error::{CredentialError, DeliveryError, GitError};
use git::{Git, PushFailure};
pub use transport::{
    NewPullRequest, PullRequestHandle, PullRequestTransport, PushCredentialProvider,
};
pub use types::{
    CommitIdentity, DeliveryOutcome, DeliveryRequest, DraftPullRequestSpec, TaskWorktree,
};

const TASK_BRANCH_TYPES: [&str; 10] = [
    "feat", "fix", "security", "docs", "refactor", "test", "ci", "chore", "build", "perf",
];

/// Trusted delivery controller for one repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    repo_path: PathBuf,
}

impl Delivery {
    /// Creates a delivery controller for the trusted repository path.
    #[must_use]
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
        }
    }

    /// Provisions a per-task worktree on `branch` (`<type>/<slug>`) based on
    /// `base_revision` at `dest`.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError::InvalidBranch`] for malformed branch names,
    /// [`DeliveryError::InvalidBaseRevision`] when `base_revision` could be
    /// misread as a git option or is empty, and
    /// [`DeliveryError::ProvisionWorktree`] when `git worktree add` fails.
    pub fn provision_worktree(
        &self,
        branch: &str,
        base_revision: &str,
        dest: &Path,
    ) -> Result<TaskWorktree, DeliveryError> {
        validate_task_branch(branch)?;
        validate_base_revision(base_revision)?;
        Git::new(&self.repo_path)
            .worktree_add(branch, dest, base_revision)
            .map_err(|source| DeliveryError::ProvisionWorktree {
                branch: branch.to_owned(),
                source,
            })?;
        Ok(TaskWorktree {
            path: dest.to_path_buf(),
            branch: branch.to_owned(),
        })
    }

    /// Stages every worktree change and commits with a `Signed-off-by` DCO
    /// trailer derived from `identity`, returning the commit id.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError::EmptyCommitMessage`] for empty messages,
    /// [`DeliveryError::NothingToCommit`] when the worktree is clean, and
    /// [`DeliveryError::Stage`] / [`DeliveryError::Commit`] on git failures.
    pub fn commit_all(
        &self,
        worktree: &TaskWorktree,
        commit_message: &str,
        identity: &CommitIdentity,
    ) -> Result<String, DeliveryError> {
        if commit_message.trim().is_empty() {
            return Err(DeliveryError::EmptyCommitMessage);
        }
        let git = Git::new(worktree.path());
        if !git.has_changes().map_err(|source| DeliveryError::Stage {
            branch: worktree.branch.clone(),
            source,
        })? {
            return Err(DeliveryError::NothingToCommit {
                branch: worktree.branch.clone(),
            });
        }
        git.stage_all().map_err(|source| DeliveryError::Stage {
            branch: worktree.branch.clone(),
            source,
        })?;
        let message = format!(
            "{}\n\nSigned-off-by: {} <{}>",
            commit_message.trim_end(),
            identity.name(),
            identity.email()
        );
        git.commit(&message, identity.name(), identity.email())
            .map_err(|source| DeliveryError::Commit {
                branch: worktree.branch.clone(),
                source,
            })?;
        git.head().map_err(|source| DeliveryError::Commit {
            branch: worktree.branch.clone(),
            source,
        })
    }

    /// Pushes the worktree branch to `origin` using only `credential`.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError::PushRejected`] on non-fast-forward refusal,
    /// [`DeliveryError::PushAuthFailed`] when the remote denies the
    /// credential, and [`DeliveryError::PushFailed`] otherwise. No ambient
    /// credential is ever consulted; the worktree is preserved for retry.
    pub fn push(
        &self,
        worktree: &TaskWorktree,
        credential: &SecretString,
    ) -> Result<(), DeliveryError> {
        Git::new(worktree.path())
            .push(worktree.branch(), credential)
            .map_err(|failure| match failure {
                PushFailure::Rejected => DeliveryError::PushRejected {
                    branch: worktree.branch.clone(),
                },
                PushFailure::Auth => DeliveryError::PushAuthFailed {
                    branch: worktree.branch.clone(),
                },
                PushFailure::Other(source) => DeliveryError::PushFailed {
                    branch: worktree.branch.clone(),
                    source,
                },
            })
    }

    /// Opens the draft pull request through `transport`.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError::PullRequest`] when the transport fails.
    pub fn open_draft_pull_request(
        &self,
        worktree: &TaskWorktree,
        spec: &DraftPullRequestSpec,
        credential: SecretString,
        transport: &dyn PullRequestTransport,
    ) -> Result<PullRequestHandle, DeliveryError> {
        let request = NewPullRequest {
            owner: spec.owner.clone(),
            repo: spec.repo.clone(),
            title: spec.title.clone(),
            head_branch: worktree.branch.clone(),
            base_branch: spec.base_branch.clone(),
            body: render_pull_request_body(spec),
            draft: true,
            credential,
        };
        transport
            .create_pull_request(&request)
            .map_err(|source| DeliveryError::PullRequest {
                owner: spec.owner.clone(),
                repo: spec.repo.clone(),
                source,
            })
    }

    /// Composite path: commit, fetch the scoped credential, push, and open
    /// the draft pull request. The credential is fetched before the push;
    /// provider failure aborts before any push attempt and preserves the
    /// worktree with its commit for retry.
    ///
    /// # Errors
    ///
    /// Returns the first [`DeliveryError`] of the failing step.
    pub fn deliver(
        &self,
        worktree: &TaskWorktree,
        request: &DeliveryRequest,
        credentials: &dyn PushCredentialProvider,
        transport: &dyn PullRequestTransport,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let commit = self.commit_all(worktree, &request.commit_message, &request.identity)?;
        let credential = credentials
            .push_credential()
            .map_err(|source| DeliveryError::Credential { source })?;
        self.push(worktree, &credential)?;
        let pull_request =
            self.open_draft_pull_request(worktree, &request.pull_request, credential, transport)?;
        Ok(DeliveryOutcome {
            worktree: worktree.clone(),
            commit,
            pull_request,
        })
    }
}

fn validate_base_revision(base_revision: &str) -> Result<(), DeliveryError> {
    if base_revision.is_empty()
        || base_revision.starts_with('-')
        || base_revision.chars().any(char::is_whitespace)
    {
        return Err(DeliveryError::InvalidBaseRevision {
            base_revision: base_revision.to_owned(),
            reason: "must be non-empty, option-free, whitespace-free",
        });
    }
    Ok(())
}

fn validate_task_branch(branch: &str) -> Result<(), DeliveryError> {
    let mut segments = branch.split('/');
    let (Some(kind), Some(slug), None) = (segments.next(), segments.next(), segments.next()) else {
        return Err(DeliveryError::InvalidBranch {
            branch: branch.to_owned(),
            reason: "expected exactly two segments <type>/<slug>",
        });
    };
    if !TASK_BRANCH_TYPES.contains(&kind) {
        return Err(DeliveryError::InvalidBranch {
            branch: branch.to_owned(),
            reason: "type segment must be a Conventional Commits type",
        });
    }
    let segment_ok = |segment: &str| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
            && !segment.contains("..")
            && !segment.starts_with(['.', '-'])
            && Path::new(segment)
                .extension()
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("lock"))
    };
    if !segment_ok(slug) {
        return Err(DeliveryError::InvalidBranch {
            branch: branch.to_owned(),
            reason: "slug contains characters unsafe for a git ref",
        });
    }
    Ok(())
}

fn render_pull_request_body(spec: &DraftPullRequestSpec) -> String {
    let mut body = String::from("## Spec anchors\n\n");
    for anchor in &spec.spec_anchors {
        body.push_str("- ");
        body.push_str(anchor);
        body.push('\n');
    }
    body.push_str("\n## Evidence\n\n");
    for path in &spec.evidence_paths {
        body.push_str("- `");
        body.push_str(&path.display().to_string());
        body.push_str("`\n");
    }
    body.push_str(
        "\n---\nOpened as a draft by the Orchestraitor delivery path; review and merge remain human-gated.\n",
    );
    body
}

#[cfg(test)]
mod tests;

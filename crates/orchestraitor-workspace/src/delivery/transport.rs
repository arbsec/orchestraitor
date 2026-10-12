//! Injectable boundaries for the delivery path: the scoped push credential
//! provider and the pull-request HTTP transport.

use std::fmt;

use secrecy::SecretString;

use super::CredentialError;

/// Supplies the scoped, short-lived push credential. Production wiring backs
/// this with `orchestraitor-core`'s GitHub App installation-token minter;
/// delivery never mints and never falls back to ambient auth.
pub trait PushCredentialProvider: Send + Sync {
    /// Returns a usable credential or a typed failure.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] when no valid credential is available.
    fn push_credential(&self) -> Result<SecretString, CredentialError>;
}

/// Fully specified draft pull request passed to a [`PullRequestTransport`].
/// The credential is moved in by the delivery path; transports use it for
/// the API call and never persist or print it. `Debug` redacts it.
pub struct NewPullRequest {
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Pull request title.
    pub title: String,
    /// Branch carrying the delivered commits.
    pub head_branch: String,
    /// Base branch the pull request targets.
    pub base_branch: String,
    /// Markdown body (spec anchors + evidence paths, built by the caller
    /// request through [`Delivery::deliver`](crate::Delivery::deliver)).
    pub body: String,
    /// Always `true` on this path — delivery opens drafts only.
    pub draft: bool,
    /// Scoped credential for the API call.
    pub credential: SecretString,
}

impl fmt::Debug for NewPullRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewPullRequest")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("title", &self.title)
            .field("head_branch", &self.head_branch)
            .field("base_branch", &self.base_branch)
            .field("body", &self.body)
            .field("draft", &self.draft)
            .field("credential", &"REDACTED")
            .finish()
    }
}

/// Handle of an opened pull request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestHandle {
    /// Pull request number.
    pub number: u64,
    /// HTML URL of the pull request.
    pub url: String,
}

/// HTTP boundary for opening the draft pull request (`POST
/// /repos/{owner}/{repo}/pulls` with `draft: true`). Implementations live in
/// adapter crates; tests substitute a fixture (spec `50-contracts-data.md`
/// §21.3).
pub trait PullRequestTransport: Send + Sync {
    /// Opens the pull request and returns its handle.
    ///
    /// # Errors
    ///
    /// Returns a transport- or API-level failure. Implementations must never
    /// include credentials, headers, or tokens in the error.
    fn create_pull_request(
        &self,
        request: &NewPullRequest,
    ) -> Result<PullRequestHandle, Box<dyn std::error::Error + Send + Sync>>;
}

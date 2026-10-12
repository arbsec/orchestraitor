//! Snapshot-mode workspace controller backed by `gix`.
//!
//! The controller keeps Git metadata in the trusted original repository and
//! exports materialized worker directories with no `.git` entry.

#![forbid(unsafe_code)]

mod controller;
mod delivery;
mod history;
mod materialize;
mod symlink;
mod types;

pub use controller::WorkspaceController;
pub use delivery::{
    CommitIdentity, CredentialError, Delivery, DeliveryError, DeliveryOutcome, DeliveryRequest,
    DraftPullRequestSpec, GitError, NewPullRequest, PullRequestHandle, PullRequestTransport,
    PushCredentialProvider, TaskWorktree,
};
pub use history::{BlameLine, HistoryDiff, LogEntry, PathChange, PathChangeKind};
pub use types::{
    FileDigest, ReconciliationReport, Result, Snapshot, SnapshotOptions, WorkspaceError,
};

#[cfg(test)]
mod tests;

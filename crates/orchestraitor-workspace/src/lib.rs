//! Snapshot-mode workspace controller backed by `gix`.
//!
//! The controller keeps Git metadata in the trusted original repository and
//! exports materialized worker directories with no `.git` entry.

#![forbid(unsafe_code)]

mod controller;
mod history;
mod materialize;
mod symlink;
mod types;

pub use controller::WorkspaceController;
pub use history::{BlameLine, HistoryDiff, LogEntry, PathChange, PathChangeKind};
pub use types::{
    FileDigest, ReconciliationReport, Result, Snapshot, SnapshotOptions, WorkspaceError,
};

#[cfg(test)]
mod tests;

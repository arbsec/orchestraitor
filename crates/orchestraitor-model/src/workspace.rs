//! Workspace domain type (spec §18.3).

use crate::enums::{GitAccess, WorkspaceMode, WorkspaceTrustState};
use crate::ids::{ObjectId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Workspace model (spec §18.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Unique workspace identifier.
    pub id: WorkspaceId,
    /// Workspace mount mode (spec §9.4).
    pub mode: WorkspaceMode,
    /// Commit the workspace was forked from.
    pub base_commit: ObjectId,
    /// On-disk path of the workspace directory.
    pub path: PathBuf,
    /// Current trust state of the workspace (spec §9.14).
    pub trust_state: WorkspaceTrustState,
    /// Git access level granted to the workspace.
    pub git_access: GitAccess,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::enums::{GitAccess, WorkspaceMode, WorkspaceTrustState};
    use crate::ids::{ObjectId, WorkspaceId};

    #[test]
    fn workspace_round_trips() {
        let ws = Workspace {
            id: WorkspaceId::new(),
            mode: WorkspaceMode::Snapshot,
            base_commit: ObjectId("abc123".into()),
            path: PathBuf::from("/tmp/ws"),
            trust_state: WorkspaceTrustState::UntrustedExposed,
            git_access: GitAccess::NoGitAccess,
        };
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(ws, back);
    }
}

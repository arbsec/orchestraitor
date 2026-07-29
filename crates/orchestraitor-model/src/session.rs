//! Session domain type (spec §18.2).

use crate::digest::Digest;
use crate::enums::Timestamp;
use crate::enums::{SecurityMode, SessionState};
use crate::ids::{AdapterId, RepositoryId, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// Session model (spec §18.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: SessionId,
    /// Repository the session is bound to.
    pub repository_id: RepositoryId,
    /// Adapter that owns the agent loop for this session.
    pub adapter_id: AdapterId,
    /// Workspace backing this session.
    pub workspace_id: WorkspaceId,
    /// Security mode active for the session (spec §14).
    pub security_mode: SecurityMode,
    /// Digest of the policy in effect at the time the session was started.
    pub policy_digest: Digest,
    /// Lifecycle state of the session (spec §9.24.1).
    pub state: SessionState,
    /// Timestamp at which the session was created.
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::digest::Digest;
    use crate::enums::{SecurityMode, SessionState};
    use crate::ids::{AdapterId, RepositoryId, SessionId, WorkspaceId};

    #[test]
    fn session_round_trips() {
        let session = Session {
            id: SessionId::new(),
            repository_id: RepositoryId::new(),
            adapter_id: AdapterId::new(),
            workspace_id: WorkspaceId::new(),
            security_mode: SecurityMode::Standard,
            policy_digest: Digest::new("c".repeat(64)),
            state: SessionState::Queued,
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session.id, back.id);
        assert_eq!(session.security_mode, back.security_mode);
        assert_eq!(session.state, back.state);
    }
}

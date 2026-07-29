//! Strongly-typed identifiers used across the Orchestraitor workspace.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! id_newtype {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        #[doc = concat!("Opaque string identifier (`", stringify!($name), "`).")]
        pub struct $name(
            /// Underlying string identifier, preserved verbatim across round-trips.
            pub String,
        );

        impl $name {
            /// Creates a new unique identifier with the configured prefix.
            #[must_use]
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4()))
            }

            /// Wraps an existing string as this identifier type.
            #[must_use]
            pub fn from_string(s: String) -> Self {
                Self(s)
            }

            /// Returns the underlying string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(RepositoryId, "repo");
id_newtype!(SessionId, "sess");
id_newtype!(WorkspaceId, "ws");
id_newtype!(AdapterId, "adapter");
id_newtype!(ContextRequestId, "ctx");
id_newtype!(ProviderId, "provider");
id_newtype!(ModelId, "model");
id_newtype!(AgentId, "agent");
id_newtype!(OperationId, "op");

/// A Git object ID (commit SHA, tree SHA, blob SHA).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(
    /// Hex string of the Git object SHA.
    pub String,
);

impl ObjectId {
    /// Returns the underlying hex string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn session_id_round_trips() {
        let id = SessionId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn session_id_has_prefix() {
        let id = SessionId::new();
        assert!(id.as_str().starts_with("sess_"));
    }
}

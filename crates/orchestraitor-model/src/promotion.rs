//! Promotion receipt domain type (spec §18.5, §9.14).

use crate::digest::Digest;
use crate::enums::Timestamp;
use crate::ids::ObjectId;
use crate::ids::{RepositoryId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A path promoted through the output quarantine (spec §9.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotedPath {
    /// Repository-relative path of the promoted artifact.
    pub path: PathBuf,
    /// Digest of the file captured at staging time.
    pub source_digest: Digest,
    /// Digest of the file actually written into the target commit.
    pub target_digest: Digest,
    /// Output class assigned by inspection (spec §9.14).
    pub output_class: OutputClass,
}

/// Classification of worker output (spec §9.14 output classes).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum OutputClass {
    /// Plain source file (default class).
    OrdinarySource,
    /// Test file under a recognized test directory.
    Tests,
    /// Auto-generated source code.
    GeneratedSource,
    /// File with executable bit set.
    Executable,
    /// Archive of bundled artifacts (tar, zip, etc.).
    PackageArchive,
    /// Lockfile pinning dependency versions.
    DependencyLockfile,
    /// IDE workspace / project configuration.
    IdeConfiguration,
    /// Shell init / rc file (`.bashrc`, `.zshrc`, etc.).
    ShellConfiguration,
    /// Git configuration file (`.gitconfig`, `.gitattributes`).
    GitConfiguration,
    /// Git hook script (`.git/hooks/*`).
    GitHook,
    /// Agent / harness configuration (e.g. `AGENTS.md`).
    AgentConfiguration,
    /// Continuous integration workflow file.
    CiWorkflow,
    /// Plugin module for the artifact's build system.
    BuildSystemPlugin,
    /// Environment variable file (`.env`, `.envrc`).
    EnvironmentFile,
    /// Content matching credential patterns (always refused from auto-promotion).
    CredentialShapedData,
    /// Symbolic link — must be normalized before promotion.
    Symlink,
    /// Device node or other special file (always refused).
    DeviceOrSpecialFile,
}

/// Severity of a security finding (spec §9.33.4).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum FindingSeverity {
    /// Informational, no action required.
    Low,
    /// Notable issue; review recommended.
    Medium,
    /// Material defect; must be addressed before promotion.
    High,
    /// Blocker; promotion must be rejected until resolved.
    Critical,
}

/// A finding from inspection or review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Severity classification of the finding.
    pub severity: FindingSeverity,
    /// Excerpt from the artifact that triggered the finding.
    pub evidence: String,
    /// Repository-relative paths the finding applies to.
    pub affected_paths: Vec<String>,
    /// Identifier of the rule that was violated.
    pub violated_rule: String,
    /// Suggested fix surfaced to the trusted UI.
    pub proposed_remediation: String,
}

/// A reference to an approval (from Arbitraitor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRef {
    /// Digest of the plan that was approved.
    pub plan_digest: Digest,
    /// Digest of the specific artifact variant, if narrow approval.
    pub artifact_digest: Option<Digest>,
    /// Timestamp at which the approval stops being valid.
    pub expiry: Timestamp,
}

/// Promotion receipt (spec §18.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionReceipt {
    /// Workspace the promotion originated from.
    pub workspace_id: WorkspaceId,
    /// Digest of the captured workspace snapshot.
    pub source_digest: Digest,
    /// Repository that received the promoted artifacts.
    pub target_repository: RepositoryId,
    /// Paths promoted by this receipt.
    pub paths: Vec<PromotedPath>,
    /// Findings raised during inspection.
    pub findings: Vec<Finding>,
    /// Approvals relied on by this promotion.
    pub approvals: Vec<ApprovalRef>,
    /// Resulting commit object, if promotion advanced the target branch.
    pub resulting_commit: Option<ObjectId>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::digest::Digest;
    use crate::ids::ObjectId;
    use crate::ids::{RepositoryId, WorkspaceId};

    #[test]
    fn promotion_receipt_round_trips() {
        let receipt = PromotionReceipt {
            workspace_id: WorkspaceId::new(),
            source_digest: Digest::new("1".repeat(64)),
            target_repository: RepositoryId::new(),
            paths: vec![PromotedPath {
                path: PathBuf::from("src/main.rs"),
                source_digest: Digest::new("2".repeat(64)),
                target_digest: Digest::new("3".repeat(64)),
                output_class: OutputClass::OrdinarySource,
            }],
            findings: vec![],
            approvals: vec![],
            resulting_commit: Some(ObjectId("abc123".into())),
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let back: PromotionReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(receipt.workspace_id, back.workspace_id);
        assert_eq!(receipt.paths.len(), back.paths.len());
    }

    #[test]
    fn finding_severity_ord() {
        assert!(FindingSeverity::Critical > FindingSeverity::High);
        assert!(FindingSeverity::High > FindingSeverity::Medium);
        assert!(FindingSeverity::Medium > FindingSeverity::Low);
    }
}

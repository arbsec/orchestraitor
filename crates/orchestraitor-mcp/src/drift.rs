//! MCP drift fingerprinting for per-session audit records.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{McpGatewayError, McpGatewayResult};

/// Stable identity inputs for an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServerIdentity {
    /// Stable server id used for tool namespacing.
    pub server_id: String,
    /// Local executable SHA-256 for stdio servers.
    pub executable_sha256: Option<FingerprintDigest>,
    /// Remote TLS certificate or SPKI SHA-256 for remote servers.
    pub remote_spki_sha256: Option<FingerprintDigest>,
    /// Server manifest version.
    pub manifest_version: String,
    /// Capability schema version.
    pub capability_schema_version: String,
}

/// SHA-256 digest used in MCP fingerprints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct FingerprintDigest(String);

impl FingerprintDigest {
    /// Returns the digest string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Tool schema identity as observed from an `rmcp` tool object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolSchemaIdentity {
    /// Tool name, before server namespace is prepended.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON schema from the tool declaration.
    pub input_schema: serde_json::Value,
}

/// Declared and effective capabilities used for cross-checking.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilitySnapshot {
    /// Capabilities claimed by MCP server config or annotations. Advisory only.
    pub declared: Vec<String>,
    /// Capabilities effectively granted by Arbitraitor.
    pub effective: Vec<String>,
}

/// Result of declared-vs-effective capability comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityCrossCheck {
    /// Declared and effective capability sets match.
    Matched,
    /// Declared capabilities exceed effective grants.
    DeclaredExceedsEffective,
    /// Effective grants exceed declared capabilities and require renewed trust.
    EffectiveExpansion,
}

/// Per-session drift fingerprint for one MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DriftFingerprint {
    /// Server identity inputs.
    pub server: ServerIdentity,
    /// Combined digest of every tool's name, description, and input schema.
    pub schema_digest: FingerprintDigest,
    /// Capability comparison result.
    pub capability_cross_check: CapabilityCrossCheck,
}

impl DriftFingerprint {
    /// Builds a drift fingerprint from identity, tool schemas, and capability snapshots.
    ///
    /// # Errors
    /// Returns an error when canonical JSON serialization fails.
    pub fn build(
        server: ServerIdentity,
        tools: &[ToolSchemaIdentity],
        capabilities: &CapabilitySnapshot,
    ) -> McpGatewayResult<Self> {
        let schema_digest = digest_canonical_json(&tools.to_vec())?;
        let capability_cross_check = cross_check_capabilities(capabilities);
        Ok(Self {
            server,
            schema_digest,
            capability_cross_check,
        })
    }
}

/// Computes SHA-256 for a local executable.
///
/// # Errors
/// Returns an I/O error when the executable cannot be read.
pub fn executable_sha256(path: &Path) -> McpGatewayResult<FingerprintDigest> {
    let bytes = std::fs::read(path)?;
    Ok(sha256_digest(&bytes))
}

pub(crate) fn sha256_digest(bytes: &[u8]) -> FingerprintDigest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    FingerprintDigest(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn digest_canonical_json<T>(value: &T) -> McpGatewayResult<FingerprintDigest>
where
    T: Serialize,
{
    let bytes = serde_json_canonicalizer::to_vec(value).map_err(|error| {
        McpGatewayError::CanonicalJson {
            message: error.to_string(),
        }
    })?;
    Ok(sha256_digest(&bytes))
}

fn cross_check_capabilities(snapshot: &CapabilitySnapshot) -> CapabilityCrossCheck {
    let declared = snapshot
        .declared
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let effective = snapshot
        .effective
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if declared == effective {
        CapabilityCrossCheck::Matched
    } else if declared.is_superset(&effective) {
        CapabilityCrossCheck::DeclaredExceedsEffective
    } else {
        CapabilityCrossCheck::EffectiveExpansion
    }
}

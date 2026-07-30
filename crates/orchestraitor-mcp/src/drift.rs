//! MCP drift fingerprinting for per-session audit records.
//!
//! Implements §9.18.1: per-session identity hashing for every MCP server, plus the
//! renewed-trust comparison primitive callers use to decide whether to require explicit
//! user re-approval between sessions.

use std::collections::BTreeSet;
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
    /// Effective capabilities at fingerprint time.
    ///
    /// Persisted on the fingerprint so two sessions can be compared with the §9.18.1
    /// renewed-trust primitive ([`DriftFingerprint::compare`]) without re-plumbing the
    /// underlying [`CapabilitySnapshot`].
    pub effective_capabilities: Vec<String>,
}

/// Renewed-trust verdict returned by [`DriftFingerprint::compare`].
///
/// Implements §9.18.1: callers compare successive per-server fingerprints and use the
/// verdict to decide whether to require renewed user trust. Variants are ordered by
/// security severity, from highest to lowest, so a single fingerprint pair is reduced
/// to its most security-relevant change for actionable reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintChange {
    /// Identity inputs and effective capabilities are equal across the two fingerprints.
    NoChange,
    /// Local executable SHA-256 (or remote SPKI SHA-256) differs from the previous fingerprint.
    ExecutableChanged,
    /// Combined tool schema digest differs from the previous fingerprint.
    SchemaChanged,
    /// Effective capability set grew relative to the previous fingerprint.
    CapabilityExpanded,
    /// Effective capability set shrank relative to the previous fingerprint.
    CapabilityReduced,
}

impl DriftFingerprint {
    /// Builds a drift fingerprint from identity, tool schemas, and capability snapshots.
    ///
    /// Tool schemas are sorted by `(name, description, canonical input_schema)` before
    /// hashing so that equivalent tool sets in different declaration order produce the
    /// same schema digest. The effective capabilities are persisted on the fingerprint so
    /// [`DriftFingerprint::compare`] can detect expansion vs. reduction.
    ///
    /// # Errors
    /// Returns an error when canonical JSON serialization of the tool set fails.
    pub fn build(
        server: ServerIdentity,
        tools: &[ToolSchemaIdentity],
        capabilities: &CapabilitySnapshot,
    ) -> McpGatewayResult<Self> {
        let schema_digest = digest_sorted_tools(tools)?;
        let capability_cross_check = cross_check_capabilities(capabilities);
        Ok(Self {
            server,
            schema_digest,
            capability_cross_check,
            effective_capabilities: capabilities.effective.clone(),
        })
    }

    /// Compares this fingerprint to a successor and returns the most severe §9.18.1 change.
    ///
    /// Precedence: `ExecutableChanged` > `SchemaChanged` > `CapabilityExpanded` >
    /// `CapabilityReduced` > `NoChange`. When more than one kind of change co-occurs the
    /// most security-sensitive variant wins; callers can prompt on any non-`NoChange`
    /// verdict. Inputs are intentionally restricted to data already present on the
    /// fingerprint so the comparison is pure and does not require re-plumbing sessions.
    #[must_use]
    pub fn compare(&self, successor: &Self) -> FingerprintChange {
        if self.server.executable_sha256 != successor.server.executable_sha256
            || self.server.remote_spki_sha256 != successor.server.remote_spki_sha256
        {
            return FingerprintChange::ExecutableChanged;
        }
        if self.schema_digest != successor.schema_digest {
            return FingerprintChange::SchemaChanged;
        }
        let previous = effective_set(&self.effective_capabilities);
        let next = effective_set(&successor.effective_capabilities);
        if previous == next {
            FingerprintChange::NoChange
        } else if next.is_superset(&previous) {
            FingerprintChange::CapabilityExpanded
        } else if next.is_subset(&previous) {
            FingerprintChange::CapabilityReduced
        } else {
            // Overlapping but neither subset nor superset: at least one capability was
            // added and one was removed. Surface the more sensitive direction (expansion)
            // so policy defaults to renewed trust.
            FingerprintChange::CapabilityExpanded
        }
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

fn digest_sorted_tools(tools: &[ToolSchemaIdentity]) -> McpGatewayResult<FingerprintDigest> {
    let mut sorted: Vec<&ToolSchemaIdentity> = tools.iter().collect();
    sorted.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.description.cmp(&right.description))
            .then_with(|| {
                canonical_bytes(&left.input_schema).cmp(&canonical_bytes(&right.input_schema))
            })
    });
    let owned: Vec<ToolSchemaIdentity> = sorted.into_iter().cloned().collect();
    digest_canonical_json(&owned)
}

fn canonical_bytes(value: &serde_json::Value) -> Vec<u8> {
    serde_json_canonicalizer::to_vec(value).unwrap_or_default()
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
    let declared = snapshot.declared.iter().collect::<BTreeSet<_>>();
    let effective = snapshot.effective.iter().collect::<BTreeSet<_>>();
    if declared == effective {
        CapabilityCrossCheck::Matched
    } else if declared.is_superset(&effective) {
        CapabilityCrossCheck::DeclaredExceedsEffective
    } else {
        CapabilityCrossCheck::EffectiveExpansion
    }
}

fn effective_set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(server_id: &str) -> ServerIdentity {
        ServerIdentity {
            server_id: server_id.to_string(),
            executable_sha256: None,
            remote_spki_sha256: None,
            manifest_version: "1".to_string(),
            capability_schema_version: "1".to_string(),
        }
    }

    fn tool(name: &str, schema: serde_json::Value) -> ToolSchemaIdentity {
        ToolSchemaIdentity {
            name: name.to_string(),
            description: format!("{name} description"),
            input_schema: schema,
        }
    }

    fn capabilities(names: &[&str]) -> CapabilitySnapshot {
        CapabilitySnapshot {
            declared: names.iter().map(ToString::to_string).collect(),
            effective: names.iter().map(ToString::to_string).collect(),
        }
    }

    fn fingerprint(
        server_id: &str,
        tools: &[ToolSchemaIdentity],
        caps: &[&str],
    ) -> McpGatewayResult<DriftFingerprint> {
        DriftFingerprint::build(identity(server_id), tools, &capabilities(caps))
    }

    #[test]
    fn schema_digest_is_order_independent() -> McpGatewayResult<()> {
        let schema = serde_json::json!({"type": "object"});
        let t_a = tool("a", schema.clone());
        let t_b = tool("b", schema.clone());
        let t_c = tool("c", schema);

        let forward = fingerprint(
            "alpha",
            &[t_a.clone(), t_b.clone(), t_c.clone()],
            &["fs.read"],
        )?;
        let reverse = fingerprint("alpha", &[t_c, t_b, t_a], &["fs.read"])?;

        assert_eq!(forward.schema_digest, reverse.schema_digest);
        Ok(())
    }

    #[test]
    fn schema_digest_changes_when_tool_set_changes() -> McpGatewayResult<()> {
        let schema = serde_json::json!({"type": "object"});
        let other_schema = serde_json::json!({"type": "string"});
        let baseline = fingerprint("alpha", &[tool("a", schema.clone())], &["fs.read"])?;
        let added = fingerprint(
            "alpha",
            &[tool("a", schema.clone()), tool("b", other_schema.clone())],
            &["fs.read"],
        )?;
        let removed = fingerprint("alpha", &[], &["fs.read"])?;
        let renamed = fingerprint("alpha", &[tool("b", schema)], &["fs.read"])?;

        assert_ne!(baseline.schema_digest, added.schema_digest);
        assert_ne!(baseline.schema_digest, removed.schema_digest);
        // Same shape, different tool name → schema digest must differ.
        assert_ne!(baseline.schema_digest, renamed.schema_digest);
        Ok(())
    }

    #[test]
    fn compare_returns_no_change_for_identical_inputs() -> McpGatewayResult<()> {
        let previous = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        let successor = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        assert_eq!(previous.compare(&successor), FingerprintChange::NoChange);
        Ok(())
    }

    #[test]
    fn compare_reports_executable_change() -> McpGatewayResult<()> {
        let previous = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        let mut successor =
            fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        successor.server.executable_sha256 = Some(FingerprintDigest("sha256:newhash".to_string()));
        assert_eq!(
            previous.compare(&successor),
            FingerprintChange::ExecutableChanged,
        );
        Ok(())
    }

    #[test]
    fn compare_reports_schema_change_when_tools_drift() -> McpGatewayResult<()> {
        let previous = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        let successor = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({"required": ["x"]}))],
            &["fs.read"],
        )?;
        assert_eq!(
            previous.compare(&successor),
            FingerprintChange::SchemaChanged
        );
        Ok(())
    }

    #[test]
    fn compare_reports_capability_expansion() -> McpGatewayResult<()> {
        let previous = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        let successor = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({}))],
            &["fs.read", "fs.write"],
        )?;
        assert_eq!(
            previous.compare(&successor),
            FingerprintChange::CapabilityExpanded,
        );
        Ok(())
    }

    #[test]
    fn compare_reports_capability_reduction() -> McpGatewayResult<()> {
        let previous = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({}))],
            &["fs.read", "fs.write"],
        )?;
        let successor = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        assert_eq!(
            previous.compare(&successor),
            FingerprintChange::CapabilityReduced,
        );
        Ok(())
    }

    #[test]
    fn compare_prefers_executable_over_schema_over_capabilities() -> McpGatewayResult<()> {
        // Both executable and schema changed; schema change should not mask the more
        // severe executable change.
        let previous = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        let mut successor = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({"required": ["x"]}))],
            &["fs.read", "fs.write"],
        )?;
        successor.server.executable_sha256 = Some(FingerprintDigest("sha256:diff".to_string()));
        assert_eq!(
            previous.compare(&successor),
            FingerprintChange::ExecutableChanged,
        );

        // Schema changed AND capability expanded → schema takes precedence.
        let baseline = fingerprint("alpha", &[tool("a", serde_json::json!({}))], &["fs.read"])?;
        let both = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({"required": ["x"]}))],
            &["fs.read", "fs.write"],
        )?;
        assert_eq!(baseline.compare(&both), FingerprintChange::SchemaChanged);
        Ok(())
    }

    #[test]
    fn compare_reports_expansion_for_overlapping_neither_subset_direction() -> McpGatewayResult<()>
    {
        // {read, scan} → {read, write}: the two sets neither superset nor subset the
        // other; the more sensitive direction (expansion) wins per §9.18.1.
        let previous = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({}))],
            &["fs.read", "fs.scan"],
        )?;
        let successor = fingerprint(
            "alpha",
            &[tool("a", serde_json::json!({}))],
            &["fs.read", "fs.write"],
        )?;
        assert_eq!(
            previous.compare(&successor),
            FingerprintChange::CapabilityExpanded,
        );
        Ok(())
    }
}

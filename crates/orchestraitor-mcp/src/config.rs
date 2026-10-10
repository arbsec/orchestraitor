//! Canonical `.agent/mcp.toml` schema and layered server resolution.

use std::collections::BTreeMap;
use std::path::Path;

use orchestraitor_core::{ConfigLayer, ConfigSource};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{McpGatewayError, McpGatewayResult};

/// Canonical MCP config file relative to a project root.
pub const CANONICAL_MCP_CONFIG_PATH: &str = ".agent/mcp.toml";

/// Canonical `.agent/mcp.toml` schema owned by Orchestraitor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Schema version. Version 1 is the MVP format.
    pub version: u32,
    /// Imported compatibility config may be represented but is never launched by default.
    #[serde(default)]
    pub launch_imported_servers: bool,
    /// Server definitions keyed by stable server id.
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

/// One resolved layer of MCP config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfigLayer {
    /// Config provenance.
    pub source: ConfigSource,
    /// Parsed MCP config.
    pub config: McpConfig,
}

/// MCP server definition from `.agent/mcp.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Whether this server is registered for the project.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Server lifetime. Defaults to spec §17.2 `project-stateful`.
    #[serde(default)]
    pub lifetime: McpServerLifetime,
    /// Whether this entry was imported from compatibility config.
    #[serde(default)]
    pub imported: bool,
    /// Transport details.
    pub transport: McpTransportConfig,
    /// Declared capability labels. Advisory only; Arbitraitor remains authoritative.
    #[serde(default)]
    pub declared_capabilities: Vec<String>,
    /// Manifest version used by drift fingerprinting.
    #[serde(default = "default_manifest_version")]
    pub manifest_version: String,
    /// Capability schema version used by drift fingerprinting.
    #[serde(default = "default_capability_schema_version")]
    pub capability_schema_version: String,
}

/// MCP server lifetime classification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum McpServerLifetime {
    /// Entire daemon lifetime, stateless.
    GlobalStateless,
    /// Entire daemon lifetime with credentials.
    GlobalAuthenticated,
    /// Project-session read-only state.
    ProjectReadonly,
    /// Project-session read-write state.
    #[default]
    ProjectStateful,
    /// Agent-session writable state.
    SessionWritable,
    /// Single task ephemeral server.
    TaskEphemeral,
}

/// Supported MCP server transports in the canonical config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransportConfig {
    /// Local stdio process. Must be inspected by Arbitraitor before launch.
    Stdio {
        /// Executable path or command name.
        command: String,
        /// Command arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Environment variable names this server requests.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// Remote streamable HTTP MCP server.
    StreamableHttp {
        /// Server URL.
        url: String,
    },
}

/// Resolved project server set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedMcpServers {
    /// Effective server map.
    pub servers: BTreeMap<String, McpServerConfig>,
}

/// Loads the canonical `.agent/mcp.toml` file if it exists.
///
/// # Errors
/// Returns an I/O or TOML parse error when the file exists but cannot be loaded.
pub fn load_canonical_mcp_config(project_root: &Path) -> McpGatewayResult<Option<McpConfig>> {
    let path = project_root.join(CANONICAL_MCP_CONFIG_PATH);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    parse_mcp_toml(&text).map(Some)
}

/// Parses canonical MCP TOML.
///
/// # Errors
/// Returns a TOML parse error when the document does not match the schema.
pub fn parse_mcp_toml(text: &str) -> McpGatewayResult<McpConfig> {
    toml::from_str(text).map_err(|error| McpGatewayError::Toml(Box::new(error)))
}

/// Resolves project server definitions through layered config precedence.
#[must_use]
pub fn resolve_mcp_servers(layers: &[McpConfigLayer]) -> ResolvedMcpServers {
    let mut ordered = layers.to_vec();
    ordered.sort_by_key(|layer| layer.source.layer);
    let mut servers = BTreeMap::new();
    for layer in ordered {
        for (server_id, server) in layer.config.servers {
            if server.enabled {
                servers.insert(server_id, server);
            } else {
                servers.remove(&server_id);
            }
        }
    }
    ResolvedMcpServers { servers }
}

impl McpConfigLayer {
    /// Builds a project-layer canonical config.
    #[must_use]
    pub fn project(config: McpConfig) -> Self {
        Self {
            source: ConfigSource {
                layer: ConfigLayer::Project,
                name: CANONICAL_MCP_CONFIG_PATH.to_string(),
            },
            config,
        }
    }
}

const fn default_enabled() -> bool {
    true
}

fn default_manifest_version() -> String {
    "1".to_string()
}

fn default_capability_schema_version() -> String {
    "1".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_toml_parses() -> McpGatewayResult<()> {
        let toml = r#"
version = 1

[servers.context7]
lifetime = "project-readonly"
declared_capabilities = ["network"]

[servers.context7.transport]
type = "streamable_http"
url = "https://example.invalid/mcp"

[servers.local.transport]
type = "stdio"
command = "context-server"
args = ["--project", "."]
"#;
        let config = parse_mcp_toml(toml)?;
        assert_eq!(config.version, 1);
        assert_eq!(config.servers.len(), 2);
        assert_eq!(
            config.servers["context7"].lifetime,
            McpServerLifetime::ProjectReadonly
        );
        Ok(())
    }

    #[test]
    fn higher_layer_overrides_server_definition() {
        let low = McpConfigLayer {
            source: ConfigSource {
                layer: ConfigLayer::GlobalUser,
                name: "user".to_string(),
            },
            config: McpConfig {
                version: 1,
                launch_imported_servers: false,
                servers: BTreeMap::from([(
                    "docs".to_string(),
                    McpServerConfig {
                        enabled: true,
                        lifetime: McpServerLifetime::GlobalStateless,
                        imported: false,
                        transport: McpTransportConfig::StreamableHttp {
                            url: "https://global.invalid".to_string(),
                        },
                        declared_capabilities: Vec::new(),
                        manifest_version: "1".to_string(),
                        capability_schema_version: "1".to_string(),
                    },
                )]),
            },
        };
        let high = McpConfigLayer::project(McpConfig {
            version: 1,
            launch_imported_servers: false,
            servers: BTreeMap::from([(
                "docs".to_string(),
                McpServerConfig {
                    enabled: false,
                    lifetime: McpServerLifetime::ProjectStateful,
                    imported: false,
                    transport: McpTransportConfig::StreamableHttp {
                        url: "https://project.invalid".to_string(),
                    },
                    declared_capabilities: Vec::new(),
                    manifest_version: "1".to_string(),
                    capability_schema_version: "1".to_string(),
                },
            )]),
        });
        let resolved = resolve_mcp_servers(&[low, high]);
        assert!(resolved.servers.is_empty());
    }
}

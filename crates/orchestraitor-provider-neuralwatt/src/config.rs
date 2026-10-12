//! Configuration for the Neuralwatt provider transport.
//!
//! The default base URL is `https://api.neuralwatt.com/v1` (spec §10.3).
//! The legacy Zhipu endpoint `https://open.bigmodel.cn/api/paas/v4/` MUST
//! NOT be used as a default and is rejected at construction time.

use std::env;

use orchestraitor_core::SecretUri;
use secrecy::SecretString;
use url::Url;

use crate::error::NeuralwattError;

/// Default Neuralwatt API base URL (spec §10.3, tech-stack §3.1).
pub const DEFAULT_NEURALWATT_BASE_URL: &str = "https://api.neuralwatt.com/v1";

/// Environment variable name for the Neuralwatt API key (tech-stack §3.2).
pub const NEURALWATT_ENV_VAR: &str = "NEURALWATT_API_KEY";

/// Default keyring entry id for the Neuralwatt API key.
const DEFAULT_KEYRING_ID: &str = "neuralwatt";

/// Hosts that are explicitly forbidden as default base URLs (spec §10.3).
const FORBIDDEN_HOST: &str = "open.bigmodel.cn";

/// Configuration for [`crate::NeuralwattTransport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeuralwattConfig {
    /// Base URL for the OpenAI-compatible API endpoint.
    base_url: String,
    /// Secret URI for API key resolution (e.g. `secret://keyring/neuralwatt`).
    auth_uri: String,
}

impl NeuralwattConfig {
    /// Creates a config with the default Neuralwatt base URL and keyring auth.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_NEURALWATT_BASE_URL.to_string(),
            auth_uri: format!("secret://keyring/{DEFAULT_KEYRING_ID}"),
        }
    }

    /// Creates a config with an explicit base URL and auth URI.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError::ForbiddenHost`] when the base URL host is
    /// `open.bigmodel.cn` (spec §10.3), or [`NeuralwattError::InvalidBaseUrl`]
    /// when the URL cannot be parsed.
    pub fn with_endpoint(base_url: String, auth_uri: String) -> Result<Self, NeuralwattError> {
        validate_base_url(&base_url)?;
        Ok(Self { base_url, auth_uri })
    }

    /// Overrides the base URL.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError::ForbiddenHost`] when the host is forbidden.
    pub fn with_base_url(mut self, base_url: String) -> Result<Self, NeuralwattError> {
        validate_base_url(&base_url)?;
        self.base_url = base_url;
        Ok(self)
    }

    /// Overrides the auth URI.
    #[must_use]
    pub fn with_auth_uri(mut self, auth_uri: String) -> Self {
        self.auth_uri = auth_uri;
        self
    }

    /// Returns the configured base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns the configured auth URI.
    #[must_use]
    pub fn auth_uri(&self) -> &str {
        &self.auth_uri
    }

    /// Resolves the API key from the configured auth URI.
    ///
    /// Resolution order:
    /// 1. `secret://env/<VAR>` — reads the env var directly.
    /// 2. `secret://keyring/neuralwatt` — attempts keyring, falls back to
    ///    `NEURALWATT_API_KEY` env var when keyring is unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError::Auth`] when the secret cannot be resolved.
    pub fn resolve_api_key(&self) -> Result<SecretString, NeuralwattError> {
        let uri = SecretUri::parse(&self.auth_uri)
            .map_err(|_| NeuralwattError::Auth(format!("invalid auth URI: {}", self.auth_uri)))?;
        match uri.store {
            orchestraitor_core::SecretStore::Env => {
                let value = env::var(&uri.id).map_err(|_| {
                    NeuralwattError::Auth(format!("environment variable `{}` is not set", uri.id))
                })?;
                if value.is_empty() {
                    return Err(NeuralwattError::Auth(format!(
                        "environment variable `{}` is empty",
                        uri.id
                    )));
                }
                Ok(SecretString::from(value))
            }
            orchestraitor_core::SecretStore::Keyring => {
                // Keyring resolution is not available in this crate (spec §9.23).
                // Fall back to the NEURALWATT_API_KEY env var.
                let value = env::var(NEURALWATT_ENV_VAR).map_err(|_| {
                    NeuralwattError::Auth(format!(
                        "keyring unavailable and fallback env var `{NEURALWATT_ENV_VAR}` is not set"
                    ))
                })?;
                if value.is_empty() {
                    return Err(NeuralwattError::Auth(format!(
                        "fallback env var `{NEURALWATT_ENV_VAR}` is empty"
                    )));
                }
                Ok(SecretString::from(value))
            }
        }
    }
}

impl Default for NeuralwattConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Validates that a base URL is well-formed and not a forbidden host.
fn validate_base_url(base_url: &str) -> Result<(), NeuralwattError> {
    let parsed =
        Url::parse(base_url).map_err(|source| NeuralwattError::InvalidBaseUrl { source })?;
    if let Some(host) = parsed.host_str()
        && host == FORBIDDEN_HOST
    {
        return Err(NeuralwattError::ForbiddenHost {
            host: host.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn default_config_uses_neuralwatt_endpoint() {
        let config = NeuralwattConfig::new();
        assert_eq!(config.base_url(), DEFAULT_NEURALWATT_BASE_URL);
        assert_eq!(config.auth_uri(), "secret://keyring/neuralwatt");
    }

    #[test]
    fn default_base_url_is_neuralwatt_com_not_bigmodel_cn() {
        let config = NeuralwattConfig::new();
        assert!(
            config.base_url().contains("neuralwatt.com"),
            "base URL must be neuralwatt.com, got: {}",
            config.base_url()
        );
        assert!(
            !config.base_url().contains("bigmodel.cn"),
            "base URL must never be bigmodel.cn"
        );
    }

    #[test]
    fn rejects_bigmodel_cn_host() {
        let result = NeuralwattConfig::with_endpoint(
            "https://open.bigmodel.cn/api/paas/v4/".to_string(),
            "secret://keyring/neuralwatt".to_string(),
        );
        assert!(matches!(result, Err(NeuralwattError::ForbiddenHost { .. })));
    }

    #[test]
    fn accepts_z_ai_endpoint() {
        let config = NeuralwattConfig::with_endpoint(
            "https://api.z.ai/api/paas/v4/".to_string(),
            "secret://env/ZHIPU_API_KEY".to_string(),
        )
        .unwrap();
        assert_eq!(config.base_url(), "https://api.z.ai/api/paas/v4/");
    }

    #[test]
    fn rejects_invalid_url() {
        let result = NeuralwattConfig::with_endpoint(
            "not a url".to_string(),
            "secret://keyring/neuralwatt".to_string(),
        );
        assert!(matches!(
            result,
            Err(NeuralwattError::InvalidBaseUrl { .. })
        ));
    }
}

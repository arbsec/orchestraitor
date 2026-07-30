//! Authentication resolver and secret-reference helpers.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use orchestraitor_core::{SecretStore, SecretUri};
use orchestraitor_model::ProviderId;
use secrecy::SecretString;
use serde::{Serialize, Serializer};
use thiserror::Error;

/// Authentication resolver returning in-memory secrets for explicit providers.
#[async_trait]
pub trait AuthResolver: Send + Sync {
    /// Resolves the secret for a provider id.
    ///
    /// # Errors
    ///
    /// Returns an auth error when the provider has no configured secret or the backing source fails.
    async fn resolve(&self, provider_id: &ProviderId) -> Result<SecretString, AuthError>;
}

/// Env-backed auth resolver for CI and headless environments.
#[derive(Debug, Clone, Default)]
pub struct EnvAuthResolver {
    env_by_provider: HashMap<ProviderId, String>,
}

impl EnvAuthResolver {
    /// Creates an empty env auth resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a provider-to-env-var mapping.
    #[must_use]
    pub fn with_provider_env(mut self, provider_id: ProviderId, env_var: String) -> Self {
        self.env_by_provider.insert(provider_id, env_var);
        self
    }
}

#[async_trait]
impl AuthResolver for EnvAuthResolver {
    async fn resolve(&self, provider_id: &ProviderId) -> Result<SecretString, AuthError> {
        let env_var = self
            .env_by_provider
            .get(provider_id)
            .ok_or_else(|| AuthError::MissingProvider(provider_id.clone()))?;
        let value = env::var(env_var).map_err(|source| AuthError::EnvVar {
            provider_id: provider_id.clone(),
            env_var: env_var.clone(),
            source,
        })?;
        if value.is_empty() {
            return Err(AuthError::EmptySecret {
                provider_id: provider_id.clone(),
                source_label: env_var.clone(),
            });
        }
        Ok(SecretString::from(value))
    }
}

/// Parsed secret reference accepted by provider authentication configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretReference {
    /// Secret reference backed by OS keyring or env.
    Uri(SecretUri),
    /// Plaintext literal accepted only in debug builds.
    PlaintextDebugOnly,
}

impl SecretReference {
    /// Parses a secret reference and refuses plaintext in release builds.
    ///
    /// # Errors
    ///
    /// Returns an auth error when the input is not an allowed secret reference.
    pub fn parse(input: &str) -> Result<Self, AuthError> {
        match SecretUri::parse(input) {
            Ok(uri) => Ok(Self::Uri(uri)),
            Err(_) if cfg!(debug_assertions) && !input.is_empty() => Ok(Self::PlaintextDebugOnly),
            Err(_) => Err(AuthError::PlaintextRefused),
        }
    }
}

impl FromStr for SecretReference {
    type Err = AuthError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

/// Resolves an env secret URI into an in-memory secret.
///
/// # Errors
///
/// Returns an auth error for unsupported keyring resolution, missing env vars, or empty values.
pub fn resolve_secret_uri(secret_uri: &SecretUri) -> Result<SecretString, AuthError> {
    match secret_uri.store {
        SecretStore::Env => {
            let value = env::var(&secret_uri.id).map_err(|source| AuthError::EnvVar {
                provider_id: ProviderId::from_string("unknown".to_string()),
                env_var: secret_uri.id.clone(),
                source,
            })?;
            if value.is_empty() {
                return Err(AuthError::EmptySecret {
                    provider_id: ProviderId::from_string("unknown".to_string()),
                    source_label: secret_uri.id.clone(),
                });
            }
            Ok(SecretString::from(value))
        }
        SecretStore::Keyring => Err(AuthError::KeyringUnavailable),
    }
}

/// Secret wrapper whose serialization never exposes the secret value.
pub struct RedactedSecret(SecretString);

impl RedactedSecret {
    /// Wraps a secret for redacted serialization.
    #[must_use]
    pub fn new(secret: SecretString) -> Self {
        Self(secret)
    }

    /// Consumes the wrapper and returns the inner secret.
    #[must_use]
    pub fn into_inner(self) -> SecretString {
        self.0
    }
}

impl fmt::Debug for RedactedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RedactedSecret(REDACTED)")
    }
}

impl Serialize for RedactedSecret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("REDACTED")
    }
}

/// Authentication resolution failures without secret material.
#[derive(Debug, Error)]
pub enum AuthError {
    /// Provider had no configured authentication source.
    #[error("provider `{0}` has no configured authentication source")]
    MissingProvider(ProviderId),
    /// Environment variable lookup failed.
    #[error(
        "provider `{provider_id}` environment authentication source `{env_var}` is unavailable"
    )]
    EnvVar {
        /// Explicit provider id.
        provider_id: ProviderId,
        /// Environment variable name, never its value.
        env_var: String,
        /// Underlying environment lookup error.
        #[source]
        source: env::VarError,
    },
    /// Secret source returned an empty value.
    #[error("provider `{provider_id}` authentication source `{source_label}` is empty")]
    EmptySecret {
        /// Explicit provider id.
        provider_id: ProviderId,
        /// Non-secret source label.
        source_label: String,
    },
    /// Plaintext secret references are refused in release builds.
    #[error("plaintext provider secret references are refused")]
    PlaintextRefused,
    /// Keyring resolution is not available in this crate.
    #[error("keyring secret resolution is unavailable in provider-api")]
    KeyringUnavailable,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use secrecy::ExposeSecret;

    use super::*;

    #[tokio::test]
    async fn auth_resolver_returns_secret_string_from_env() -> Result<(), AuthError> {
        let provider_id = ProviderId::from_string("path-provider".to_string());
        let resolver =
            EnvAuthResolver::new().with_provider_env(provider_id.clone(), "PATH".to_string());

        let secret = resolver.resolve(&provider_id).await?;

        assert!(!secret.expose_secret().is_empty());
        Ok(())
    }

    #[test]
    fn redacted_secret_serializes_as_redacted() -> Result<(), serde_json::Error> {
        let rendered = serde_json::to_string(&RedactedSecret::new(SecretString::from(
            "super-secret-value".to_string(),
        )))?;

        assert_eq!(rendered, "\"REDACTED\"");
        assert!(!rendered.contains("super-secret-value"));
        Ok(())
    }
}

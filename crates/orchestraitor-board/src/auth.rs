//! Bearer-token seam for GitHub board access.
//!
//! The board client never reads ambient credentials. Callers inject a
//! [`BoardAuth`] implementation; the bootstrap stub resolves the token from
//! the secret URI declared in the board project config (spec §9.23).
//!
//! # Follow-up
//!
//! The `arbsec-agent` GitHub App identity module (tracked separately) will
//! provide a concrete [`BoardAuth`] that mints short-lived installation
//! tokens. Until then, reads and writes authenticate through the explicitly
//! configured owner-fallback token, labelled as such in configuration.

use std::env;

use async_trait::async_trait;
use orchestraitor_core::SecretStore;
use orchestraitor_core::SecretUri;
use secrecy::SecretString;

use crate::BoardError;

/// Resolves a bearer token for GitHub API calls.
///
/// Implementations must return the token in memory only and must never log,
/// serialize, or persist it.
#[async_trait]
pub trait BoardAuth: Send + Sync {
    /// Resolves a GitHub bearer token.
    ///
    /// # Errors
    ///
    /// Returns a typed error describing the auth source; never the token value.
    async fn bearer_token(&self) -> Result<SecretString, BoardError>;
}

/// Config-driven bootstrap auth: resolves `secret://env/<VAR>` only.
///
/// Keyring URIs produce a typed unavailable error rather than falling back to
/// ambient credentials.
#[derive(Debug, Clone)]
pub struct SecretUriAuth {
    uri: SecretUri,
}

impl SecretUriAuth {
    /// Creates auth from a parsed secret URI.
    #[must_use]
    pub fn new(uri: SecretUri) -> Self {
        Self { uri }
    }
}

#[async_trait]
impl BoardAuth for SecretUriAuth {
    async fn bearer_token(&self) -> Result<SecretString, BoardError> {
        match &self.uri.store {
            SecretStore::Env => {
                let value = env::var(&self.uri.id).map_err(|_source| BoardError::AuthEnvVar {
                    var: self.uri.id.clone(),
                })?;
                if value.is_empty() {
                    return Err(BoardError::AuthEnvVar {
                        var: self.uri.id.clone(),
                    });
                }
                Ok(SecretString::from(value))
            }
            SecretStore::Keyring => Err(BoardError::AuthKeyringUnavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[tokio::test]
    async fn env_uri_resolves_existing_variable() -> Result<(), BoardError> {
        // `PATH` is present in every test process; its value is irrelevant.
        let auth = SecretUriAuth::new(SecretUri::parse("secret://env/PATH")?);
        let token = auth.bearer_token().await?;
        let expected = env::var("PATH").map_err(|_source| BoardError::AuthEnvVar {
            var: "PATH".to_string(),
        })?;
        assert_eq!(token.expose_secret(), &expected);
        Ok(())
    }

    #[tokio::test]
    async fn missing_env_uri_is_typed_not_ambient() -> Result<(), BoardError> {
        let auth = SecretUriAuth::new(SecretUri::parse(
            "secret://env/ORCHESTRAITOR_BOARD_TEST_ABSENT_VAR",
        )?);
        let result = auth.bearer_token().await;
        assert!(matches!(
            result,
            Err(BoardError::AuthEnvVar { ref var })
                if var == "ORCHESTRAITOR_BOARD_TEST_ABSENT_VAR"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn keyring_uri_is_a_typed_unavailable_error() -> Result<(), BoardError> {
        let auth = SecretUriAuth::new(SecretUri::parse("secret://keyring/github")?);
        let result = auth.bearer_token().await;
        assert!(matches!(result, Err(BoardError::AuthKeyringUnavailable)));
        if let Err(error) = result {
            assert!(!error.to_string().contains("github.com/"));
        }
        Ok(())
    }
}

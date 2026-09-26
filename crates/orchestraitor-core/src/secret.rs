//! Secret URI shape parsing and `secret://` resolution.
//!
//! Resolution is exact-store: `secret://env/<VAR>` reads the named environment
//! variable and `secret://keyring/<id>` reads the platform keyring under the
//! caller's service label. There is no cross-store or ambient-credential
//! fallback — an unresolvable reference fails closed (spec
//! `40-arbitraitor-integration.md` §9.23).

use std::env;
use std::str::FromStr;

use schemars::JsonSchema;
use secrecy::SecretString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{ConfigError, SecretResolveError};

/// Default keyring service label for `secret://keyring/<id>` resolution,
/// matching `[secrets].keyring_service` (spec `40-arbitraitor-integration.md` §9.23.1).
pub const DEFAULT_KEYRING_SERVICE: &str = "orchestraitor";

/// Supported secret backing stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SecretStore {
    /// Secret id held by the platform keyring.
    Keyring,
    /// Secret name held by the process environment.
    Env,
}

/// Parsed secret URI reference.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct SecretUri {
    /// Store selector parsed from the URI.
    pub store: SecretStore,
    /// Store-specific secret identifier.
    pub id: String,
}

impl SecretUri {
    /// Parses a secret URI shape without resolving the secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the URI is not a supported secret reference shape.
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        if let Some(id) = input.strip_prefix("secret://keyring/") {
            return parse_id(SecretStore::Keyring, id);
        }
        if let Some(id) = input.strip_prefix("secret://env/") {
            return parse_env(id);
        }
        if let Some(id) = input.strip_prefix("env:") {
            return parse_env(id);
        }
        Err(ConfigError::SecretUri)
    }

    /// Renders the non-secret URI reference.
    #[must_use]
    pub fn as_uri(&self) -> String {
        match self.store {
            SecretStore::Keyring => format!("secret://keyring/{}", self.id),
            SecretStore::Env => format!("secret://env/{}", self.id),
        }
    }

    /// Resolves this reference into an in-memory secret.
    ///
    /// Resolution is exact-store with no fallback: an env URI reads only the
    /// named environment variable and a keyring URI reads only the platform
    /// keyring entry under `keyring_service`.
    ///
    /// # Errors
    ///
    /// Returns [`SecretResolveError`] when the reference cannot be resolved;
    /// the error never contains the secret value.
    pub fn resolve(&self, keyring_service: &str) -> Result<SecretString, SecretResolveError> {
        match self.store {
            SecretStore::Env => {
                let value = env::var(&self.id).map_err(|_| SecretResolveError::EnvMissing {
                    id: self.id.clone(),
                })?;
                if value.is_empty() {
                    return Err(SecretResolveError::Empty {
                        label: format!("env:{}", self.id),
                    });
                }
                Ok(SecretString::from(value))
            }
            SecretStore::Keyring => resolve_keyring(self, keyring_service),
        }
    }
}

#[cfg(feature = "secrets-keyring")]
fn resolve_keyring(uri: &SecretUri, service: &str) -> Result<SecretString, SecretResolveError> {
    let entry = keyring::Entry::new(service, &uri.id).map_err(|source| {
        SecretResolveError::KeyringLookup {
            service: service.to_string(),
            id: uri.id.clone(),
            source: Box::new(source),
        }
    })?;
    let value = entry
        .get_password()
        .map_err(|source| SecretResolveError::KeyringLookup {
            service: service.to_string(),
            id: uri.id.clone(),
            source: Box::new(source),
        })?;
    if value.is_empty() {
        return Err(SecretResolveError::Empty {
            label: format!("keyring:{service}/{}", uri.id),
        });
    }
    Ok(SecretString::from(value))
}

#[cfg(not(feature = "secrets-keyring"))]
fn resolve_keyring(_uri: &SecretUri, _service: &str) -> Result<SecretString, SecretResolveError> {
    Err(SecretResolveError::KeyringDisabled)
}

impl FromStr for SecretUri {
    type Err = ConfigError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl Serialize for SecretUri {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_uri())
    }
}

impl<'de> Deserialize<'de> for SecretUri {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

fn parse_id(store: SecretStore, id: &str) -> Result<SecretUri, ConfigError> {
    if id.is_empty() || id.contains('/') {
        return Err(ConfigError::SecretUri);
    }
    Ok(SecretUri {
        store,
        id: id.to_string(),
    })
}

fn parse_env(id: &str) -> Result<SecretUri, ConfigError> {
    if id.is_empty()
        || id.contains('/')
        || !id
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(ConfigError::SecretUri);
    }
    Ok(SecretUri {
        store: SecretStore::Env,
        id: id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_uri_parses_supported_shapes() -> Result<(), ConfigError> {
        assert_eq!(
            SecretUri::parse("secret://keyring/provider")?,
            SecretUri {
                store: SecretStore::Keyring,
                id: "provider".to_string()
            }
        );
        assert_eq!(
            SecretUri::parse("secret://env/ZHIPU_API_KEY")?,
            SecretUri {
                store: SecretStore::Env,
                id: "ZHIPU_API_KEY".to_string()
            }
        );
        assert_eq!(
            SecretUri::parse("env:OPENAI_API_KEY")?.as_uri(),
            "secret://env/OPENAI_API_KEY"
        );
        Ok(())
    }

    #[test]
    fn secret_uri_rejects_resolution_or_invalid_shapes() {
        assert!(SecretUri::parse("plain-secret").is_err());
        assert!(SecretUri::parse("secret://env/").is_err());
        assert!(SecretUri::parse("secret://keyring/a/b").is_err());
    }
}

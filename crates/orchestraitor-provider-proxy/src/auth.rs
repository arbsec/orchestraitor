//! Local proxy authentication and upstream credential isolation.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestraitor_model::ProviderId;
use orchestraitor_provider_api::{AuthResolver, RedactedSecret};
use secrecy::SecretString;
use uuid::Uuid;

use crate::{ProxyError, ProxyResult};

/// Short-lived local bearer token accepted by the provider-compatible proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAuthToken {
    value: String,
    expires_at: Instant,
}

impl LocalAuthToken {
    /// Returns the bearer token text that is safe to give only to the local harness.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns whether the token has expired at `now`.
    #[must_use]
    pub fn is_expired_at(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// Issues and validates short-lived local bearer tokens.
#[derive(Debug, Clone)]
pub struct LocalTokenIssuer {
    ttl: Duration,
    issued: Arc<Mutex<HashMap<String, Instant>>>,
}

impl LocalTokenIssuer {
    /// Creates a local token issuer with the supplied TTL.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            issued: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Issues a fresh local-only bearer token.
    ///
    /// # Errors
    /// Returns an authentication error if the token table is unavailable.
    pub fn issue(&self) -> ProxyResult<LocalAuthToken> {
        let value = format!("orc-local-{}", Uuid::new_v4());
        let expires_at = Instant::now() + self.ttl;
        self.issued
            .lock()
            .map_err(|_| ProxyError::LocalAuthentication)?
            .insert(value.clone(), expires_at);
        Ok(LocalAuthToken { value, expires_at })
    }

    /// Validates a bearer token from an HTTP `Authorization` header.
    ///
    /// # Errors
    /// Returns an authentication error for missing, unknown, or expired tokens.
    pub fn validate_bearer(&self, authorization: Option<&str>) -> ProxyResult<()> {
        let token = authorization
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(ProxyError::LocalAuthentication)?;
        let now = Instant::now();
        let mut issued = self
            .issued
            .lock()
            .map_err(|_| ProxyError::LocalAuthentication)?;
        match issued.get(token).copied() {
            Some(expires_at) if now < expires_at => Ok(()),
            Some(_) => {
                issued.remove(token);
                Err(ProxyError::LocalAuthentication)
            }
            None => Err(ProxyError::LocalAuthentication),
        }
    }
}

/// Resolves upstream BYOK credentials inside the proxy process only.
pub struct UpstreamCredentialBroker<R> {
    resolver: R,
}

impl<R> UpstreamCredentialBroker<R>
where
    R: AuthResolver,
{
    /// Creates an upstream credential broker from an auth resolver.
    #[must_use]
    pub const fn new(resolver: R) -> Self {
        Self { resolver }
    }

    /// Resolves a provider credential and wraps it in a redacted holder.
    ///
    /// # Errors
    /// Returns an auth error when the configured upstream credential is unavailable.
    pub async fn resolve(&self, provider_id: &ProviderId) -> ProxyResult<RedactedSecret> {
        let secret: SecretString = self.resolver.resolve(provider_id).await?;
        Ok(RedactedSecret::new(secret))
    }
}

/// Environment policy for child harness processes launched next to the proxy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildEnvironment {
    vars: HashMap<String, String>,
}

impl ChildEnvironment {
    /// Returns an environment containing no inherited upstream provider credentials.
    #[must_use]
    pub fn provider_proxy_child() -> Self {
        Self::default()
    }

    /// Applies the sanitized environment to a child command.
    pub fn apply_to_command(&self, command: &mut Command) {
        command.env_clear();
        for (key, value) in &self.vars {
            command.env(key, value);
        }
    }

    /// Returns whether a variable would be visible to the child process.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.vars.contains_key(key)
    }
}

//! GitHub App service-identity authentication (spec `10-orchestrator.md`
//! §9.25.2; runbook `.omo/drafts/github-app-setup.md`).
//!
//! The daemon authenticates to GitHub as the `arbsec-agent` App by signing a
//! short-lived RS256 JWT with the App's private key and exchanging it for a
//! per-installation access token (~1h expiry). The private key resolves
//! exclusively through its configured `secret://` URI; resolution failure is
//! fail-closed — there is no ambient-credential fallback (no `GITHUB_TOKEN`
//! env sniffing, no owner-account auth). Errors, logs, and `Debug` output
//! never contain the PEM, a JWT, or a token (spec
//! `40-arbitraitor-integration.md` §9.23.4).

use std::fmt::{self, Write as _};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{GitHubAppError, SecretResolveError};
use crate::secret::{DEFAULT_KEYRING_SERVICE, SecretUri};

/// Injectable App-PEM resolution hook, defaulting to [`SecretUri::resolve`].
/// The daemon can later swap in an Arbitraitor-brokered release without
/// changing the minting policy.
pub type PemResolver =
    dyn Fn(&SecretUri, &str) -> Result<SecretString, SecretResolveError> + Send + Sync;

/// JWT lifetime: `exp = iat + 600` exactly, per the runbook-verified contract.
pub const JWT_LIFETIME_SECS: u64 = 600;

/// Cached installation tokens are re-minted when fewer than this many seconds
/// remain before expiry ("refresh at expiry − 5 minutes"). Chosen over the
/// 90%-of-lifetime option because a fixed skew is independent of the issuer's
/// lifetime grants; documented decision for issue #307.
pub const TOKEN_REFRESH_SKEW_SECS: u64 = 300;

/// Accepted minted-token lifetime above the request time. GitHub grants 1h;
/// one minute of leeway absorbs issuer clock skew without weakening the cap.
const MAX_TOKEN_LIFETIME_SECS: u64 = 3660;

#[derive(Serialize)]
struct AppJwtClaims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
}

/// Signs an App JWT (`{ iat: now, exp: now + 10min, iss: client_id }`, RS256).
///
/// The `iss` claim is the App **client ID** — the numeric app ID form is
/// rejected by GitHub with 401 (runbook §5, verified 2026-09-26). The returned
/// JWT is secret material and is wrapped in a [`SecretString`].
///
/// # Errors
///
/// Returns [`GitHubAppError::InvalidPrivateKeyPem`] when the PEM cannot be
/// parsed and [`GitHubAppError::JwtSigning`] when signing fails.
pub fn mint_app_jwt(
    now_epoch: u64,
    client_id: &str,
    private_key_pem: &SecretString,
) -> Result<SecretString, GitHubAppError> {
    let key = EncodingKey::from_rsa_pem(private_key_pem.expose_secret().as_bytes())
        .map_err(|_| GitHubAppError::InvalidPrivateKeyPem)?;
    let claims = AppJwtClaims {
        iss: client_id,
        iat: now_epoch,
        exp: now_epoch.saturating_add(JWT_LIFETIME_SECS),
    };
    let jwt = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|_| GitHubAppError::JwtSigning)?;
    Ok(SecretString::from(jwt))
}

/// Deserialized payload of `POST /app/installations/{id}/access_tokens`.
///
/// Carries the minted token, so no `Debug` derive exists: the value must be
/// moved into [`InstallationToken`] before it can be printed anywhere.
#[derive(Deserialize)]
pub struct AccessTokenResponse {
    /// Installation access token (secret).
    pub token: String,
    /// RFC 3339 expiry timestamp (GitHub grants 1h).
    pub expires_at: String,
}

impl AccessTokenResponse {
    /// Validates the mint contract (future expiry, lifetime within the 1h cap)
    /// and wraps the token value in secret-safe storage.
    ///
    /// # Errors
    ///
    /// Returns [`GitHubAppError::ExpiryUnparseable`],
    /// [`GitHubAppError::ExpiryExceedsMaximum`], or
    /// [`GitHubAppError::MalformedResponse`] when the response violates the
    /// minting contract; never carries response content.
    pub fn into_validated(self, now_epoch: u64) -> Result<InstallationToken, GitHubAppError> {
        let expiry = OffsetDateTime::parse(&self.expires_at, &Rfc3339)
            .map_err(|_| GitHubAppError::ExpiryUnparseable)?;
        let expires_at_epoch = u64::try_from(expiry.unix_timestamp()).map_err(|_| {
            GitHubAppError::MalformedResponse {
                detail: "expiry predates the unix epoch",
            }
        })?;
        if expires_at_epoch <= now_epoch {
            return Err(GitHubAppError::MalformedResponse {
                detail: "expiry is not in the future",
            });
        }
        if expires_at_epoch > now_epoch.saturating_add(MAX_TOKEN_LIFETIME_SECS) {
            return Err(GitHubAppError::ExpiryExceedsMaximum);
        }
        Ok(InstallationToken {
            token: SecretString::from(self.token),
            expires_at_epoch,
            expires_at_rfc3339: self.expires_at,
        })
    }
}

/// A minted installation access token with its expiry.
///
/// `Clone` is derived so the cache can hand out shared handles; the token
/// value stays a [`SecretString`] (zeroize-on-drop, no `Serialize`), and the
/// custom `Debug` impl renders only non-secret metadata.
#[derive(Clone)]
pub struct InstallationToken {
    token: SecretString,
    expires_at_epoch: u64,
    expires_at_rfc3339: String,
}

impl InstallationToken {
    /// Borrowed access to the token value for transport header injection.
    #[must_use]
    pub const fn token(&self) -> &SecretString {
        &self.token
    }

    /// Expiry as seconds since the unix epoch.
    #[must_use]
    pub const fn expires_at_epoch(&self) -> u64 {
        self.expires_at_epoch
    }

    /// Expiry as the issuer-supplied RFC 3339 string.
    #[must_use]
    pub fn expires_at_rfc3339(&self) -> &str {
        &self.expires_at_rfc3339
    }

    /// Whether the token still covers a call at `now_epoch` given the
    /// re-mint skew (`refresh at expiry − skew`).
    #[must_use]
    pub fn is_usable_at(&self, now_epoch: u64, refresh_skew_secs: u64) -> bool {
        now_epoch.saturating_add(refresh_skew_secs) < self.expires_at_epoch
    }

    /// Lowercase hex SHA-256 fingerprint prefix for audit lines; reveals
    /// nothing about the token value beyond brute-force resistance of the
    /// full fingerprint.
    #[must_use]
    pub fn fingerprint_prefix(&self, hex_len: usize) -> String {
        let digest = Sha256::digest(self.token.expose_secret().as_bytes());
        let full = digest
            .iter()
            .fold(String::with_capacity(digest.len() * 2), |mut acc, byte| {
                let _infallible = write!(acc, "{byte:02x}");
                acc
            });
        full.get(..hex_len.min(full.len()))
            .map_or_else(|| full.clone(), ToOwned::to_owned)
    }
}

impl fmt::Debug for InstallationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationToken")
            .field("token", &"REDACTED")
            .field("expires_at_epoch", &self.expires_at_epoch)
            .field("expires_at_rfc3339", &self.expires_at_rfc3339)
            .finish()
    }
}

/// HTTP transport for the token mint call. Implementations live in adapter
/// crates (the CLI's reqwest-blocking transport); this stays I/O-free so the
/// minting policy is unit-testable with counting fakes.
pub trait InstallationTokenTransport: Send + Sync {
    /// POSTs the App JWT to
    /// `<github>/app/installations/{installation_id}/access_tokens` and parses
    /// the success payload.
    ///
    /// # Errors
    ///
    /// Returns [`GitHubAppError::MintRequest`] for non-2xx statuses and
    /// [`GitHubAppError::Transport`] for transport-level failures. Neither
    /// variant carries request or response bodies.
    fn create_access_token(
        &self,
        installation_id: u64,
        app_jwt: &SecretString,
    ) -> Result<AccessTokenResponse, GitHubAppError>;
}

enum CacheState {
    Empty,
    Minting,
    Ready(InstallationToken),
}

/// Caching installation-token minter for the GitHub App service identity.
///
/// Tokens are cached until `expiry − TOKEN_REFRESH_SKEW_SECS` and re-minted
/// on demand. A mutex+condvar single-flight guards minting: a concurrent
/// caller waits for the in-flight mint instead of issuing a duplicate
/// request. The private key is re-read from its `secret://` store on each
/// mint so rotation takes effect without a daemon restart.
pub struct GitHubAppAuth {
    client_id: String,
    installation_id: u64,
    private_key_uri: SecretUri,
    keyring_service: String,
    pem_resolver: Arc<PemResolver>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    cache: Mutex<CacheState>,
    minted: Condvar,
}

impl GitHubAppAuth {
    /// Creates a minter from the resolved `github_app.*` configuration.
    #[must_use]
    pub fn new(client_id: String, installation_id: u64, private_key_uri: SecretUri) -> Self {
        Self {
            client_id,
            installation_id,
            private_key_uri,
            keyring_service: DEFAULT_KEYRING_SERVICE.to_string(),
            pem_resolver: Arc::new(SecretUri::resolve),
            clock: Arc::new(system_now_epoch),
            cache: Mutex::new(CacheState::Empty),
            minted: Condvar::new(),
        }
    }

    /// Overrides the App-PEM resolver (test fixtures and brokered secret
    /// release integrate here; production default is [`SecretUri::resolve`]).
    #[must_use]
    pub fn with_pem_resolver(mut self, resolver: Arc<PemResolver>) -> Self {
        self.pem_resolver = resolver;
        self
    }

    /// Overrides the keyring service label (tests and non-default setups).
    #[must_use]
    pub fn with_keyring_service(mut self, service: String) -> Self {
        self.keyring_service = service;
        self
    }

    /// Overrides the wall clock (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Returns a usable installation token, minting or re-minting as needed.
    ///
    /// # Errors
    ///
    /// Returns [`GitHubAppError`] on secret-resolution, signing, transport, or
    /// response-contract failures. Resolution failure is fail-closed: no
    /// ambient credential is ever consulted.
    pub fn installation_token(
        &self,
        transport: &dyn InstallationTokenTransport,
    ) -> Result<InstallationToken, GitHubAppError> {
        let mut state = self
            .cache
            .lock()
            .map_err(|_| GitHubAppError::CachePoisoned)?;
        loop {
            match &*state {
                CacheState::Ready(token)
                    if token.is_usable_at((self.clock)(), TOKEN_REFRESH_SKEW_SECS) =>
                {
                    return Ok(token.clone());
                }
                CacheState::Minting => {
                    state = self
                        .minted
                        .wait(state)
                        .map_err(|_| GitHubAppError::CachePoisoned)?;
                }
                CacheState::Empty | CacheState::Ready(_) => {
                    *state = CacheState::Minting;
                    let mint_outcome = {
                        drop(state);
                        let outcome = self.mint_once(transport);
                        state = self
                            .cache
                            .lock()
                            .map_err(|_| GitHubAppError::CachePoisoned)?;
                        outcome
                    };
                    let returned = match mint_outcome {
                        Ok(token) => {
                            let returned = token.clone();
                            *state = CacheState::Ready(token);
                            Ok(returned)
                        }
                        Err(error) => {
                            *state = CacheState::Empty;
                            Err(error)
                        }
                    };
                    self.minted.notify_all();
                    return returned;
                }
            }
        }
    }

    fn mint_once(
        &self,
        transport: &dyn InstallationTokenTransport,
    ) -> Result<InstallationToken, GitHubAppError> {
        let pem = (self.pem_resolver)(&self.private_key_uri, &self.keyring_service).map_err(
            |source| GitHubAppError::PrivateKeyResolution {
                uri: self.private_key_uri.as_uri(),
                source: Box::new(source),
            },
        )?;
        let jwt = mint_app_jwt((self.clock)(), &self.client_id, &pem)?;
        drop(pem);
        let response = transport.create_access_token(self.installation_id, &jwt)?;
        response.into_validated((self.clock)())
    }
}

impl fmt::Debug for GitHubAppAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubAppAuth")
            .field("client_id", &self.client_id)
            .field("installation_id", &self.installation_id)
            .field("private_key_uri", &self.private_key_uri.as_uri())
            .field("keyring_service", &self.keyring_service)
            .finish_non_exhaustive()
    }
}

fn system_now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests;

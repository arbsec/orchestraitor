use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use jsonwebtoken::{DecodingKey, Validation};
use serde::Deserialize;

use super::*;
use crate::error::SecretResolveError;

/// Throwaway RSA-2048 pair (crate `tests/fixtures/`, generated for these tests
/// only; registered with nothing).
const TEST_PRIVATE_PEM: &str = include_str!("../../tests/fixtures/github-app-rsa-test.pem");
const TEST_PUBLIC_PEM: &str = include_str!("../../tests/fixtures/github-app-rsa-test.pub.pem");
const TEST_CLIENT_ID: &str = "Iv1.testclient";
const TEST_INSTALLATION_ID: u64 = 424_242;
const FIXTURE_TOKEN_MARKER: &str = "ghs_fixtureTOKENmarker0000";
const FIXTURE_PEM_MARKER: &str = "MIIFIXTUREPEMMARKER";
/// `1970-01-01T01:16:40Z` == epoch 4600, exactly 1h past the test start (1000)
/// and inside the minted-token lifetime cap.
const EXPIRY_OK: &str = "1970-01-01T01:16:40Z";
/// `1970-01-01T02:11:40Z` == epoch 7900: exactly 1h past the advanced clock
/// (4300) used by the re-mint boundary test.
const EXPIRY_AFTER_REMINT: &str = "1970-01-01T02:11:40Z";
/// Guaranteed-unset variable for fail-closed assertions.
const UNSET_ENV_VAR: &str = "ORCHESTRAITOR_TEST_GITHUB_APP_PEM_DEFINITELY_UNSET";

#[derive(Debug, Deserialize)]
struct DecodedClaims {
    iss: String,
    iat: u64,
    exp: u64,
}

struct ScriptedTransport {
    calls: AtomicUsize,
    delay: Duration,
    token: String,
    expires_at: StdMutex<String>,
}

impl ScriptedTransport {
    fn new(token: &str, expires_at: &str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            token: token.to_string(),
            expires_at: StdMutex::new(expires_at.to_string()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn set_expires_at(&self, value: &str) {
        if let Ok(mut slot) = self.expires_at.lock() {
            *slot = value.to_string();
        }
    }
}

impl InstallationTokenTransport for ScriptedTransport {
    fn create_access_token(
        &self,
        installation_id: u64,
        _app_jwt: &SecretString,
    ) -> Result<AccessTokenResponse, GitHubAppError> {
        assert_eq!(installation_id, TEST_INSTALLATION_ID);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            thread::sleep(self.delay);
        }
        let expires_at = self
            .expires_at
            .lock()
            .map_err(|_| GitHubAppError::CachePoisoned)?;
        Ok(AccessTokenResponse {
            token: self.token.clone(),
            expires_at: expires_at.clone(),
        })
    }
}

fn unexpected(detail: &'static str) -> GitHubAppError {
    GitHubAppError::MalformedResponse { detail }
}

fn clock_for(now: &Arc<StdMutex<u64>>) -> Arc<dyn Fn() -> u64 + Send + Sync> {
    let now = Arc::clone(now);
    Arc::new(move || now.lock().map_or(u64::MAX / 2, |guard| *guard))
}

fn set_clock(clock: &Arc<StdMutex<u64>>, value: u64) {
    if let Ok(mut guard) = clock.lock() {
        *guard = value;
    }
}

fn fixture_pem_resolver() -> Arc<PemResolver> {
    Arc::new(|_uri, _service| Ok(SecretString::from(TEST_PRIVATE_PEM.to_string())))
}

fn test_private_key_uri() -> Result<SecretUri, GitHubAppError> {
    "secret://env/ORCHESTRAITOR_TEST_GITHUB_APP_PEM_FIXTURE"
        .parse()
        .map_err(|_| unexpected("test secret URI must parse"))
}

fn auth_with_fixture_pem(now: &Arc<StdMutex<u64>>) -> Result<GitHubAppAuth, GitHubAppError> {
    Ok(
        GitHubAppAuth::new(TEST_CLIENT_ID.to_string(), TEST_INSTALLATION_ID, {
            test_private_key_uri()?
        })
        .with_clock(clock_for(now))
        .with_pem_resolver(fixture_pem_resolver()),
    )
}

fn auth_with_default_resolver(
    now: &Arc<StdMutex<u64>>,
    env_var: &str,
) -> Result<GitHubAppAuth, GitHubAppError> {
    let uri = format!("secret://env/{env_var}")
        .parse()
        .map_err(|_| unexpected("test secret URI must parse"))?;
    Ok(
        GitHubAppAuth::new(TEST_CLIENT_ID.to_string(), TEST_INSTALLATION_ID, uri)
            .with_clock(clock_for(now)),
    )
}

#[test]
fn jwt_claims_use_client_id_and_exact_ten_minute_expiry() -> Result<(), GitHubAppError> {
    let pem = SecretString::from(TEST_PRIVATE_PEM.to_string());
    let jwt = mint_app_jwt(1_000, TEST_CLIENT_ID, &pem)?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = false;
    let data = jsonwebtoken::decode::<DecodedClaims>(
        jwt.expose_secret(),
        &DecodingKey::from_rsa_pem(TEST_PUBLIC_PEM.as_bytes())
            .map_err(|_| GitHubAppError::InvalidPrivateKeyPem)?,
        &validation,
    )
    .map_err(|_| GitHubAppError::JwtSigning)?;

    assert_eq!(data.header.alg, Algorithm::RS256);
    assert_eq!(data.claims.iss, TEST_CLIENT_ID);
    assert_ne!(
        data.claims.iss, "5082653",
        "iss must be the client ID, never the numeric app id (401, runbook §5)"
    );
    assert_eq!(data.claims.iat, 1_000);
    assert_eq!(
        data.claims.exp,
        1_000 + JWT_LIFETIME_SECS,
        "exp must equal iat + 600 exactly"
    );
    Ok(())
}

#[test]
fn absent_env_private_key_fails_closed_with_zero_transport_calls() -> Result<(), GitHubAppError> {
    let now = Arc::new(StdMutex::new(1_000u64));
    let auth = auth_with_default_resolver(&now, UNSET_ENV_VAR)?;
    let transport = ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK);

    let result = auth.installation_token(&transport);

    match result {
        Err(GitHubAppError::PrivateKeyResolution { uri, source }) => {
            assert_eq!(uri, format!("secret://env/{UNSET_ENV_VAR}"));
            assert!(matches!(*source, SecretResolveError::EnvMissing { .. }));
        }
        Err(other) => return Err(unexpected_from_debug(&other)),
        Ok(_) => return Err(unexpected("unresolvable key must never mint a token")),
    }
    assert_eq!(
        transport.call_count(),
        0,
        "fail-closed requires zero GitHub requests when the key cannot be resolved"
    );
    Ok(())
}

#[test]
fn absent_keyring_private_key_fails_closed_with_zero_transport_calls() -> Result<(), GitHubAppError>
{
    let now = Arc::new(StdMutex::new(1_000u64));
    let uri: SecretUri = "secret://keyring/orchestraitor-test-nonexistent-entry"
        .parse()
        .map_err(|_| unexpected("keyring URI must parse"))?;
    let auth = GitHubAppAuth::new(TEST_CLIENT_ID.to_string(), TEST_INSTALLATION_ID, uri)
        .with_clock(clock_for(&now));
    let transport = ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK);

    let result = auth.installation_token(&transport);

    match result {
        Err(GitHubAppError::PrivateKeyResolution { .. }) => {}
        Err(other) => return Err(unexpected_from_debug(&other)),
        Ok(_) => return Err(unexpected("keyring-missing key must never mint a token")),
    }
    assert_eq!(transport.call_count(), 0);
    Ok(())
}

#[test]
fn fresh_token_is_served_from_cache_without_reminting() -> Result<(), GitHubAppError> {
    let now = Arc::new(StdMutex::new(1_000u64));
    let auth = auth_with_fixture_pem(&now)?;
    let transport = ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK);

    let first = auth.installation_token(&transport)?;
    assert_eq!(transport.call_count(), 1);
    assert_eq!(first.expires_at_epoch(), 4_600);
    assert_eq!(first.expires_at_rfc3339(), EXPIRY_OK);

    let second = auth.installation_token(&transport)?;
    assert_eq!(
        transport.call_count(),
        1,
        "a fresh cached token must not trigger a re-mint"
    );
    assert_eq!(second.fingerprint_prefix(12), first.fingerprint_prefix(12));
    assert!(second.is_usable_at(1_000, TOKEN_REFRESH_SKEW_SECS));
    Ok(())
}

#[test]
fn remint_happens_exactly_at_expiry_minus_skew_boundary() -> Result<(), GitHubAppError> {
    let now = Arc::new(StdMutex::new(1_000u64));
    let auth = auth_with_fixture_pem(&now)?;
    let transport = ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK);

    let _ = auth.installation_token(&transport)?;
    assert_eq!(transport.call_count(), 1);

    // One second before the boundary: `now + skew < expiry` still holds.
    set_clock(&now, 4_600 - TOKEN_REFRESH_SKEW_SECS - 1);
    let _ = auth.installation_token(&transport)?;
    assert_eq!(
        transport.call_count(),
        1,
        "t = expiry - skew - 1 must use the cached token"
    );

    // At the boundary: `now + skew == expiry` forces a re-mint; the second
    // mint grants a token expiring 1h past the advanced clock.
    set_clock(&now, 4_600 - TOKEN_REFRESH_SKEW_SECS);
    transport.set_expires_at(EXPIRY_AFTER_REMINT);
    let refreshed = auth.installation_token(&transport)?;
    assert_eq!(
        transport.call_count(),
        2,
        "t = expiry - skew must re-mint exactly"
    );
    assert_eq!(refreshed.expires_at_rfc3339(), EXPIRY_AFTER_REMINT);
    Ok(())
}

#[test]
fn concurrent_mints_single_flight_into_one_transport_call() -> Result<(), GitHubAppError> {
    let now = Arc::new(StdMutex::new(1_000u64));
    let auth = Arc::new(auth_with_fixture_pem(&now)?);
    let transport = Arc::new(ScriptedTransport {
        delay: Duration::from_millis(200),
        ..ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK)
    });
    let barrier = Arc::new(Barrier::new(3));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let auth = Arc::clone(&auth);
        let transport = Arc::clone(&transport);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            auth.installation_token(transport.as_ref())
        }));
    }
    barrier.wait();

    let mut fingerprints = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(Ok(token)) => fingerprints.push(token.fingerprint_prefix(12)),
            Ok(Err(error)) => return Err(error),
            Err(_panic) => return Err(unexpected("worker thread panicked")),
        }
    }

    assert_eq!(
        transport.call_count(),
        1,
        "concurrent mints must single-flight into exactly one GitHub request"
    );
    assert_eq!(fingerprints.len(), 2);
    assert_eq!(fingerprints[0], fingerprints[1]);
    Ok(())
}

struct PanickingTransport {
    arm_panic: bool,
}

impl InstallationTokenTransport for PanickingTransport {
    fn create_access_token(
        &self,
        _installation_id: u64,
        _app_jwt: &SecretString,
    ) -> Result<AccessTokenResponse, GitHubAppError> {
        assert!(!self.arm_panic, "forced transport panic");
        Err(GitHubAppError::MalformedResponse {
            detail: "panic path unexpectedly returned",
        })
    }
}

#[test]
fn panicking_mint_does_not_wedge_subsequent_mints() -> Result<(), GitHubAppError> {
    // A panic mid-mint runs without the cache lock held, so without the
    // unwind guard the slot would stay in `Minting` and this test's second
    // mint would block on the condvar forever (test-runner timeout = failure).
    let now = Arc::new(StdMutex::new(1_000u64));
    let auth = Arc::new(auth_with_fixture_pem(&now)?);

    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _ = auth.installation_token(&PanickingTransport { arm_panic: true });
    }));
    assert!(panicked.is_err(), "the forced transport panic must unwind");

    let transport = ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK);
    let token = auth.installation_token(&transport)?;
    assert_eq!(
        transport.call_count(),
        1,
        "the next mint must issue exactly one fresh transport call"
    );
    assert_eq!(token.expires_at_rfc3339(), EXPIRY_OK);
    Ok(())
}

#[test]
fn mint_response_contract_cap_past_and_shape_are_enforced() -> Result<(), GitHubAppError> {
    let within_cap = AccessTokenResponse {
        token: FIXTURE_TOKEN_MARKER.to_string(),
        expires_at: EXPIRY_OK.to_string(),
    }
    .into_validated(1_000)?;
    assert_eq!(within_cap.expires_at_epoch(), 4_600);

    let over_cap = AccessTokenResponse {
        token: FIXTURE_TOKEN_MARKER.to_string(),
        expires_at: "1970-01-01T02:16:41Z".to_string(),
    }
    .into_validated(1_000);
    assert!(matches!(
        over_cap,
        Err(GitHubAppError::ExpiryExceedsMaximum)
    ));

    let past = AccessTokenResponse {
        token: FIXTURE_TOKEN_MARKER.to_string(),
        expires_at: "1969-12-31T23:00:00Z".to_string(),
    }
    .into_validated(1_000);
    assert!(matches!(
        past,
        Err(GitHubAppError::MalformedResponse { .. })
    ));

    let garbage = AccessTokenResponse {
        token: FIXTURE_TOKEN_MARKER.to_string(),
        expires_at: "not-a-timestamp".to_string(),
    }
    .into_validated(1_000);
    assert!(matches!(garbage, Err(GitHubAppError::ExpiryUnparseable)));
    Ok(())
}

#[test]
fn secret_material_never_renders_in_errors_or_debug() -> Result<(), GitHubAppError> {
    let token = AccessTokenResponse {
        token: FIXTURE_TOKEN_MARKER.to_string(),
        expires_at: EXPIRY_OK.to_string(),
    }
    .into_validated(1_000)?;

    let rendered = format!("{token:?}");
    assert!(!rendered.contains(FIXTURE_TOKEN_MARKER));
    assert!(rendered.contains("REDACTED"));

    let bad_pem = SecretString::from(format!(
        "{FIXTURE_PEM_MARKER}\n-----END RSA PRIVATE KEY-----"
    ));
    let pem_error = match mint_app_jwt(1_000, TEST_CLIENT_ID, &bad_pem) {
        Err(error) => format!("{error:?} {error}"),
        Ok(_) => return Err(unexpected("garbage PEM must never sign")),
    };
    assert!(!pem_error.contains(FIXTURE_PEM_MARKER));

    let now = Arc::new(StdMutex::new(1_000u64));
    let auth = auth_with_default_resolver(&now, UNSET_ENV_VAR)?;
    let transport = ScriptedTransport::new(FIXTURE_TOKEN_MARKER, EXPIRY_OK);
    let resolution_error = match auth.installation_token(&transport) {
        Err(error) => format!("{error:?} {error}"),
        Ok(_) => return Err(unexpected("unset key must never resolve")),
    };
    assert!(!resolution_error.contains(FIXTURE_TOKEN_MARKER));
    assert!(!resolution_error.contains("PRIVATE KEY"));
    assert!(!resolution_error.contains(TEST_PRIVATE_PEM.lines().nth(1).unwrap_or("n/a")));
    Ok(())
}

#[test]
fn fingerprint_prefix_is_stable_and_bounded() -> Result<(), GitHubAppError> {
    let token = AccessTokenResponse {
        token: FIXTURE_TOKEN_MARKER.to_string(),
        expires_at: EXPIRY_OK.to_string(),
    }
    .into_validated(1_000)?;

    let prefix = token.fingerprint_prefix(12);
    assert_eq!(prefix.len(), 12);
    assert!(prefix.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(token.fingerprint_prefix(128).len(), 64);
    Ok(())
}

fn unexpected_from_debug(_error: &GitHubAppError) -> GitHubAppError {
    unexpected("unexpected GitHubAppError variant")
}

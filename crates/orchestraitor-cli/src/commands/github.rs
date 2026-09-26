//! `orc github` implementation.

use std::io::Write;
use std::time::Duration;

use miette::{IntoDiagnostic, Result, miette};
use orchestraitor_core::GitHubAppError;
use orchestraitor_core::github_app::{
    AccessTokenResponse, GitHubAppAuth, InstallationTokenTransport,
};
use secrecy::{ExposeSecret, SecretString};

use crate::cli::{ConfigPaths, GitHubCommand};
use crate::commands::config::layers::load_layers;

/// GitHub REST API base URL.
const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// Runs an `orc github` subcommand.
///
/// # Errors
/// Returns a diagnostic when configuration, secret resolution, or the mint
/// request fails. No diagnostic ever contains key material or tokens.
pub fn run<W: Write>(paths: &ConfigPaths, command: GitHubCommand, writer: &mut W) -> Result<()> {
    match command {
        GitHubCommand::MintToken => mint_token(paths, writer),
    }
}

fn mint_token<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    let layers = load_layers(paths)?;
    let config = layers.resolver.resolve_config().map_err(|error| {
        miette!(
            "configuration validation failed: {}",
            error.structured().cause
        )
    })?;
    let Some(github_app) = config.github_app else {
        return Err(miette!(
            "github_app configuration is not set (need client_id, installation_id, private_key_uri; see docs/cli/orc-github.md)"
        ));
    };
    let client_id = required(github_app.client_id.as_ref(), "client_id")?.clone();
    let installation_id = *required(github_app.installation_id.as_ref(), "installation_id")?;
    let private_key_uri = required(github_app.private_key_uri.as_ref(), "private_key_uri")?.clone();

    let auth = GitHubAppAuth::new(client_id, installation_id, private_key_uri);
    let transport =
        ReqwestInstallationTransport::new(paths.github_api_endpoint.clone()).into_diagnostic()?;
    let token = auth.installation_token(&transport).into_diagnostic()?;

    writeln!(writer, "minted GitHub App installation token").into_diagnostic()?;
    writeln!(writer, "installation_id = {installation_id}").into_diagnostic()?;
    writeln!(writer, "expires_at = {}", token.expires_at_rfc3339()).into_diagnostic()?;
    writeln!(writer, "expires_at_epoch = {}", token.expires_at_epoch()).into_diagnostic()?;
    writeln!(
        writer,
        "token_sha256_prefix = {}",
        token.fingerprint_prefix(12)
    )
    .into_diagnostic()?;
    writeln!(
        writer,
        "token is held in memory only; it is never printed, logged, or persisted"
    )
    .into_diagnostic()
}

fn required<'a, T>(value: Option<&'a T>, key: &str) -> Result<&'a T> {
    value.ok_or_else(|| {
        miette!(
            "{} (set it at the user/org layer; see docs/cli/orc-github.md)",
            GitHubAppError::MissingConfig {
                key: key.to_string()
            }
        )
    })
}

struct ReqwestInstallationTransport {
    client: reqwest::blocking::Client,
    base_url: String,
}

impl ReqwestInstallationTransport {
    fn new(endpoint_override: Option<String>) -> Result<Self, GitHubAppError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("orchestraitor-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| GitHubAppError::Transport {
                kind: "client-build",
            })?;
        let base_url = endpoint_override
            .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self { client, base_url })
    }
}

impl InstallationTokenTransport for ReqwestInstallationTransport {
    fn create_access_token(
        &self,
        installation_id: u64,
        app_jwt: &SecretString,
    ) -> Result<AccessTokenResponse, GitHubAppError> {
        // The only place the JWT leaves memory: injected into the Authorization
        // header of this single request (spec 40-arbitraitor-integration.md §9.23.4).
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.base_url
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(app_jwt.expose_secret())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&serde_json::json!({}))
            .send()
            .map_err(|error| GitHubAppError::Transport {
                kind: classify_transport(&error),
            })?;
        let status = response.status();
        if !status.is_success() {
            // Response bodies are dropped deliberately: they are never echoed
            // into errors or logs.
            return Err(GitHubAppError::MintRequest {
                status: status.as_u16(),
            });
        }
        response
            .json::<AccessTokenResponse>()
            .map_err(|_| GitHubAppError::Transport { kind: "decode" })
    }
}

fn classify_transport(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() || error.is_decode() {
        "request"
    } else {
        "send"
    }
}

//! End-to-end command behavior for `orc config` and `orc models`.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use clap::Parser;
use miette::IntoDiagnostic;
use orchestraitor_cli::cli::Cli;

#[test]
fn config_get_returns_resolved_value() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[normalization]\nmax_passes = 7\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "get",
        "normalization.max_passes",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    assert_eq!(String::from_utf8(output).into_diagnostic()?, "7\n");
    Ok(())
}

#[test]
fn validate_rejects_ambiguous_conflicts() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    let org_shards = config_dir.join("org.d");
    fs::create_dir_all(&org_shards).into_diagnostic()?;
    fs::write(config_dir.join("org.toml"), "[retry]\nmax_attempts = 2\n").into_diagnostic()?;
    fs::write(
        org_shards.join("override.toml"),
        "[retry]\nmax_attempts = 3\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "config",
        "validate",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);
    assert!(result.is_err());
    let error = match result {
        Ok(()) => return Err(miette::miette!("ambiguous conflict passed validation")),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("ambiguous configuration conflict")
    );
    Ok(())
}

#[test]
fn migrate_preserves_comments() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config = temp.path().join("orchestraitor.toml");
    fs::write(&config, "# keep me\n[retry]\nmax_attempts = 2\n").into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "migrate",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let migrated = fs::read_to_string(config).into_diagnostic()?;

    assert!(migrated.contains("# keep me"));
    assert!(migrated.contains("schema_version = \"0.14\""));
    Ok(())
}

#[test]
fn models_refresh_fetches_live_catalog() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let endpoint = spawn_catalog_server()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &temp.path().display().to_string(),
        "--models-dev-endpoint",
        &endpoint,
        "models",
        "refresh",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    assert!(rendered.contains("refreshed models.dev catalog"));
    assert!(temp.path().join("models-dev").exists());
    Ok(())
}

fn spawn_catalog_server() -> miette::Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!(
        "http://{}/catalog.json",
        listener.local_addr().into_diagnostic()?
    );
    thread::spawn(move || {
        if let Ok((mut stream, _addr)) = listener.accept() {
            let mut request = [0_u8; 512];
            let _read_result = stream.read(&mut request);
            let body =
                br#"{"providers":{"neuralwatt":{"env":["NEURALWATT_API_KEY"]}},"models":{}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    Ok(endpoint)
}

const GITHUB_APP_FIXTURE_PEM: &str =
    include_str!("../../orchestraitor-core/tests/fixtures/github-app-rsa-test.pem");
const GITHUB_APP_TOKEN_MARKER: &str = "ghs_cliE2eTOKENmarker";
const GITHUB_APP_PEM_ENV_VAR: &str = "ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM";

fn write_github_app_project_config(temp: &tempfile::TempDir) -> miette::Result<()> {
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nclient_id = \"Iv1.cli-e2e\"\ninstallation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()
}

#[test]
fn github_mint_token_fails_closed_when_private_key_unresolvable() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // Config points at an env var that is never set; no ambient fallback exists.
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nclient_id = \"Iv1.cli-e2e\"\ninstallation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_NEVER_SET\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "--config-dir",
        &temp.path().display().to_string(),
        "github",
        "mint-token",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);

    let error = match result {
        Ok(()) => return Err(miette::miette!("unresolvable key must never mint")),
        Err(error) => error,
    };
    // miette's fancy renderer wraps lines; compare against a
    // whitespace-collapsed rendering.
    let flat: String = format!("{error:?}")
        .chars()
        .filter(|c| c.is_ascii() && !c.is_whitespace())
        .collect();
    assert!(flat.contains("secret://env/ORCHESTRAITOR_CLI_TEST_NEVER_SET"));
    assert!(flat.contains("privatekeyresolutionfailed"));
    assert!(
        !flat.contains("GITHUB_TOKEN"),
        "no ambient-credential hint may appear"
    );
    Ok(())
}

#[test]
fn github_mint_token_requires_config_keys() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    fs::write(temp.path().join("orchestraitor.toml"), "[github_app]\n").into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "--config-dir",
        &temp.path().display().to_string(),
        "github",
        "mint-token",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);

    let error = match result {
        Ok(()) => return Err(miette::miette!("missing client_id must fail validation")),
        Err(error) => error,
    };
    assert!(error.to_string().contains("github_app.client_id"));
    Ok(())
}

#[test]
fn github_mint_token_happy_path_prints_only_metadata() -> miette::Result<()> {
    use std::process::{Command, Stdio};

    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    let expires_at_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .into_diagnostic()?
        .as_secs()
        + 3_300;
    let expires_at =
        time::OffsetDateTime::from_unix_timestamp(expires_at_epoch.try_into().into_diagnostic()?)
            .into_diagnostic()?
            .format(&time::format_description::well_known::Rfc3339)
            .into_diagnostic()?;
    let (endpoint, observed) = spawn_access_token_server(GITHUB_APP_TOKEN_MARKER, &expires_at)?;

    let output = Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &endpoint,
            "github",
            "mint-token",
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    assert!(stdout.contains("minted GitHub App installation token"));
    assert!(stdout.contains("installation_id = 165043398"));
    assert!(stdout.contains(&format!("expires_at_epoch = {expires_at_epoch}")));
    assert!(stdout.contains("token_sha256_prefix = "));
    assert!(
        !stdout.contains(GITHUB_APP_TOKEN_MARKER),
        "the token value must never be printed"
    );
    let request = observed
        .join()
        .map_err(|_| miette::miette!("mock server panicked"))?;
    assert!(request.starts_with("POST /app/installations/165043398/access_tokens"));
    let auth_header = request
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
        .map(str::to_owned)
        .ok_or_else(|| miette::miette!("request must carry an Authorization header"))?;
    assert!(
        auth_header.contains("Bearer eyJ"),
        "JWT must be bearer-injected"
    );
    Ok(())
}

fn spawn_access_token_server(
    token: &str,
    expires_at: &str,
) -> miette::Result<(String, thread::JoinHandle<String>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!("http://{}", listener.local_addr().into_diagnostic()?);
    let body = format!(r#"{{"token":"{token}","expires_at":"{expires_at}"}}"#);
    let observed = thread::spawn(move || {
        let (mut stream, _addr) = match listener.accept() {
            Ok(pair) => pair,
            Err(_error) => return String::new(),
        };
        let mut request_bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    request_bytes.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&request_bytes);
                    if let Some(match_headers_end) = text.find("\r\n\r\n") {
                        let content_length = text[..match_headers_end]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request_bytes.len() >= match_headers_end + 4 + content_length {
                            break;
                        }
                    }
                }
            }
        }
        let response = format!(
            "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _write_result = stream.write_all(response.as_bytes());
        String::from_utf8_lossy(&request_bytes).to_string()
    });
    Ok((endpoint, observed))
}

const BOARD_RESOLVE_PROJECT: &str = r#"{"data":{"organization":{"projectV2":{"id":"PVT_fixture_project","title":"Arbsec Development"}}}}"#;
const BOARD_RESOLVE_FIELDS: &str = r#"{"data":{"node":{"fields":{"nodes":[{"id":"PVTSSF_fixture_status","name":"Status","options":[{"id":"OPT_fixture_ready","name":"Ready"},{"id":"OPT_fixture_in_progress","name":"In Progress"}]},{"id":"PVTSSF_fixture_target","name":"Target","options":[{"id":"OPT_fixture_mvp","name":"MVP"}]}]}}}}"#;
const BOARD_ITEMS: &str = r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"PVTI_F_130","content":{"__typename":"Issue","number":130,"state":"OPEN","title":"Eligible native task","url":"https://github.com/arbsec/orchestraitor/issues/130","repository":{"nameWithOwner":"arbsec/orchestraitor"},"issueType":{"name":"Task"},"labels":{"nodes":[],"totalCount":0},"blockedBy":{"nodes":[],"totalCount":0}},"fieldValues":{"totalCount":2,"nodes":[{"name":"MVP","field":{"name":"Target"}},{"name":"Ready","field":{"name":"Status"}}]}},{"id":"PVTI_F_139","content":null,"fieldValues":{"totalCount":0,"nodes":[]}}]}}}}"#;

#[test]
fn board_ready_emits_json_ready_queue() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let project_dir = temp.path().join("project");
    let agents_dir = project_dir.join(".agents").join("project");
    fs::create_dir_all(&agents_dir).into_diagnostic()?;
    // The `secret://env/PATH` stub keeps the test free of credential material:
    // every test process has PATH, and the scripted server ignores the token.
    fs::write(
        agents_dir.join("github-project.local.toml"),
        "[project]\norganization = \"arbsec\"\nnumber = 1\nrepos = [\"arbsec/orchestraitor\"]\n\n[issue_types]\nleaf_implementable = [\"Task\", \"Bug\"]\n\n[mvp]\ntarget_field = \"Target\"\ntarget_value = \"MVP\"\nready_field = \"Status\"\nready_value = \"Ready\"\n\n[auth]\ntoken = \"secret://env/PATH\"\n",
    )
    .into_diagnostic()?;
    let endpoint = spawn_graphql_server(&[
        ("projectV2(number", BOARD_RESOLVE_PROJECT),
        ("fields(first", BOARD_RESOLVE_FIELDS),
        ("items(first", BOARD_ITEMS),
    ])?;
    let cache_path = temp.path().join("board-cache.json");
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &project_dir.display().to_string(),
        "--github-graphql-endpoint",
        &endpoint,
        "--board-cache-path",
        &cache_path.display().to_string(),
        "board",
        "ready",
        "--json",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let printed = String::from_utf8(output).into_diagnostic()?;
    let parsed: serde_json::Value = serde_json::from_str(&printed).into_diagnostic()?;
    let numbers: Vec<u64> = parsed
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("number").and_then(serde_json::Value::as_u64))
                .collect()
        })
        .unwrap_or_default();

    assert_eq!(numbers, [130]);
    assert!(printed.contains("\"item_id\": \"PVTI_F_130\""));
    assert!(!printed.contains("139"), "malformed item must be skipped");
    Ok(())
}

#[test]
fn board_ready_without_local_config_is_an_actionable_error() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "board",
        "ready",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);
    assert!(result.is_err());
    let error = match result {
        Ok(()) => return Err(miette::miette!("board ready succeeded without config")),
        Err(error) => error,
    };

    assert!(error.to_string().contains("github-project.local.toml"));
    Ok(())
}

/// Accepts sequential one-request connections (`connection: close`), matching
/// the request body against `(needle, payload)` rules in order.
fn spawn_graphql_server(rules: &'static [(&'static str, &'static str)]) -> miette::Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!(
        "http://{}/graphql",
        listener.local_addr().into_diagnostic()?
    );
    thread::spawn(move || {
        while let Ok((mut stream, _addr)) = listener.accept() {
            let mut buffer = vec![0_u8; 65_536].into_boxed_slice();
            let Ok(read) = stream.read(&mut buffer) else {
                continue;
            };
            let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let payload = rules
                .iter()
                .find(|(needle, _)| text.contains(needle))
                .map_or(
                    r#"{"data":null,"errors":[{"message":"unmatched request"}]}"#,
                    |(_, payload)| payload,
                );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    Ok(endpoint)
}

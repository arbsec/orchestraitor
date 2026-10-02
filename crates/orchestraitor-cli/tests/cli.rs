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

#[test]
fn config_get_resolves_builtin_role_routing_keys() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "get",
        "roles.implement.routing.provider",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    assert_eq!(String::from_utf8(output).into_diagnostic()?, "neuralwatt\n");
    Ok(())
}

#[test]
fn config_get_resolves_the_routing_table_for_builtin_and_custom_roles() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
         [roles.migrator.routing]\nprovider = \"acme\"\nmodel = \"acme-pro\"\n",
    )
    .into_diagnostic()?;

    for (key, expected) in [
        (
            "roles.review.routing",
            "{\"model\":\"glm-5.2\",\"provider\":\"neuralwatt\"}",
        ),
        (
            "roles.migrator.routing",
            "{\"model\":\"acme-pro\",\"provider\":\"acme\"}",
        ),
    ] {
        let mut output = Vec::new();
        let cli = Cli::parse_from([
            "orc",
            "--config-dir",
            &config_dir.display().to_string(),
            "--project-dir",
            &temp.path().display().to_string(),
            "config",
            "get",
            key,
        ]);
        orchestraitor_cli::run_with_writer(cli, &mut output)?;
        assert_eq!(
            String::from_utf8(output).into_diagnostic()?,
            format!("{expected}\n"),
            "config get {key}"
        );
    }
    Ok(())
}

#[test]
fn config_get_reports_provenance_for_a_custom_role_routing_table() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    // Mixed layers: the user layer sets the model, the project layer sets the
    // provider. The composed-table view attributes provenance to the
    // highest-precedence layer among the leaves (project).
    fs::write(
        config_dir.join("user.toml"),
        "[roles.migrator.routing]\nmodel = \"glm-5.2\"\n",
    )
    .into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[roles.migrator.routing]\nprovider = \"neuralwatt\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "explain",
        "roles.migrator.routing",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let rendered = String::from_utf8(output).into_diagnostic()?;

    assert!(rendered.contains("key = roles.migrator.routing"));
    assert!(rendered.contains("value = {\"model\":\"glm-5.2\",\"provider\":\"neuralwatt\"}"));
    assert!(rendered.contains("source_layer = project"));
    Ok(())
}

#[test]
fn config_get_composes_the_full_routing_table_at_role_depth() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
         [roles.migrator.routing]\nprovider = \"acme\"\nmodel = \"acme-pro\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "get",
        "roles.migrator",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;

    assert_eq!(
        String::from_utf8(output).into_diagnostic()?,
        "{\"routing\":{\"model\":\"acme-pro\",\"provider\":\"acme\"}}\n"
    );
    Ok(())
}

#[test]
fn config_get_composes_every_role_with_its_full_routing_nesting() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
         [roles.migrator.routing]\nprovider = \"acme\"\nmodel = \"acme-pro\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "get",
        "roles",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let composed: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    // The depth-2 `routing` nesting level must survive composition; built-in
    // defaults ship all six roles, the custom role joins them.
    let roles = composed
        .as_object()
        .ok_or_else(|| miette::miette!("composed `roles` is not an object"))?;
    assert_eq!(
        roles
            .get("migrator")
            .and_then(|r| r.get("routing"))
            .and_then(|r| r.get("provider")),
        Some(&serde_json::Value::String("acme".to_string()))
    );
    assert_eq!(
        roles
            .get("migrator")
            .and_then(|r| r.get("routing"))
            .and_then(|r| r.get("model")),
        Some(&serde_json::Value::String("acme-pro".to_string()))
    );
    for role in [
        "explore",
        "research",
        "plan",
        "implement",
        "review",
        "verify",
    ] {
        assert_eq!(
            roles
                .get(role)
                .and_then(|r| r.get("routing"))
                .and_then(|r| r.get("provider")),
            Some(&serde_json::Value::String("neuralwatt".to_string())),
            "built-in role {role} must nest under routing"
        );
        assert_eq!(
            roles
                .get(role)
                .and_then(|r| r.get("routing"))
                .and_then(|r| r.get("model")),
            Some(&serde_json::Value::String("glm-5.2".to_string())),
            "built-in role {role} must nest under routing"
        );
    }
    Ok(())
}

#[test]
fn config_get_and_explain_agree_on_an_empty_role_table() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    // An empty `roles.myrole` table registers no leaves: both `get` and
    // `explain` must report the key as not set.
    fs::write(temp.path().join("orchestraitor.toml"), "[roles.myrole]\n").into_diagnostic()?;

    for command in ["get", "explain"] {
        let mut output = Vec::new();
        let cli = Cli::parse_from([
            "orc",
            "--config-dir",
            &config_dir.display().to_string(),
            "--project-dir",
            &temp.path().display().to_string(),
            "config",
            command,
            "roles.myrole",
        ]);
        let result = orchestraitor_cli::run_with_writer(cli, &mut output);
        let error = match result {
            Ok(()) => {
                return Err(miette::miette!("{command} resolved an empty role table"));
            }
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("is not set"),
            "{command} must report the empty table as not set, got: {error}"
        );
    }
    Ok(())
}

#[test]
fn routing_resolve_accepts_a_custom_role_from_the_project_layer() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
         [roles.migrator.routing]\nprovider = \"acme\"\nmodel = \"acme-pro\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "routing",
        "resolve",
        "--role",
        "migrator",
        "--json",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(json["role"], "migrator");
    assert_eq!(json["provider"], "acme");
    assert_eq!(json["model"], "acme-pro");
    assert_eq!(
        json["precedence_path"],
        "roles.migrator.routing (provider: project, model: project)"
    );
    Ok(())
}

#[test]
fn validate_rejects_same_layer_role_routing_conflict() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let project_shards = temp.path().join("orchestraitor.d");
    fs::create_dir_all(&project_shards).into_diagnostic()?;
    fs::write(
        project_shards.join("a.toml"),
        "[roles.myrole.routing]\nprovider = \"neuralwatt\"\nmodel = \"glm-5.2\"\n",
    )
    .into_diagnostic()?;
    fs::write(
        project_shards.join("b.toml"),
        "[roles.myrole.routing]\nprovider = \"neuralwatt\"\nmodel = \"glm-5.2\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--project-dir",
        &temp.path().display().to_string(),
        "config",
        "validate",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);
    let error = match result {
        Ok(()) => {
            return Err(miette::miette!(
                "same-layer role conflict passed validation"
            ));
        }
        Err(error) => error,
    };

    let text = error.to_string();
    assert!(
        text.contains("ambiguous configuration conflict for key `roles.myrole.routing."),
        "error must name the conflicting role key, got: {text}"
    );
    assert!(text.contains("a.toml"), "error must name source a.toml");
    assert!(text.contains("b.toml"), "error must name source b.toml");
    Ok(())
}

#[test]
fn routing_resolve_persists_six_distinct_role_records() -> miette::Result<()> {
    use orchestraitor_agent_catalog::{BUILT_IN_ORCHESTRATION_ROLES, RoleRoutingDecisionStore};

    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;

    for role in BUILT_IN_ORCHESTRATION_ROLES {
        let mut output = Vec::new();
        let cli = Cli::parse_from([
            "orc",
            "--config-dir",
            &config_dir.display().to_string(),
            "--project-dir",
            &temp.path().display().to_string(),
            "routing",
            "resolve",
            "--role",
            role.id,
            "--json",
        ]);
        orchestraitor_cli::run_with_writer(cli, &mut output)?;
        let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;
        assert_eq!(json["role"], role.id);
        assert_eq!(json["provider"], "neuralwatt");
        assert_eq!(json["model"], "glm-5.2");
        assert!(json["record_id"].is_number());
    }

    let store = RoleRoutingDecisionStore::open(&config_dir.join("routing.db"))
        .map_err(|error| miette::miette!("decision store reopen failed: {error}"))?;
    let records = store
        .list()
        .map_err(|error| miette::miette!("decision store list failed: {error}"))?;
    assert_eq!(records.len(), 6);
    let distinct: std::collections::BTreeSet<_> =
        records.iter().map(|record| record.role.as_str()).collect();
    assert_eq!(distinct.len(), 6);
    Ok(())
}

#[test]
fn routing_resolve_inherits_missing_subkey_from_builtin_defaults() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[roles.implement.routing]\nprovider = \"neuralwatt\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "routing",
        "resolve",
        "--role",
        "implement",
        "--json",
    ]);
    orchestraitor_cli::run_with_writer(cli, &mut output)?;
    let json: serde_json::Value = serde_json::from_slice(&output).into_diagnostic()?;

    assert_eq!(json["provider"], "neuralwatt");
    assert_eq!(json["model"], "glm-5.2");
    assert_eq!(
        json["precedence_path"],
        "roles.implement.routing (provider: project, model: built-in-defaults)"
    );
    Ok(())
}

#[test]
fn routing_resolve_unconfigured_provider_names_the_key() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    let config_dir = temp.path().join("config");
    fs::create_dir_all(&config_dir).into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
         [roles.plan.routing]\nprovider = \"unknownco\"\nmodel = \"x\"\n",
    )
    .into_diagnostic()?;
    let mut output = Vec::new();

    let cli = Cli::parse_from([
        "orc",
        "--config-dir",
        &config_dir.display().to_string(),
        "--project-dir",
        &temp.path().display().to_string(),
        "routing",
        "resolve",
        "--role",
        "plan",
    ]);
    let result = orchestraitor_cli::run_with_writer(cli, &mut output);

    let error = match result {
        Ok(()) => return Err(miette::miette!("unconfigured provider resolved silently")),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("roles.plan.routing.provider"),
        "error must name the offending key, got: {error}"
    );
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

/// Serves one mint response, then API responses on the next connections.
/// The API response echoes the incoming Authorization header inside the body
/// so tests can assert where the installation token traveled.
///
/// Every request's path and Authorization header value are sent to the
/// returned channel as `"<path>\t<auth-or-empty>"`, so tests can assert WHICH
/// bearer traveled to WHICH endpoint (installation token for `api`, App JWT
/// for `commit-author`, no mint request at all for `commit-author`).
fn spawn_mint_then_api_server(
    api_status: u16,
    api_body: &str,
) -> miette::Result<(String, std::sync::mpsc::Receiver<String>)> {
    use std::sync::mpsc;
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!("http://{}", listener.local_addr().into_diagnostic()?);
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
    let mint_body =
        format!(r#"{{"token":"{GITHUB_APP_TOKEN_MARKER}","expires_at":"{expires_at}"}}"#);
    let api_body_owned = api_body.to_string();
    let (auth_tx, auth_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            let mut request_bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request_bytes.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&request_bytes);
                        let Some(match_headers_end) = text.find("\r\n\r\n") else {
                            continue;
                        };
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
            let request = String::from_utf8_lossy(&request_bytes).to_string();
            let request_line = request.lines().next().unwrap_or_default();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let auth = request
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_default();
            let _ = auth_tx.send(format!("{path}\t{auth}"));
            let is_mint = path.contains("/app/installations/");
            let (status_line, body) = if is_mint {
                (String::from("HTTP/1.1 201 Created"), mint_body.clone())
            } else {
                let status_line = format!("HTTP/1.1 {api_status}");
                (status_line, api_body_owned.clone())
            };
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    Ok((endpoint, auth_rx))
}

fn github_cli(
    temp: &tempfile::TempDir,
    endpoint: &str,
    args: &[&str],
) -> miette::Result<std::process::Output> {
    use std::process::{Command, Stdio};
    Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            endpoint,
        ])
        .args(args)
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .into_diagnostic()
}

#[test]
fn github_api_passthrough_prints_body_and_exits_zero_on_2xx() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    let (endpoint, auth_rx) = spawn_mint_then_api_server(200, r#"{"number":446,"state":"open"}"#)?;

    let output = github_cli(
        &temp,
        &endpoint,
        &[
            "github",
            "api",
            "POST",
            "repos/arbsec/orchestraitor/issues",
            "--field",
            "title=hello",
        ],
    )?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    assert_eq!(stdout.trim(), r#"{"number":446,"state":"open"}"#);
    assert!(!stdout.contains(GITHUB_APP_TOKEN_MARKER));
    // Exactly two requests: the mint, then the API call — and the API call
    // must carry the INSTALLATION token (not the App JWT).
    let mint = auth_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .into_diagnostic()?;
    assert!(
        mint.starts_with("/app/installations/165043398/access_tokens\t"),
        "{mint}"
    );
    assert!(mint.contains("\tBearer eyJ"), "{mint}");
    let api = auth_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .into_diagnostic()?;
    assert!(
        api.starts_with("/repos/arbsec/orchestraitor/issues\t"),
        "{api}"
    );
    assert_eq!(
        api.split('\t').next_back(),
        Some(format!("Bearer {GITHUB_APP_TOKEN_MARKER}").as_str()),
        "api must send the installation token, never the App JWT: {api}"
    );
    Ok(())
}

#[test]
fn github_api_passthrough_exits_nonzero_on_4xx_without_leaking_headers() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    // The 404 body deliberately contains a token-look-alike; the CLI must
    // still print the body (caller's business) but the failure must be
    // status-shaped and the stderr must not name the Authorization header.
    let (endpoint, _auth_rx) =
        spawn_mint_then_api_server(404, r#"{"message":"ghs_fakeLEAK0123"}"#)?;

    let output = github_cli(&temp, &endpoint, &["github", "api", "GET", "/app"])?;

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert_eq!(stdout.trim(), r#"{"message":"ghs_fakeLEAK0123"}"#);
    assert!(stderr.contains("404"), "stderr: {stderr}");
    assert!(!stderr.contains("ghs_fakeLEAK0123"), "stderr: {stderr}");
    assert!(
        !stderr.to_lowercase().contains("authorization"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains(GITHUB_APP_TOKEN_MARKER),
        "stderr: {stderr}"
    );
    Ok(())
}

#[test]
fn github_api_get_fields_become_query_parameters_not_a_body() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    let (endpoint, auth_rx) = spawn_mint_then_api_server(200, r#"[{"number":1},{"number":2}]"#)?;

    // `-f state=closed` on GET must ride the URL query string (the `gh api`
    // shape), never a silently-dropped JSON body. The value contains a space
    // and a comma to prove percent-encoding.
    let output = github_cli(
        &temp,
        &endpoint,
        &[
            "github",
            "api",
            "GET",
            "repos/arbsec/orchestraitor/issues",
            "--field",
            "labels=bug,help wanted",
            "--field",
            "state=closed",
        ],
    )?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _mint = auth_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .into_diagnostic()?;
    let api = auth_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .into_diagnostic()?;
    let api_path = api.split('\t').next().unwrap_or_default();
    assert_eq!(
        api_path, "/repos/arbsec/orchestraitor/issues?labels=bug%2Chelp+wanted&state=closed",
        "GET --field pairs must be percent-encoded query parameters: {api_path}"
    );
    Ok(())
}

#[test]
fn github_api_rejects_unknown_method_before_minting() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    // No endpoint/server: the typed method error must fire before any mint.
    let output = github_cli(
        &temp,
        "http://127.0.0.1:1",
        &["github", "api", "CONNECT", "/app"],
    )?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(stderr.contains("unsupported method"), "stderr: {stderr}");
    Ok(())
}

#[test]
fn github_commit_author_derives_identity_from_app_and_bot_user_responses() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    // GET /app carries the slug but NOT the bot user id (live GitHub shape);
    // the id comes from the follow-up GET /users/{slug}[bot] profile.
    let server = spawn_two_response_server(
        r#"{"id":5082653,"slug":"arbsec-agent"}"#,
        r#"{"id":334074867,"login":"arbsec-agent[bot]","type":"Bot"}"#,
    )?;

    let output = github_cli(&temp, &server.endpoint, &["github", "commit-author"])?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "name=arbsec-agent[bot]",
            "email=334074867+arbsec-agent[bot]@users.noreply.github.com"
        ]
    );
    assert!(!stdout.contains(GITHUB_APP_TOKEN_MARKER));
    // Request order and credentials: the App-JWT bearer goes ONLY to /app;
    // the bot-user lookup is unauthenticated (no Authorization header), and
    // NO installation-token mint happens on this subcommand.
    let first = server
        .auth_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .into_diagnostic()?;
    assert!(
        first.starts_with("/app\tBearer eyJ"),
        "commit-author must send the App JWT to GET /app: {first}"
    );
    let second = server
        .auth_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .into_diagnostic()?;
    assert_eq!(
        second, "/users/arbsec-agent[bot]\t",
        "bot-user lookup must be unauthenticated: {second}"
    );
    Ok(())
}

/// Serves two distinct JSON responses on successive connections and records
/// each request's path + Authorization header (same recording shape as
/// [`spawn_mint_then_api_server`]).
fn spawn_two_response_server(
    first_body: &str,
    second_body: &str,
) -> miette::Result<TwoResponseServer> {
    spawn_sequence_server(&[first_body, second_body])
}

/// Serves the given JSON bodies on successive connections (the last body
/// repeats for any further connections) and records each request's path +
/// Authorization header. Unlike [`spawn_mint_then_api_server`] this serves
/// NO mint response: every connection gets the next scripted body.
fn spawn_sequence_server(bodies: &[&str]) -> miette::Result<TwoResponseServer> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!("http://{}", listener.local_addr().into_diagnostic()?);
    let mut bodies: Vec<String> = bodies.iter().map(ToString::to_string).collect();
    let (auth_tx, auth_rx) = std::sync::mpsc::channel::<String>();
    thread::spawn(move || {
        for (index, connection) in listener.incoming().enumerate() {
            let Ok(mut stream) = connection else { break };
            let mut request_bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request_bytes.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&request_bytes);
                        if text.contains("\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let request = String::from_utf8_lossy(&request_bytes).to_string();
            let request_line = request.lines().next().unwrap_or_default();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let auth = request
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_default();
            let _ = auth_tx.send(format!("{path}\t{auth}"));
            let body = bodies
                .get_mut(index)
                .map_or_else(|| "{}".to_string(), std::mem::take);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    Ok(TwoResponseServer { endpoint, auth_rx })
}

struct TwoResponseServer {
    endpoint: String,
    auth_rx: std::sync::mpsc::Receiver<String>,
}

/// Serves one mint response (the recorded fixture token), then two scripted
/// API responses on the following connections — the flow
/// `gh-env -- required` performs: mint (installation token), `GET /app`
/// (App JWT), `GET /users/{slug}[bot]` (unauthenticated).
fn spawn_mint_then_two_api_server(
    first_api_body: &str,
    second_api_body: &str,
) -> miette::Result<TwoResponseServer> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).into_diagnostic()?;
    let endpoint = format!("http://{}", listener.local_addr().into_diagnostic()?);
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
    let mint_body =
        format!(r#"{{"token":"{GITHUB_APP_TOKEN_MARKER}","expires_at":"{expires_at}"}}"#);
    let mut bodies = vec![
        mint_body,
        first_api_body.to_string(),
        second_api_body.to_string(),
    ];
    let (auth_tx, auth_rx) = std::sync::mpsc::channel::<String>();
    thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            let mut request_bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        request_bytes.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&request_bytes);
                        if text.contains("\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let request = String::from_utf8_lossy(&request_bytes).to_string();
            let request_line = request.lines().next().unwrap_or_default();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let auth = request
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_default();
            let _ = auth_tx.send(format!("{path}\t{auth}"));
            // Mint requests are detected by path so the scripted bodies stay
            // positional: connection 1 = mint, 2 = /app, 3 = /users/{slug}[bot].
            let is_mint = path.contains("/app/installations/");
            let body = if is_mint {
                bodies[0].clone()
            } else if bodies.len() > 2 {
                // Consume the next scripted API body; the last one repeats
                // for any further connection.
                bodies.remove(1)
            } else {
                bodies[1].clone()
            };
            let (status_line, payload) = if is_mint {
                (String::from("HTTP/1.1 201 Created"), body)
            } else {
                (String::from("HTTP/1.1 200 OK"), body)
            };
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _write_result = stream.write_all(response.as_bytes());
        }
    });
    Ok(TwoResponseServer { endpoint, auth_rx })
}

#[test]
#[cfg(unix)]
fn github_gh_env_injects_gh_token_into_child_and_propagates_exit() -> miette::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    // Child script: asserts the token reaches GH_TOKEN by writing its LENGTH
    // (never the value) into a file, then exits with a distinctive code.
    let child = temp.path().join("child.sh");
    let out_file = temp.path().join("child.out");
    fs::write(
        &child,
        format!(
            "#!/bin/sh\nprintf '%s' \"$GH_TOKEN\" | wc -c | tr -d ' \"' > \"{}\"\nexit 7\n",
            out_file.display()
        ),
    )
    .into_diagnostic()?;
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
    let (endpoint, _auth_rx) = spawn_mint_then_api_server(200, "{}")?;

    let output = github_cli(
        &temp,
        &endpoint,
        &["github", "gh-env", "--", &child.display().to_string()],
    )?;

    // The child's exit code propagates...
    assert_eq!(output.status.code(), Some(7));
    // ... orc's own streams never contain the token ...
    let stdout = String::from_utf8(output.stdout).into_diagnostic()?;
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    assert!(stdout.is_empty() && stderr.is_empty());
    assert!(!stdout.contains(GITHUB_APP_TOKEN_MARKER));
    // ... and the child DID observe GH_TOKEN with the expected length.
    let observed = fs::read_to_string(&out_file).into_diagnostic()?;
    assert_eq!(observed.trim(), GITHUB_APP_TOKEN_MARKER.len().to_string());
    Ok(())
}

#[test]
fn github_gh_env_fails_typed_when_child_is_missing() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    let (endpoint, _auth_rx) = spawn_mint_then_api_server(200, "{}")?;

    let output = github_cli(
        &temp,
        &endpoint,
        &[
            "github",
            "gh-env",
            "--",
            "definitely-not-a-real-binary-orchestraitor",
        ],
    )?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    // miette's fancy renderer wraps lines; collapse whitespace before matching.
    let flat: String = stderr.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat.contains("notfoundon") && flat.contains("PATH"),
        "stderr: {stderr}"
    );
    Ok(())
}

#[test]
fn github_gh_env_required_enforcement_fails_closed_when_config_absent() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // No github_app block at all; enforcement still readable as `required`
    // via the built-in defaults' slug layer + this project layer.
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nenforcement = \"required\"\n",
    )
    .into_diagnostic()?;
    let (endpoint, _auth_rx) = spawn_mint_then_api_server(200, "{}")?;

    let output = github_cli(&temp, &endpoint, &["github", "gh-env", "--", "true"])?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let flat: String = stderr.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat.contains("enforcementis`required`") && flat.contains("client_id"),
        "typed refusal must name the mode and the missing keys: {stderr}"
    );
    Ok(())
}

#[test]
fn github_gh_env_required_enforcement_fails_closed_when_config_partial() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nenforcement = \"required\"\nclient_id = \"Iv1.cli-e2e\"\n",
    )
    .into_diagnostic()?;
    let (endpoint, _auth_rx) = spawn_mint_then_api_server(200, "{}")?;

    let output = github_cli(&temp, &endpoint, &["github", "gh-env", "--", "true"])?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let flat: String = stderr.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat.contains("missing:installation_id,") && flat.contains("private_key_uri"),
        "typed refusal must list every missing key: {stderr}"
    );
    Ok(())
}

#[test]
fn github_gh_env_env_pinned_required_runs_gate_when_layered_unset() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // Complete App config, layered enforcement UNSET, env pin `required`:
    // the pin must upgrade the gate — with an attacker git identity the
    // canonical-identity check has to fire (previously the pin was
    // wrapper-only and orc delegated without the gate).
    write_github_app_project_config(&temp)?;
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-C", &repo_str])
            .args(args)
            .output()
            .into_diagnostic()
    };
    assert!(git(&["init", "-q"])?.status.success());
    assert!(
        git(&["config", "user.email", "attacker@evil.example"])?
            .status
            .success()
    );
    let child = repo.join("child.sh");
    fs::write(&child, "#!/bin/sh\nexit 0\n").into_diagnostic()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
    }
    #[cfg(not(unix))]
    {
        // Best effort on non-unix: the test body below still exercises the
        // gate before any spawn.
    }
    let child_str = child.display().to_string();
    let server = spawn_mint_then_two_api_server(
        r#"{"id":5082653,"slug":"arbsec-agent"}"#,
        r#"{"id":334074867,"login":"arbsec-agent[bot]","type":"Bot"}"#,
    )?;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &server.endpoint,
            "github",
            "gh-env",
            "--",
            &child_str,
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .env("ORC_GITHUB_APP_ENFORCEMENT", "required")
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let flat: String = stderr
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '@')
        .collect();
    assert!(
        flat.contains("isnottheserviceidentitybotscanonicalnoreplyaddress"),
        "env-pinned required must run the canonical-identity gate: {stderr}"
    );
    Ok(())
}

#[test]
fn github_gh_env_env_pinned_invalid_value_fails_closed() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    let (endpoint, _auth_rx) = spawn_mint_then_api_server(200, "{}")?;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &endpoint,
            "github",
            "gh-env",
            "--",
            "true",
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .env("ORC_GITHUB_APP_ENFORCEMENT", "optional")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let flat: String = stderr.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat.contains("invalidORC_GITHUB_APP_ENFORCEMENTvalue"),
        "invalid pin value must fail closed with a typed error: {stderr}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn github_gh_env_required_mode_pins_git_identity_env_for_child() -> miette::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nenforcement = \"required\"\nclient_id = \"Iv1.cli-e2e\"\n\
         installation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()?;
    // The required-mode gate resolves the bot identity from the live App and
    // pins GIT_AUTHOR_*/GIT_COMMITTER_* into the child env: a child that
    // runs `git -c user.email=attacker@… commit` still stamps bot
    // attribution, because those env vars beat repo config (and `-c`
    // config) for commit identity.
    let child = temp.path().join("child.sh");
    let out_file = temp.path().join("child.out");
    // The child sets a HOSTILE per-invocation config override, then records
    // what git would actually use for author/committer identity. Because
    // GIT_AUTHOR_*/GIT_COMMITTER_* env vars beat `-c` config, the identity
    // must still be the bot's.
    fs::write(
        &child,
        format!(
            concat!(
                "#!/bin/sh\n",
                "export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null\n",
                "git -c user.name=attacker -c user.email=attacker@evil.example commit --allow-empty -m x >/dev/null 2>&1 && git log -1 --format='%an|%ae|%cn|%ce' > \"{}\" 2>/dev/null\n",
                "exit 0\n"
            ),
            out_file.display()
        ),
    )
    .into_diagnostic()?;
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-C", &repo_str])
            .args(args)
            .output()
            .into_diagnostic()
    };
    assert!(git(&["init", "-q"])?.status.success());
    // The per-repo gitconfig must already carry the bot identity (the gate
    // verifies it before delegating); the env pin then protects against
    // per-invocation `git -c` overrides inside the child.
    assert!(
        git(&["config", "user.name", "arbsec-agent[bot]"])?
            .status
            .success()
    );
    assert!(
        git(&[
            "config",
            "user.email",
            "334074867+arbsec-agent[bot]@users.noreply.github.com"
        ])?
        .status
        .success()
    );
    let child_str = child.display().to_string();
    let server = spawn_mint_then_two_api_server(
        r#"{"id":5082653,"slug":"arbsec-agent"}"#,
        r#"{"id":334074867,"login":"arbsec-agent[bot]","type":"Bot"}"#,
    )?;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &server.endpoint,
            "github",
            "gh-env",
            "--",
            &child_str,
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let observed = fs::read_to_string(&out_file).into_diagnostic()?;
    assert_eq!(
        observed.trim(),
        "arbsec-agent[bot]|334074867+arbsec-agent[bot]@users.noreply.github.com|\
         arbsec-agent[bot]|334074867+arbsec-agent[bot]@users.noreply.github.com",
        "child git identity must be env-pinned to the bot even under `git -c` overrides: {observed}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn github_gh_env_required_enforcement_refuses_personal_git_identity() -> miette::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    // enforcement = required rides the same project layer.
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nenforcement = \"required\"\nclient_id = \"Iv1.cli-e2e\"\n\
         installation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()?;
    // A real git repo whose identity is PERSONAL — the forbidden effect.
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-C", &repo_str])
            .args(args)
            .output()
            .into_diagnostic()
    };
    assert!(git(&["init", "-q"])?.status.success());
    assert!(
        git(&["config", "user.email", "marcus.ekwall@gmail.com"])?
            .status
            .success()
    );
    let child = repo.join("child.sh");
    fs::write(&child, "#!/bin/sh\nexit 0\n").into_diagnostic()?;
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
    let child_str = child.display().to_string();
    // The required-mode gate resolves the expected bot identity from the live
    // App (GET /app -> slug, GET /users/{slug}[bot] -> id) before delegating.
    let server = spawn_two_response_server(
        r#"{"id":5082653,"slug":"arbsec-agent"}"#,
        r#"{"id":334074867,"login":"arbsec-agent[bot]","type":"Bot"}"#,
    )?;

    // The child process must observe the personal identity: cwd is the repo.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &server.endpoint,
            "github",
            "gh-env",
            "--",
            &child_str,
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    // Collapse whitespace AND miette's gutter glyphs (`│`, `·`), which are not
    // whitespace but sit inside wrapped lines at terminal-width-dependent
    // positions — asserting on the collapsed text must not depend on where
    // miette happened to break the line (differs between Linux and macOS).
    // Keep only ASCII alphanumerics (plus `@` in emails): miette wraps at
    // terminal-width-dependent positions and its gutter glyphs (`│`), the
    // em-dash, and hyphens sit at different places per platform, so any
    // substring assertion on the rendered text must ignore all punctuation.
    let flat: String = stderr
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '@')
        .collect();
    assert!(
        flat.contains("isnottheserviceidentitybotscanonicalnoreplyaddress"),
        "typed refusal must name the identity mismatch: {stderr}"
    );
    Ok(())
}

#[test]
fn github_gh_env_recommended_mode_fails_typed_when_config_absent() -> miette::Result<()> {
    let temp = tempfile::tempdir().into_diagnostic()?;
    // No github_app block: recommended mode keeps the labelled fallback
    // surface (the daemon-side wrapper owns the WARNING); gh-env itself
    // still fails typed — minting without a complete config is impossible.
    let (endpoint, _auth_rx) = spawn_mint_then_api_server(200, "{}")?;

    let output = github_cli(&temp, &endpoint, &["github", "gh-env", "--", "true"])?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let flat: String = stderr.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat.contains("github_app.client_id") && flat.contains("notset"),
        "recommended mode keeps the typed mint error when config is absent: {stderr}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn github_gh_env_required_enforcement_rejects_generic_noreply_identity() -> miette::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nenforcement = \"required\"\nclient_id = \"Iv1.cli-e2e\"\n\
         installation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()?;
    // A generic @users.noreply.github.com identity (e.g. some other account's
    // noreply address) must ALSO be refused: only the bot pattern passes.
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-C", &repo_str])
            .args(args)
            .output()
            .into_diagnostic()
    };
    assert!(git(&["init", "-q"])?.status.success());
    assert!(
        git(&["config", "user.email", "attacker@users.noreply.github.com"])?
            .status
            .success()
    );
    let child = repo.join("child.sh");
    fs::write(&child, "#!/bin/sh\nexit 0\n").into_diagnostic()?;
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
    let child_str = child.display().to_string();
    // The required-mode gate resolves the expected bot identity from the live
    // App (GET /app -> slug, GET /users/{slug}[bot] -> id) before delegating.
    let server = spawn_two_response_server(
        r#"{"id":5082653,"slug":"arbsec-agent"}"#,
        r#"{"id":334074867,"login":"arbsec-agent[bot]","type":"Bot"}"#,
    )?;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &server.endpoint,
            "github",
            "gh-env",
            "--",
            &child_str,
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    // Keep only ASCII alphanumerics (plus `@` in emails): miette wraps at
    // terminal-width-dependent positions and its gutter glyphs (`│`), the
    // em-dash, and hyphens sit at different places per platform, so any
    // substring assertion on the rendered text must ignore all punctuation.
    let flat: String = stderr
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '@')
        .collect();
    assert!(
        flat.contains("isnottheserviceidentitybotscanonicalnoreplyaddress"),
        "generic noreply identity must be refused in required mode: {stderr}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn github_gh_env_required_enforcement_rejects_suffix_lookalike_bot_email() -> miette::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().into_diagnostic()?;
    write_github_app_project_config(&temp)?;
    fs::write(
        temp.path().join("orchestraitor.toml"),
        "[github_app]\nenforcement = \"required\"\nclient_id = \"Iv1.cli-e2e\"\n\
         installation_id = 165043398\n\
         private_key_uri = \"secret://env/ORCHESTRAITOR_CLI_TEST_GITHUB_APP_PEM\"\n",
    )
    .into_diagnostic()?;
    // A suffix match on `+arbsec-agent[bot]@users.noreply.github.com` would
    // accept this look-alike (wrong local part); only the canonical
    // `<bot-id>+<slug>[bot]@…` address passes.
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).into_diagnostic()?;
    let repo_str = repo.display().to_string();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-C", &repo_str])
            .args(args)
            .output()
            .into_diagnostic()
    };
    assert!(git(&["init", "-q"])?.status.success());
    assert!(
        git(&[
            "config",
            "user.email",
            "anything+arbsec-agent[bot]@users.noreply.github.com"
        ])?
        .status
        .success()
    );
    let child = repo.join("child.sh");
    fs::write(&child, "#!/bin/sh\nexit 0\n").into_diagnostic()?;
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
    let child_str = child.display().to_string();
    let server = spawn_two_response_server(
        r#"{"id":5082653,"slug":"arbsec-agent"}"#,
        r#"{"id":334074867,"login":"arbsec-agent[bot]","type":"Bot"}"#,
    )?;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orc"))
        .args([
            "--project-dir",
            &temp.path().display().to_string(),
            "--config-dir",
            &temp.path().display().to_string(),
            "--github-api-endpoint",
            &server.endpoint,
            "github",
            "gh-env",
            "--",
            &child_str,
        ])
        .env(GITHUB_APP_PEM_ENV_VAR, GITHUB_APP_FIXTURE_PEM)
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .into_diagnostic()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).into_diagnostic()?;
    let flat: String = stderr
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '@')
        .collect();
    assert!(
        flat.contains("isnottheserviceidentitybotscanonicalnoreplyaddress"),
        "suffix look-alike bot email must be refused in required mode: {stderr}"
    );
    Ok(())
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

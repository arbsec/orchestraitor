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
    let Err(error) = result else {
        return Err(miette::miette!("ambiguous conflict passed validation"));
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

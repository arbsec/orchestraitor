//! `orc models` implementation.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use miette::{IntoDiagnostic, Result, bail};
use orchestraitor_provider_meta::ModelsDevClient;

use crate::cli::{ConfigPaths, ModelsCommand};

/// Runs an `orc models` subcommand.
///
/// # Errors
/// Returns a diagnostic when catalog fetch, cache write, or rollback fails.
pub fn run<W: Write>(paths: &ConfigPaths, command: ModelsCommand, writer: &mut W) -> Result<()> {
    match command {
        ModelsCommand::Refresh => refresh(paths, writer),
        ModelsCommand::Rollback => rollback(paths, writer),
    }
}

fn refresh<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    let client = match &paths.models_dev_endpoint {
        Some(endpoint) => ModelsDevClient::with_endpoint(endpoint.clone(), Some(cache_dir(paths))),
        None => ModelsDevClient::new(Some(cache_dir(paths))),
    }
    .into_diagnostic()?;
    let catalog = runtime.block_on(client.refresh_now()).into_diagnostic()?;
    writeln!(
        writer,
        "refreshed models.dev catalog digest={} source={:?}",
        catalog.digest, catalog.source
    )
    .into_diagnostic()
}

fn rollback<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    let mut entries = cache_entries(paths)?;
    entries.sort_by_key(|entry| entry.modified);
    if entries.len() < 2 {
        bail!("models.dev rollback requires at least two cached snapshots");
    }
    let newest = entries
        .pop()
        .ok_or_else(|| miette::miette!("missing newest snapshot"))?;
    let previous = entries
        .pop()
        .ok_or_else(|| miette::miette!("missing previous snapshot"))?;
    fs::remove_file(&newest.path).into_diagnostic()?;
    writeln!(
        writer,
        "rolled back models.dev catalog to {}",
        previous.path.display()
    )
    .into_diagnostic()
}

#[derive(Debug)]
struct CacheEntry {
    path: PathBuf,
    modified: std::time::SystemTime,
}

fn cache_entries(paths: &ConfigPaths) -> Result<Vec<CacheEntry>> {
    let dir = cache_dir(paths);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).into_diagnostic(),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let metadata = entry.metadata().into_diagnostic()?;
        if metadata.is_file() {
            files.push(CacheEntry {
                path: entry.path(),
                modified: metadata.modified().into_diagnostic()?,
            });
        }
    }
    Ok(files)
}

fn cache_dir(paths: &ConfigPaths) -> PathBuf {
    paths.config_dir.join("models-dev")
}

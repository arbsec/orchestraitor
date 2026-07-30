//! `orc config` implementation.

mod edit;
mod layers;
mod values;

use std::fs;
use std::io::Write;

use miette::{IntoDiagnostic, Result, miette};
use toml_edit::Value;

use crate::cli::{
    CliLayer, ConfigCommand, ConfigPaths, DiffArgs, KeyArgs, LayeredKeyArgs, SetArgs,
};

use edit::{parse_cli_value, read_document, remove_key, schema_version, set_key, write_document};
use layers::{BUILT_IN_DEFAULTS, layer_name, layer_path, load_layers};
use values::{diff_entries, read_value_map, read_value_map_from_str, render_json_value};
const CURRENT_SCHEMA_VERSION: &str = "0.14";

/// Runs an `orc config` subcommand.
///
/// # Errors
/// Returns a diagnostic when config parsing, resolution, mutation, or output fails.
pub fn run<W: Write>(paths: &ConfigPaths, command: ConfigCommand, writer: &mut W) -> Result<()> {
    match command {
        ConfigCommand::Get(args) => get(paths, &args, writer),
        ConfigCommand::Explain(args) => explain(paths, &args, writer),
        ConfigCommand::Set(args) => set(paths, &args, writer),
        ConfigCommand::Unset(args) => unset(paths, &args, writer),
        ConfigCommand::Validate => validate(paths, writer),
        ConfigCommand::Diff(args) => diff(paths, &args, writer),
        ConfigCommand::Migrate => migrate(paths, writer),
    }
}

fn get<W: Write>(paths: &ConfigPaths, args: &KeyArgs, writer: &mut W) -> Result<()> {
    let resolved = load_layers(paths)?.resolved_map()?;
    let value = resolved
        .get(&args.key)
        .ok_or_else(|| miette!("config key `{}` is not set", args.key))?;
    writeln!(writer, "{}", render_json_value(&value.value)).into_diagnostic()
}

fn explain<W: Write>(paths: &ConfigPaths, args: &KeyArgs, writer: &mut W) -> Result<()> {
    let resolved = load_layers(paths)?.resolved_map()?;
    let value = resolved
        .get(&args.key)
        .ok_or_else(|| miette!("config key `{}` is not set", args.key))?;
    writeln!(writer, "key = {}", args.key).into_diagnostic()?;
    writeln!(writer, "value = {}", render_json_value(&value.value)).into_diagnostic()?;
    writeln!(writer, "source_layer = {}", value.source_layer).into_diagnostic()?;
    writeln!(writer, "source = {}", value.source_name).into_diagnostic()?;
    writeln!(writer, "inherited = {}", value.inherited).into_diagnostic()?;
    writeln!(writer, "profile_contribution = none").into_diagnostic()
}

fn set<W: Write>(paths: &ConfigPaths, args: &SetArgs, writer: &mut W) -> Result<()> {
    let path = layer_path(paths, args.layer);
    let mut document = read_document(&path)?;
    set_key(&mut document, &args.key, parse_cli_value(&args.value))?;
    write_document(&path, &document)?;
    writeln!(writer, "set {} at {}", args.key, layer_name(args.layer)).into_diagnostic()
}

fn unset<W: Write>(paths: &ConfigPaths, args: &LayeredKeyArgs, writer: &mut W) -> Result<()> {
    let path = layer_path(paths, args.layer);
    let mut document = read_document(&path)?;
    remove_key(&mut document, &args.key)?;
    write_document(&path, &document)?;
    writeln!(writer, "unset {} at {}", args.key, layer_name(args.layer)).into_diagnostic()
}

fn validate<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    let layers = load_layers(paths)?;
    if !layers.unknown_keys.is_empty() {
        writeln!(writer, "unknown keys:").into_diagnostic()?;
        for unknown in &layers.unknown_keys {
            writeln!(writer, "- {} ({})", unknown.key, unknown.source).into_diagnostic()?;
        }
    }
    layers.resolver.resolve_config().map_err(|error| {
        miette!(
            "configuration validation failed: {}",
            error.structured().cause
        )
    })?;
    if layers.unknown_keys.is_empty() {
        writeln!(writer, "config valid").into_diagnostic()?;
    }
    Ok(())
}

fn diff<W: Write>(paths: &ConfigPaths, args: &DiffArgs, writer: &mut W) -> Result<()> {
    let target = match args.layer {
        Some(layer) => read_value_map(&layer_path(paths, layer))?,
        None => load_layers(paths)?.effective_map()?,
    };
    let baseline = read_value_map_from_str(BUILT_IN_DEFAULTS)?;
    let entries = diff_entries(&baseline, &target);
    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &entries).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
        return Ok(());
    }
    for entry in entries {
        writeln!(writer, "{}: {} -> {}", entry.key, entry.before, entry.after).into_diagnostic()?;
    }
    Ok(())
}

fn migrate<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    for layer in [
        CliLayer::Project,
        CliLayer::User,
        CliLayer::Org,
        CliLayer::Dir,
    ] {
        let path = layer_path(paths, layer);
        if !path.exists() {
            continue;
        }
        let mut document = read_document(&path)?;
        let previous = schema_version(&document);
        if previous.as_deref() == Some(CURRENT_SCHEMA_VERSION) {
            continue;
        }
        let backup = path.with_extension(format!(
            "toml.bak.{}",
            previous.unwrap_or_else(|| "pre-schema".to_string())
        ));
        fs::copy(&path, &backup).into_diagnostic()?;
        set_key(
            &mut document,
            "schema_version",
            Value::from(CURRENT_SCHEMA_VERSION),
        )?;
        write_document(&path, &document)?;
        writeln!(
            writer,
            "migrated {} (backup: {})",
            path.display(),
            backup.display()
        )
        .into_diagnostic()?;
    }
    Ok(())
}

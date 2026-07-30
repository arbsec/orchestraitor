//! Layer loading, precedence resolution, and provenance tracking.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use miette::{IntoDiagnostic, Result, bail, miette};
use orchestraitor_core::{ConfigLayer, ConfigResolver, ConfigSource};

use crate::cli::{CliLayer, ConfigPaths};
use crate::commands::config::values::{flatten_json, read_value_map_from_str};

pub(crate) const BUILT_IN_DEFAULTS: &str = r#"
[normalization]
format_on_write = true
max_passes = 2
safe_fix_classifications = ["format", "organize-imports"]

[retry]
max_attempts = 3
backoff_ms = 250
"#;

#[derive(Debug, Clone)]
pub(crate) struct LoadedLayers {
    pub(crate) resolver: ConfigResolver,
    pub(crate) unknown_keys: Vec<UnknownKey>,
    layer_values: Vec<LayerValues>,
}

impl LoadedLayers {
    pub(crate) fn effective_map(&self) -> Result<BTreeMap<String, serde_json::Value>> {
        let config = self.resolver.resolve_config().map_err(|error| {
            miette!(
                "configuration validation failed: {}",
                error.structured().cause
            )
        })?;
        let value = serde_json::to_value(config).into_diagnostic()?;
        Ok(flatten_json(&value))
    }

    pub(crate) fn resolved_map(&self) -> Result<BTreeMap<String, ResolvedJson>> {
        let mut resolved = BTreeMap::new();
        reject_same_layer_conflicts(&self.layer_values)?;
        let mut sorted = self.layer_values.clone();
        sorted.sort_by_key(|layer| layer.layer);
        for layer in sorted {
            for (key, value) in layer.values {
                resolved.insert(
                    key,
                    ResolvedJson {
                        value,
                        source_layer: layer.layer_name.clone(),
                        source_name: layer.source_name.clone(),
                        inherited: false,
                    },
                );
            }
        }
        Ok(resolved)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UnknownKey {
    pub(crate) source: String,
    pub(crate) key: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedJson {
    pub(crate) value: serde_json::Value,
    pub(crate) source_layer: String,
    pub(crate) source_name: String,
    pub(crate) inherited: bool,
}

#[derive(Debug, Clone)]
struct LayerValues {
    layer: ConfigLayer,
    layer_name: String,
    source_name: String,
    values: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy)]
struct LayerLoad<'a> {
    layer: ConfigLayer,
    name: &'a str,
    content: &'a str,
}

impl<'a> LayerLoad<'a> {
    const fn inline(layer: ConfigLayer, name: &'a str, content: &'a str) -> Self {
        Self {
            layer,
            name,
            content,
        }
    }
}

pub(crate) fn load_layers(paths: &ConfigPaths) -> Result<LoadedLayers> {
    let mut resolver = ConfigResolver::new();
    let mut layer_values = Vec::new();
    let mut unknown_keys = Vec::new();
    add_layer(
        &mut resolver,
        &mut layer_values,
        &mut unknown_keys,
        &LayerLoad::inline(
            ConfigLayer::BuiltInDefaults,
            "built-in defaults",
            BUILT_IN_DEFAULTS,
        ),
    )?;
    for layer in [
        CliLayer::User,
        CliLayer::Org,
        CliLayer::Project,
        CliLayer::Dir,
    ] {
        for path in layer_files(paths, layer)? {
            let name = path.display().to_string();
            let content = fs::read_to_string(&path).into_diagnostic()?;
            add_layer(
                &mut resolver,
                &mut layer_values,
                &mut unknown_keys,
                &LayerLoad::inline(config_layer(layer), &name, &content),
            )?;
        }
    }
    Ok(LoadedLayers {
        resolver,
        unknown_keys,
        layer_values,
    })
}

pub(crate) fn layer_path(paths: &ConfigPaths, layer: CliLayer) -> PathBuf {
    match layer {
        CliLayer::Project => paths.project_dir.join("orchestraitor.toml"),
        CliLayer::User => paths.config_dir.join("user.toml"),
        CliLayer::Org => paths.config_dir.join("org.toml"),
        CliLayer::Dir => paths.config_dir.join("dir.toml"),
    }
}

pub(crate) fn layer_name(layer: CliLayer) -> &'static str {
    match layer {
        CliLayer::Project => "project",
        CliLayer::User => "user",
        CliLayer::Org => "org",
        CliLayer::Dir => "dir",
    }
}

fn add_layer(
    resolver: &mut ConfigResolver,
    layer_values: &mut Vec<LayerValues>,
    unknown_keys: &mut Vec<UnknownKey>,
    load: &LayerLoad<'_>,
) -> Result<()> {
    let report = orchestraitor_core::config::parse_toml_config(load.content)
        .map_err(|error| miette!("configuration parse failed: {}", error.structured().cause))?;
    for key in report.unknown_keys {
        unknown_keys.push(UnknownKey {
            source: load.name.to_string(),
            key,
        });
    }
    *resolver = resolver
        .clone()
        .with_toml(
            ConfigSource {
                layer: load.layer,
                name: load.name.to_string(),
            },
            load.content,
        )
        .map_err(|error| miette!("configuration parse failed: {}", error.structured().cause))?;
    layer_values.push(LayerValues {
        layer: load.layer,
        layer_name: format_config_layer(load.layer),
        source_name: load.name.to_string(),
        values: read_value_map_from_str(load.content)?,
    });
    Ok(())
}

fn layer_files(paths: &ConfigPaths, layer: CliLayer) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let primary = layer_path(paths, layer);
    if primary.exists() {
        files.push(primary);
    }
    let dir = layer_shard_dir(paths, layer);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error).into_diagnostic(),
    };
    for entry in entries {
        let path = entry.into_diagnostic()?.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) == Some("toml") {
            files.push(path);
        }
    }
    Ok(files)
}

fn reject_same_layer_conflicts(layers: &[LayerValues]) -> Result<()> {
    let mut seen: BTreeMap<(ConfigLayer, String), Vec<String>> = BTreeMap::new();
    for layer in layers {
        for key in layer.values.keys() {
            seen.entry((layer.layer, key.clone()))
                .or_default()
                .push(layer.source_name.clone());
        }
    }
    for ((_, key), names) in seen {
        if names.len() > 1 {
            bail!(
                "ambiguous configuration conflict for key `{}` from sources {:?}",
                key,
                names
            );
        }
    }
    Ok(())
}

fn layer_shard_dir(paths: &ConfigPaths, layer: CliLayer) -> PathBuf {
    match layer {
        CliLayer::Project => paths.project_dir.join("orchestraitor.d"),
        CliLayer::User => paths.config_dir.join("user.d"),
        CliLayer::Org => paths.config_dir.join("org.d"),
        CliLayer::Dir => paths.config_dir.join("dir.d"),
    }
}

const fn config_layer(layer: CliLayer) -> ConfigLayer {
    match layer {
        CliLayer::Project => ConfigLayer::Project,
        CliLayer::User => ConfigLayer::GlobalUser,
        CliLayer::Org => ConfigLayer::OrganizationTeam,
        CliLayer::Dir => ConfigLayer::DirectoryDomain,
    }
}

fn format_config_layer(layer: ConfigLayer) -> String {
    match layer {
        ConfigLayer::BuiltInDefaults => "built-in-defaults",
        ConfigLayer::PluginDefaults => "plugin-defaults",
        ConfigLayer::GlobalUser => "user",
        ConfigLayer::OrganizationTeam => "org",
        ConfigLayer::Project => "project",
        ConfigLayer::DirectoryDomain => "dir",
        ConfigLayer::TaskAgent => "task-agent",
        ConfigLayer::CliFlag => "cli-flag",
    }
    .to_string()
}

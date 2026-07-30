//! Dotted-key JSON value rendering and diffing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use miette::{IntoDiagnostic, Result};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct DiffEntry {
    pub(crate) key: String,
    pub(crate) before: String,
    pub(crate) after: String,
}

pub(crate) fn read_value_map(path: &Path) -> Result<BTreeMap<String, serde_json::Value>> {
    match fs::read_to_string(path) {
        Ok(content) => read_value_map_from_str(&content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error).into_diagnostic(),
    }
}

pub(crate) fn read_value_map_from_str(
    content: &str,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let value = toml::from_str::<serde_json::Value>(content).into_diagnostic()?;
    Ok(flatten_json(&value))
}

pub(crate) fn flatten_json(value: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    collect_json(None, value, &mut map);
    map
}

pub(crate) fn diff_entries(
    before: &BTreeMap<String, serde_json::Value>,
    after: &BTreeMap<String, serde_json::Value>,
) -> Vec<DiffEntry> {
    let keys = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|key| {
            let old = before.get(key);
            let new = after.get(key);
            (old != new).then(|| DiffEntry {
                key: key.clone(),
                before: old.map_or_else(|| "<unset>".to_string(), render_json_value),
                after: new.map_or_else(|| "<unset>".to_string(), render_json_value),
            })
        })
        .collect()
}

pub(crate) fn render_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn collect_json(
    prefix: Option<&str>,
    value: &serde_json::Value,
    map: &mut BTreeMap<String, serde_json::Value>,
) {
    if let serde_json::Value::Object(object) = value {
        for (key, child) in object {
            if child.is_null() {
                continue;
            }
            let path = prefix.map_or_else(|| key.clone(), |prefix| format!("{prefix}.{key}"));
            collect_json(Some(&path), child, map);
        }
        return;
    }
    if let Some(path) = prefix {
        map.insert(path.to_string(), value.clone());
    }
}

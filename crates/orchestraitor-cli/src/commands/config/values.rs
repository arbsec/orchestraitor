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

/// Composes a table-valued result for a dotted key that names a table rather
/// than a leaf: collects every entry under `prefix` and nests them into a
/// JSON object. Returns `None` when no entry lives under the prefix.
pub(crate) fn compose_prefix_value<V, F>(
    map: &BTreeMap<String, V>,
    prefix: &str,
    value_of: F,
) -> Option<serde_json::Value>
where
    F: Fn(&V) -> &serde_json::Value,
{
    let mut root = serde_json::Map::new();
    let dotted_prefix = format!("{prefix}.");
    for (key, entry) in map {
        let Some(suffix) = key.strip_prefix(&dotted_prefix) else {
            continue;
        };
        if suffix.is_empty() {
            // The prefix itself appears as a flattened entry; only an empty
            // table can reach compose here (leaves resolve via direct get).
            // An empty table composes to absent, never to a nested object.
            continue;
        }
        insert_nested(&mut root, suffix.split('.'), value_of(entry).clone());
    }
    (!root.is_empty()).then(|| serde_json::Value::Object(root))
}

fn insert_nested<'a>(
    target: &mut serde_json::Map<String, serde_json::Value>,
    mut path: impl Iterator<Item = &'a str>,
    value: serde_json::Value,
) {
    let Some(segment) = path.next() else {
        return;
    };
    if path.next().is_none() {
        target.insert(segment.to_string(), value);
        return;
    }
    let child = target
        .entry(segment.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let serde_json::Value::Object(child_map) = child {
        insert_nested(child_map, path, value);
        if child_map.is_empty() {
            target.remove(segment);
        }
    }
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

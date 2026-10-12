//! Format-preserving TOML document edits.

use std::fs;
use std::path::Path;

use miette::{IntoDiagnostic, Result, bail, miette};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

pub(crate) fn read_document(path: &Path) -> Result<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(content) => content.parse::<DocumentMut>().into_diagnostic(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(error).into_diagnostic(),
    }
}

pub(crate) fn write_document(path: &Path, document: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).into_diagnostic()?;
    }
    fs::write(path, document.to_string()).into_diagnostic()
}

pub(crate) fn set_key(document: &mut DocumentMut, key: &str, value: Value) -> Result<()> {
    let parts = key.split('.').collect::<Vec<_>>();
    let Some((field, parents)) = parts.split_last() else {
        bail!("config key cannot be empty");
    };
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for parent in parents {
        table = table
            .entry(parent)
            .or_insert(Item::Table(Table::default()))
            .as_table_like_mut()
            .ok_or_else(|| miette!("config key `{}` crosses a scalar value", key))?;
    }
    table.insert(field, Item::Value(value));
    Ok(())
}

pub(crate) fn remove_key(document: &mut DocumentMut, key: &str) -> Result<()> {
    let parts = key.split('.').collect::<Vec<_>>();
    let Some((field, parents)) = parts.split_last() else {
        bail!("config key cannot be empty");
    };
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for parent in parents {
        let Some(next) = table.get_mut(parent).and_then(Item::as_table_like_mut) else {
            return Ok(());
        };
        table = next;
    }
    table.remove(field);
    Ok(())
}

pub(crate) fn parse_cli_value(raw: &str) -> Value {
    let document = format!("value = {raw}");
    if let Ok(parsed) = document.parse::<DocumentMut>()
        && let Some(value) = parsed.get("value").and_then(Item::as_value)
    {
        return value.clone();
    }
    Value::from(raw)
}

pub(crate) fn schema_version(document: &DocumentMut) -> Option<String> {
    document
        .get("schema_version")
        .and_then(Item::as_str)
        .map(ToOwned::to_owned)
}

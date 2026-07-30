//! Tree-sitter symbol extraction for one blob.

use std::path::Path;

use orchestraitor_model::Digest;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

use crate::language::spec_for_path;
use crate::{
    ContextAge, ContextError, ContextIndex, LocationRange, ProvenanceEnvelope, SourceRef, SymbolId,
    SymbolKind, SymbolRecord,
};

/// Parses one source blob and inserts extracted symbols into the index.
pub(crate) fn parse_blob(
    index: &mut ContextIndex,
    path: &Path,
    bytes: &[u8],
    digest: &Digest,
) -> Result<(), ContextError> {
    let Some(spec) = spec_for_path(path).transpose()? else {
        return Ok(());
    };
    let mut parser = Parser::new();
    parser
        .set_language(&spec.language)
        .map_err(|_| ContextError::LanguageSetup {
            language: spec.name,
        })?;
    let Some(tree) = parser.parse(bytes, None) else {
        return Err(ContextError::LanguageSetup {
            language: spec.name,
        });
    };
    let query =
        Query::new(&spec.language, spec.query).map_err(|_| ContextError::LanguageSetup {
            language: spec.name,
        })?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(query_match) = matches.next() {
        if let Some(symbol) = symbol_from_match(path, bytes, digest, &query, query_match) {
            index.insert_symbol(symbol);
        }
    }
    Ok(())
}

fn symbol_from_match(
    path: &Path,
    bytes: &[u8],
    digest: &Digest,
    query: &Query,
    query_match: &tree_sitter::QueryMatch<'_, '_>,
) -> Option<SymbolRecord> {
    let mut name = None;
    let mut range = None;
    let mut kind = None;
    for capture in query_match.captures {
        let capture_name = query.capture_names().get(capture.index as usize)?;
        if *capture_name == "name" {
            name = capture.node.utf8_text(bytes).ok().map(str::to_owned);
        } else if let Some(symbol_kind) = kind_from_capture(capture_name) {
            range = Some(range_from_node(capture.node));
            kind = Some(symbol_kind);
        }
    }
    Some(build_symbol(path, bytes, digest, name?, range?, kind?))
}

fn build_symbol(
    path: &Path,
    bytes: &[u8],
    digest: &Digest,
    name: String,
    range: LocationRange,
    kind: SymbolKind,
) -> SymbolRecord {
    let source_ref = SourceRef(format!("{}:{}", path.display(), range.start_line));
    let provenance =
        ProvenanceEnvelope::repository_blob(digest.clone(), ContextAge::Current, source_ref);
    SymbolRecord {
        id: SymbolId(format!("{}:{}:{}", path.display(), range.start_line, name)),
        name,
        kind,
        path: path.to_path_buf(),
        range,
        signature: signature_for_range(bytes, range),
        blob_digest: digest.clone(),
        provenance,
    }
}

fn kind_from_capture(capture_name: &str) -> Option<SymbolKind> {
    if capture_name.ends_with("function") {
        Some(SymbolKind::Function)
    } else if capture_name.ends_with("method") {
        Some(SymbolKind::Method)
    } else if capture_name.ends_with("type") {
        Some(SymbolKind::Type)
    } else if capture_name.ends_with("module") {
        Some(SymbolKind::Module)
    } else if capture_name.ends_with("variable") {
        Some(SymbolKind::Variable)
    } else {
        None
    }
}

fn range_from_node(node: tree_sitter::Node<'_>) -> LocationRange {
    let start = node.start_position();
    let end = node.end_position();
    LocationRange {
        start_line: u32::try_from(start.row.saturating_add(1)).unwrap_or(u32::MAX),
        start_column: u32::try_from(start.column).unwrap_or(u32::MAX),
        end_line: u32::try_from(end.row.saturating_add(1)).unwrap_or(u32::MAX),
        end_column: u32::try_from(end.column).unwrap_or(u32::MAX),
    }
}

fn signature_for_range(bytes: &[u8], range: LocationRange) -> String {
    let source = String::from_utf8_lossy(bytes);
    let line_index = usize::try_from(range.start_line.saturating_sub(1)).unwrap_or(usize::MAX);
    source
        .lines()
        .nth(line_index)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned()
}

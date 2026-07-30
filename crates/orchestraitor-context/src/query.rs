//! Appendix E context query API.

use std::path::{Path, PathBuf};

use crate::{
    CallEdge, ContextError, ContextIndex, ContextItem, Excerpt, ProvenanceEnvelope,
    ReferenceRecord, SymbolId, SymbolKind, SymbolRecord,
};

/// Text-search hit with bounded excerpt and provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Repository-relative path.
    pub path: PathBuf,
    /// One-based line number.
    pub line: u32,
    /// Matching line text.
    pub text: String,
    /// Provenance envelope for the hit.
    pub provenance: ProvenanceEnvelope,
}

/// Query facade over a content-addressed [`ContextIndex`].
pub struct ContextQuery<'a> {
    index: &'a ContextIndex,
}

impl<'a> ContextQuery<'a> {
    /// Builds a query facade over an immutable index snapshot.
    #[must_use]
    pub const fn new(index: &'a ContextIndex) -> Self {
        Self { index }
    }

    /// Finds symbols by name with optional kind and path-scope filtering.
    #[must_use]
    pub fn find_symbol(
        &self,
        name: &str,
        kind: Option<SymbolKind>,
        scope: Option<&Path>,
    ) -> Vec<SymbolRecord> {
        self.index
            .symbols()
            .values()
            .filter(|symbol| symbol.name == name)
            .filter(|symbol| kind.is_none_or(|expected| symbol.kind == expected))
            .filter(|symbol| scope.is_none_or(|prefix| symbol.path.starts_with(prefix)))
            .cloned()
            .collect()
    }

    /// Returns the compact symbol signature for a symbol id.
    ///
    /// # Errors
    /// Returns [`ContextError::NotFound`] when `symbol_id` is absent.
    pub fn symbol_signature(&self, symbol_id: &SymbolId) -> Result<ContextItem, ContextError> {
        self.index
            .symbols()
            .get(symbol_id)
            .cloned()
            .map(ContextItem::Symbol)
            .ok_or_else(|| ContextError::NotFound {
                id: symbol_id.0.clone(),
            })
    }

    /// Finds textual references to a symbol, bounded by `limit` when provided.
    #[must_use]
    pub fn find_references(
        &self,
        symbol_id: &SymbolId,
        limit: Option<usize>,
    ) -> Vec<ReferenceRecord> {
        let iter = self
            .index
            .references()
            .iter()
            .filter(|reference| reference.target.as_ref() == Some(symbol_id))
            .cloned();
        match limit {
            Some(max) => iter.take(max).collect(),
            None => iter.collect(),
        }
    }

    /// Returns inbound call edges for a symbol.
    #[must_use]
    pub fn callers(&self, symbol_id: &SymbolId, limit: Option<usize>) -> Vec<CallEdge> {
        let iter = self
            .index
            .calls()
            .iter()
            .filter(|edge| &edge.callee == symbol_id)
            .cloned();
        match limit {
            Some(max) => iter.take(max).collect(),
            None => iter.collect(),
        }
    }

    /// Returns outbound call edges for a symbol.
    #[must_use]
    pub fn callees(&self, symbol_id: &SymbolId, limit: Option<usize>) -> Vec<CallEdge> {
        let iter = self
            .index
            .calls()
            .iter()
            .filter(|edge| &edge.caller == symbol_id)
            .cloned();
        match limit {
            Some(max) => iter.take(max).collect(),
            None => iter.collect(),
        }
    }

    /// Returns context items changed in the current index generation.
    #[must_use]
    pub fn recent_changes(&self, path_or_symbol: Option<&str>) -> Vec<ContextItem> {
        self.index
            .blobs()
            .values()
            .filter(|blob| matches!(blob.provenance.age, crate::ContextAge::Current))
            .filter(|blob| {
                path_or_symbol.is_none_or(|needle| blob.path.to_string_lossy().contains(needle))
            })
            .cloned()
            .map(ContextItem::Blob)
            .collect()
    }

    /// Searches indexed source text for a literal query.
    #[must_use]
    pub fn search_text(
        &self,
        query: &str,
        glob: Option<&str>,
        limit: Option<usize>,
    ) -> Vec<SearchHit> {
        let max = limit.unwrap_or(usize::MAX);
        let mut hits = Vec::new();
        for (path, blob) in self.index.blobs() {
            if glob.is_some_and(|suffix| !path.to_string_lossy().ends_with(suffix)) {
                continue;
            }
            let Some(bytes) = self.index.source_bytes(path) else {
                continue;
            };
            let source = String::from_utf8_lossy(bytes);
            for (line_index, line) in source.lines().enumerate() {
                if line.contains(query) {
                    hits.push(SearchHit {
                        path: path.clone(),
                        line: u32::try_from(line_index.saturating_add(1)).unwrap_or(u32::MAX),
                        text: line.to_owned(),
                        provenance: blob.provenance.clone(),
                    });
                    if hits.len() >= max {
                        return hits;
                    }
                }
            }
        }
        hits
    }

    /// Reads a bounded one-based line excerpt from an indexed source file.
    ///
    /// # Errors
    /// Returns [`ContextError::InvalidRange`] for empty or inverted ranges and
    /// [`ContextError::NotFound`] when the path is absent from the index.
    pub fn read_excerpt(
        &self,
        path: &Path,
        start_line: u32,
        end_line: u32,
    ) -> Result<Excerpt, ContextError> {
        if start_line == 0 || end_line < start_line {
            return Err(ContextError::InvalidRange {
                start_line,
                end_line,
            });
        }
        let blob = self
            .index
            .blobs()
            .get(path)
            .ok_or_else(|| ContextError::NotFound {
                id: path.display().to_string(),
            })?;
        let bytes = self
            .index
            .source_bytes(path)
            .ok_or_else(|| ContextError::NotFound {
                id: path.display().to_string(),
            })?;
        let source = String::from_utf8_lossy(bytes);
        let lines = source
            .lines()
            .enumerate()
            .filter_map(|(index, line)| excerpt_line(index, line, start_line, end_line))
            .collect::<Vec<_>>();
        Ok(Excerpt {
            path: path.to_path_buf(),
            start_line,
            end_line,
            text: lines.join("\n"),
            provenance: blob.provenance.clone(),
        })
    }
}

fn excerpt_line(index: usize, line: &str, start_line: u32, end_line: u32) -> Option<String> {
    let line_number = u32::try_from(index.saturating_add(1)).ok()?;
    if (start_line..=end_line).contains(&line_number) {
        Some(line.to_owned())
    } else {
        None
    }
}

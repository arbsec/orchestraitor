//! Appendix E context query API.

use std::path::{Path, PathBuf};

use crate::{
    BlobRecord, CallEdge, ContextError, ContextIndex, ContextItem, Excerpt, ProvenanceEnvelope,
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

/// Compact repository overview returned by [`ContextQuery::repository_summary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySummary {
    /// Number of distinct blobs indexed by content digest.
    pub blob_count: usize,
    /// Number of distinct paths currently visible in the index.
    pub path_count: usize,
    /// Number of indexed symbols.
    pub symbol_count: usize,
    /// Number of indexed references.
    pub reference_count: usize,
    /// Number of indexed call edges.
    pub call_edge_count: usize,
}

/// Bounded source body for a symbol, returned by [`ContextQuery::symbol_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolBody {
    /// Symbol id the body belongs to.
    pub symbol_id: SymbolId,
    /// Repository-relative path.
    pub path: PathBuf,
    /// First one-based line included in the body.
    pub start_line: u32,
    /// Last one-based line included in the body.
    pub end_line: u32,
    /// Body text, trim-truncated to `line_budget`.
    pub text: String,
    /// `line_budget` actually consumed (≤ requested).
    pub lines_used: u32,
    /// `true` when the returned body was truncated.
    pub truncated: bool,
    /// Provenance envelope for the body.
    pub provenance: ProvenanceEnvelope,
}

/// Test-reference entry returned by [`ContextQuery::related_tests`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedTest {
    /// Repository-relative path of the test file.
    pub path: PathBuf,
    /// One-based line of the test reference, if known.
    pub line: Option<u32>,
    /// Matching textual reference.
    pub text: String,
    /// Provenance envelope for the reference.
    pub provenance: ProvenanceEnvelope,
}

/// Compact diagnostic emitted by [`ContextQuery::diagnostics`].
///
/// The MVP indexer does not produce language-server diagnostics; this stub preserves
/// the public API surface so callers can structure their requests ahead of LSP
/// integration (§9.16). `severity` is reserved for the LSP-backed implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDiagnostic {
    /// Path or symbol the diagnostic is associated with.
    pub target: String,
    /// Diagnostic severity (e.g. `error`, `warning`, `note`).
    pub severity: String,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Optional source code location (one-based line).
    pub line: Option<u32>,
    /// Provenance envelope for the diagnostic.
    pub provenance: ProvenanceEnvelope,
}

/// Neighbouring context items returned by [`ContextQuery::expand_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedContext {
    /// Original context item identifier the expansion was anchored on.
    pub anchor: String,
    /// Expanded context items.
    pub items: Vec<ContextItem>,
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

    /// Returns a compact summary of the indexed repository.
    ///
    /// MVP stub: returns zeroed counts. LSP-backed implementation will fill
    /// these from the index once §9.16 wiring lands.
    #[must_use]
    pub fn repository_summary(&self) -> RepositorySummary {
        RepositorySummary {
            blob_count: 0,
            path_count: 0,
            symbol_count: 0,
            reference_count: 0,
            call_edge_count: 0,
        }
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

    /// Returns a bounded source body for a symbol, capped at `line_budget` lines.
    ///
    /// MVP stub: returns [`ContextError::NotFound`] until the bounded body reader
    /// is wired up. The stub keeps the public API surface stable so callers can
    /// structure their requests ahead of the LSP-backed implementation.
    ///
    /// # Errors
    /// Always returns [`ContextError::NotFound`] in this stub.
    pub fn symbol_body(
        &self,
        symbol_id: &SymbolId,
        _line_budget: Option<u32>,
    ) -> Result<SymbolBody, ContextError> {
        Err(ContextError::NotFound {
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

    /// Returns test-bearing references associated with a symbol.
    ///
    /// MVP stub: always returns an empty vector. The full implementation will
    /// scan `tests/` paths for references to the symbol once §9.16 LSP
    /// integration is in place.
    #[must_use]
    pub fn related_tests(&self, _symbol_id: &SymbolId) -> Vec<RelatedTest> {
        Vec::new()
    }

    /// Returns compact diagnostics for a path or symbol.
    ///
    /// MVP stub: always returns an empty vector. The full implementation will
    /// surface language-server diagnostics once §9.16 LSP integration lands.
    #[must_use]
    pub fn diagnostics(&self, _path_or_symbol: Option<&str>) -> Vec<ContextDiagnostic> {
        Vec::new()
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
        for (path, digest) in self.index.paths() {
            if glob.is_some_and(|suffix| !path.to_string_lossy().ends_with(suffix)) {
                continue;
            }
            let Some(bytes) = self.index.source_bytes(path) else {
                continue;
            };
            let source = String::from_utf8_lossy(bytes);
            for (line_index, line) in source.lines().enumerate() {
                if line.contains(query) {
                    let provenance = self.index.blob_for_digest(digest).map_or_else(
                        || {
                            ProvenanceEnvelope::repository_blob(
                                digest.clone(),
                                crate::ContextAge::ReusedUnchanged,
                                crate::SourceRef(path.display().to_string()),
                            )
                        },
                        |blob: &BlobRecord| blob.provenance.clone(),
                    );
                    hits.push(SearchHit {
                        path: path.clone(),
                        line: u32::try_from(line_index.saturating_add(1)).unwrap_or(u32::MAX),
                        text: line.to_owned(),
                        provenance,
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
        let digest = self
            .index
            .paths()
            .get(path)
            .ok_or_else(|| ContextError::NotFound {
                id: path.display().to_string(),
            })?
            .clone();
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
        let provenance = self.index.blob_for_digest(&digest).map_or_else(
            || {
                ProvenanceEnvelope::repository_blob(
                    digest.clone(),
                    crate::ContextAge::ReusedUnchanged,
                    crate::SourceRef(path.display().to_string()),
                )
            },
            |blob| blob.provenance.clone(),
        );
        Ok(Excerpt {
            path: path.to_path_buf(),
            start_line,
            end_line,
            text: lines.join("\n"),
            provenance,
        })
    }

    /// Expands a context item by surfacing directly connected items.
    ///
    /// MVP stub: always returns an empty expansion with the requested anchor.
    /// The full implementation will resolve symbols by id, path, or digest and
    /// surface their connected items once §9.16 LSP integration lands.
    #[must_use]
    pub fn expand_context(&self, context_item_id: &str) -> ExpandedContext {
        ExpandedContext {
            anchor: context_item_id.to_owned(),
            items: Vec::new(),
        }
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

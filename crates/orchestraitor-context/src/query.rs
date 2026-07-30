//! Appendix E context query API.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use orchestraitor_model::Digest;

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
    #[must_use]
    pub fn repository_summary(&self) -> RepositorySummary {
        RepositorySummary {
            blob_count: self.index.blobs().len(),
            path_count: self.index.paths().len(),
            symbol_count: self.index.symbols().len(),
            reference_count: self.index.references().len(),
            call_edge_count: self.index.calls().len(),
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
    /// When `line_budget` is `None`, the full body (`start_line..end_line`) is returned.
    ///
    /// # Errors
    /// Returns [`ContextError::NotFound`] when `symbol_id` is absent or its blob has no
    /// readable source bytes.
    pub fn symbol_body(
        &self,
        symbol_id: &SymbolId,
        line_budget: Option<u32>,
    ) -> Result<SymbolBody, ContextError> {
        let symbol = self
            .index
            .symbols()
            .get(symbol_id)
            .cloned()
            .ok_or_else(|| ContextError::NotFound {
                id: symbol_id.0.clone(),
            })?;
        let bytes =
            self.index
                .source_bytes(&symbol.path)
                .ok_or_else(|| ContextError::NotFound {
                    id: symbol.path.display().to_string(),
                })?;
        let source = String::from_utf8_lossy(bytes);
        let symbol_lines = source
            .lines()
            .enumerate()
            .skip(usize::try_from(symbol.range.start_line.saturating_sub(1)).unwrap_or(0))
            .take(
                usize::try_from(
                    symbol
                        .range
                        .end_line
                        .saturating_sub(symbol.range.start_line)
                        .saturating_add(1),
                )
                .unwrap_or(0),
            )
            .map(|(_, line)| line.to_owned())
            .collect::<Vec<_>>();
        let total_lines = u32::try_from(symbol_lines.len()).unwrap_or(u32::MAX);
        let requested = line_budget.unwrap_or(total_lines);
        let lines_used = total_lines.min(requested);
        let truncated = lines_used < total_lines;
        let text = symbol_lines
            .iter()
            .take(usize::try_from(lines_used).unwrap_or(0))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let start_line = symbol.range.start_line;
        let end_line = symbol
            .range
            .start_line
            .saturating_add(lines_used)
            .saturating_sub(1);
        Ok(SymbolBody {
            symbol_id: symbol.id.clone(),
            path: symbol.path.clone(),
            start_line,
            end_line,
            text,
            lines_used,
            truncated,
            provenance: symbol.provenance.clone(),
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
    /// The MVP baseline has no test-to-symbol mapping (§9.16 LSP integration is the
    /// long-term source). This stub scans the available references for those whose
    /// path contains `tests/` or ends in `_test.<ext>` / `test.<ext>` and reports any
    /// that mention the target symbol by name. Returns an empty vector when no such
    /// references exist.
    #[must_use]
    pub fn related_tests(&self, symbol_id: &SymbolId) -> Vec<RelatedTest> {
        let Some(symbol) = self.index.symbols().get(symbol_id) else {
            return Vec::new();
        };
        self.index
            .references()
            .iter()
            .filter(|reference| reference.target.as_ref() == Some(symbol_id))
            .filter(|reference| is_test_path(&reference.path))
            .map(|reference| RelatedTest {
                path: reference.path.clone(),
                line: Some(reference.range.start_line),
                text: reference.name.clone(),
                provenance: reference.provenance.clone(),
            })
            .chain(related_tests_from_source(self.index, symbol))
            .collect()
    }

    /// Returns compact diagnostics for a path or symbol.
    ///
    /// The MVP baseline does not produce language-server diagnostics. The stub scans
    /// for parse-related failures already recorded in the index by treating missing
    /// blobs for a path as a single `note` diagnostic (no false-positive errors).
    #[must_use]
    pub fn diagnostics(&self, path_or_symbol: Option<&str>) -> Vec<ContextDiagnostic> {
        let Some(needle) = path_or_symbol else {
            return Vec::new();
        };
        let probe_path = Path::new(needle);
        let symbols = self
            .index
            .symbols()
            .values()
            .filter(|symbol| {
                symbol.id.0 == needle
                    || symbol.name == needle
                    || symbol.path == probe_path
                    || symbol.path.to_string_lossy().contains(needle)
            })
            .collect::<Vec<_>>();
        if symbols.is_empty() {
            return Vec::new();
        }
        symbols
            .into_iter()
            .map(|symbol| ContextDiagnostic {
                target: symbol.id.0.clone(),
                severity: "note".to_owned(),
                message: "no language-server diagnostics available in baseline index".to_owned(),
                line: Some(symbol.range.start_line),
                provenance: symbol.provenance.clone(),
            })
            .collect()
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
    /// `context_item_id` accepts either a [`SymbolId`] inner string, a repository-relative
    /// path, or a blob digest. The expansion surfaces:
    ///
    /// - the symbol itself (when id resolves),
    /// - its blob (when path or digest resolves),
    /// - its references and outbound call edges.
    #[must_use]
    pub fn expand_context(&self, context_item_id: &str) -> ExpandedContext {
        let mut items: Vec<ContextItem> = Vec::new();
        let push_blob = |blob: &BlobRecord, items: &mut Vec<ContextItem>| {
            items.push(ContextItem::Blob(blob.clone()));
        };

        if let Some(symbol) = self
            .index
            .symbols()
            .values()
            .find(|symbol| symbol.id.0 == context_item_id)
            .cloned()
        {
            items.push(ContextItem::Symbol(symbol.clone()));
            for reference in self.index.references() {
                if reference.target.as_ref() == Some(&symbol.id) {
                    items.push(ContextItem::Reference(reference.clone()));
                }
            }
            for edge in self.index.calls() {
                if edge.caller == symbol.id {
                    items.push(ContextItem::CallEdge(edge.clone()));
                }
            }
            if let Some(blob) = self.index.blob_for_digest(&symbol.blob_digest) {
                push_blob(blob, &mut items);
            }
        } else if let Some(blob) = self
            .index
            .paths()
            .get(Path::new(context_item_id))
            .and_then(|digest| self.index.blob_for_digest(digest))
        {
            push_blob(blob, &mut items);
            for symbol in self.index.symbols().values() {
                if symbol.path == Path::new(context_item_id) {
                    items.push(ContextItem::Symbol(symbol.clone()));
                }
            }
            for reference in self.index.references() {
                if reference.path == Path::new(context_item_id) {
                    items.push(ContextItem::Reference(reference.clone()));
                }
            }
        } else if let Ok(digest) = Digest::from_str(context_item_id)
            && let Some(blob) = self.index.blob_for_digest(&digest)
        {
            push_blob(blob, &mut items);
        }

        items.sort_by_cached_key(context_item_sort_key);
        items.dedup();

        ExpandedContext {
            anchor: context_item_id.to_owned(),
            items,
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

fn is_test_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized.contains("/tests/")
        || normalized.contains("/test/")
        || normalized.contains("/__tests__/")
        || normalized.ends_with("_test.rs")
        || normalized.ends_with("test.rs")
        || normalized.ends_with("_test.py")
        || normalized.ends_with("test.py")
        || normalized.ends_with(".test.ts")
        || normalized.ends_with(".test.tsx")
        || normalized.ends_with(".spec.ts")
        || normalized.ends_with(".spec.tsx")
}

fn related_tests_from_source(index: &ContextIndex, symbol: &SymbolRecord) -> Vec<RelatedTest> {
    let mut results: Vec<RelatedTest> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for (path, digest) in index.paths() {
        if !is_test_path(path) {
            continue;
        }
        let Some(bytes) = index.source_bytes(path) else {
            continue;
        };
        let source = String::from_utf8_lossy(bytes);
        let lines = source.lines().enumerate();
        for (line_index, line) in lines {
            if line.contains(&symbol.name) {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let provenance = index.blob_for_digest(digest).map_or_else(
                    || {
                        ProvenanceEnvelope::repository_blob(
                            digest.clone(),
                            crate::ContextAge::ReusedUnchanged,
                            crate::SourceRef(path.display().to_string()),
                        )
                    },
                    |blob| blob.provenance.clone(),
                );
                results.push(RelatedTest {
                    path: path.clone(),
                    line: u32::try_from(line_index.saturating_add(1)).ok(),
                    text: line.to_owned(),
                    provenance,
                });
                break;
            }
        }
    }
    results
}

fn context_item_sort_key(item: &ContextItem) -> (u8, String) {
    match item {
        ContextItem::Symbol(_) => (0, String::new()),
        ContextItem::Blob(_) => (1, String::new()),
        ContextItem::Reference(_) => (2, String::new()),
        ContextItem::CallEdge(_) => (3, String::new()),
        ContextItem::Excerpt(_) => (4, String::new()),
    }
}

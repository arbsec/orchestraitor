//! Content-addressed repository index.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use orchestraitor_model::Digest;
use sha2::{Digest as ShaDigest, Sha256};

use crate::language::spec_for_path;
use crate::parser::parse_blob;
use crate::walker::{relative_path, repository_files};
use crate::{
    BlobRecord, CallEdge, ContextAge, ContextError, LocationRange, ProvenanceEnvelope,
    ReferenceRecord, SourceRef, SymbolId, SymbolRecord,
};

/// In-memory content-addressed repository context index.
#[derive(Debug, Clone, Default)]
pub struct ContextIndex {
    blobs: BTreeMap<PathBuf, BlobRecord>,
    bytes: BTreeMap<PathBuf, Vec<u8>>,
    symbols: BTreeMap<SymbolId, SymbolRecord>,
    references: Vec<ReferenceRecord>,
    calls: Vec<CallEdge>,
}

/// Summary of one indexing pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexReport {
    /// Number of blobs observed in the repository.
    pub observed_blobs: u64,
    /// Number of blobs newly parsed by tree-sitter in this pass.
    pub processed_blobs: u64,
    /// Number of blobs skipped because their digest was unchanged.
    pub skipped_unchanged_blobs: u64,
}

/// Repository indexer keyed by blob digest.
#[derive(Debug, Default)]
pub struct Indexer {
    index: ContextIndex,
}

impl ContextIndex {
    /// Returns all blob records indexed by repository-relative path.
    #[must_use]
    pub const fn blobs(&self) -> &BTreeMap<PathBuf, BlobRecord> {
        &self.blobs
    }

    /// Returns all symbol records indexed by symbol id.
    #[must_use]
    pub const fn symbols(&self) -> &BTreeMap<SymbolId, SymbolRecord> {
        &self.symbols
    }

    /// Returns all reference records.
    #[must_use]
    pub fn references(&self) -> &[ReferenceRecord] {
        &self.references
    }

    /// Returns all call edges.
    #[must_use]
    pub fn calls(&self) -> &[CallEdge] {
        &self.calls
    }

    /// Returns source bytes for a repository-relative path.
    #[must_use]
    pub fn source_bytes(&self, path: &Path) -> Option<&[u8]> {
        self.bytes.get(path).map(Vec::as_slice)
    }

    pub(crate) fn insert_symbol(&mut self, symbol: SymbolRecord) {
        self.symbols.insert(symbol.id.clone(), symbol);
    }
}

impl Indexer {
    /// Returns the current in-memory index.
    #[must_use]
    pub const fn index(&self) -> &ContextIndex {
        &self.index
    }

    /// Consumes the indexer and returns its current index.
    #[must_use]
    pub fn into_index(self) -> ContextIndex {
        self.index
    }

    /// Indexes a repository baseline and incrementally skips unchanged blobs on later calls.
    ///
    /// # Errors
    /// Returns [`ContextError`] when repository traversal or source reads fail.
    pub fn index_repository(&mut self, repo_path: &Path) -> Result<IndexReport, ContextError> {
        let _repo_discovery_marker = std::mem::size_of::<gix::Repository>();
        let mut report = IndexReport::default();
        let mut next = self.index.clone();
        for path in repository_files(repo_path)? {
            index_path(repo_path, &path, &mut next, &mut report)?;
        }
        next.rebuild_references_and_calls();
        self.index = next;
        Ok(report)
    }
}

impl ContextIndex {
    fn remove_path(&mut self, path: &Path) {
        self.symbols.retain(|_, symbol| symbol.path != path);
        self.references.retain(|reference| reference.path != path);
        self.calls.retain(|edge| {
            let caller_path = self
                .symbols
                .get(&edge.caller)
                .map(|symbol| symbol.path.as_path());
            let callee_path = self
                .symbols
                .get(&edge.callee)
                .map(|symbol| symbol.path.as_path());
            caller_path != Some(path) && callee_path != Some(path)
        });
    }

    fn rebuild_references_and_calls(&mut self) {
        self.references.clear();
        self.calls.clear();
        let symbols: Vec<SymbolRecord> = self.symbols.values().cloned().collect();
        for symbol in &symbols {
            for (path, bytes) in &self.bytes {
                let source = String::from_utf8_lossy(bytes);
                self.references
                    .extend(references_for_symbol(symbol, path, &source));
            }
        }
        for caller in &symbols {
            if let Some(bytes) = self.bytes.get(&caller.path) {
                let source = String::from_utf8_lossy(bytes);
                self.calls
                    .extend(calls_from_symbol(caller, &symbols, &source));
            }
        }
    }
}

fn index_path(
    repo_path: &Path,
    path: &Path,
    index: &mut ContextIndex,
    report: &mut IndexReport,
) -> Result<(), ContextError> {
    let relative = relative_path(repo_path, path)?;
    let bytes = fs::read(path).map_err(|source| ContextError::ReadSource {
        path: path.to_path_buf(),
        source,
    })?;
    let digest = digest_bytes(&bytes);
    report.observed_blobs = report.observed_blobs.saturating_add(1);
    let unchanged = index
        .blobs
        .get(&relative)
        .is_some_and(|blob| blob.digest == digest);
    let age = if unchanged {
        report.skipped_unchanged_blobs = report.skipped_unchanged_blobs.saturating_add(1);
        ContextAge::ReusedUnchanged
    } else {
        report.processed_blobs = report.processed_blobs.saturating_add(1);
        index.remove_path(&relative);
        parse_blob(index, &relative, &bytes, &digest)?;
        ContextAge::Current
    };
    upsert_blob(index, relative, bytes, digest, age);
    Ok(())
}

fn upsert_blob(
    index: &mut ContextIndex,
    relative: PathBuf,
    bytes: Vec<u8>,
    digest: Digest,
    age: ContextAge,
) {
    let source_ref = SourceRef(relative.display().to_string());
    let provenance = ProvenanceEnvelope::repository_blob(digest.clone(), age, source_ref);
    let language = spec_for_path(&relative)
        .and_then(Result::ok)
        .map(|spec| spec.kind);
    let process_count = u64::from(!matches!(age, ContextAge::ReusedUnchanged));
    index.blobs.insert(
        relative.clone(),
        BlobRecord {
            path: relative.clone(),
            digest,
            language,
            process_count,
            provenance,
        },
    );
    index.bytes.insert(relative, bytes);
}

fn digest_bytes(bytes: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let finalized = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in finalized {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Digest::new(hex)
}

fn references_for_symbol(symbol: &SymbolRecord, path: &Path, source: &str) -> Vec<ReferenceRecord> {
    let mut refs = Vec::new();
    for (line_index, line) in source.lines().enumerate() {
        for (column, _) in line.match_indices(&symbol.name) {
            refs.push(reference_record(symbol, path, line_index, column));
        }
    }
    refs
}

fn reference_record(
    symbol: &SymbolRecord,
    path: &Path,
    line_index: usize,
    column: usize,
) -> ReferenceRecord {
    let start_line = u32::try_from(line_index.saturating_add(1)).unwrap_or(u32::MAX);
    let start_column = u32::try_from(column).unwrap_or(u32::MAX);
    let end_column = u32::try_from(column.saturating_add(symbol.name.len())).unwrap_or(u32::MAX);
    ReferenceRecord {
        target: Some(symbol.id.clone()),
        name: symbol.name.clone(),
        path: path.to_path_buf(),
        range: LocationRange {
            start_line,
            start_column,
            end_line: start_line,
            end_column,
        },
        provenance: symbol.provenance.clone(),
    }
}

fn calls_from_symbol(
    caller: &SymbolRecord,
    symbols: &[SymbolRecord],
    source: &str,
) -> Vec<CallEdge> {
    let start = usize::try_from(caller.range.start_line.saturating_sub(1)).unwrap_or(usize::MAX);
    let len = usize::try_from(
        caller
            .range
            .end_line
            .saturating_sub(caller.range.start_line)
            .saturating_add(1),
    )
    .unwrap_or(usize::MAX);
    let body = source
        .lines()
        .skip(start)
        .take(len)
        .collect::<Vec<_>>()
        .join("\n");
    symbols
        .iter()
        .filter(|callee| callee.id != caller.id && body.contains(&format!("{}(", callee.name)))
        .map(|callee| CallEdge {
            caller: caller.id.clone(),
            callee: callee.id.clone(),
            provenance: caller.provenance.clone(),
        })
        .collect()
}

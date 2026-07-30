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
    /// Blobs indexed by SHA-256 content digest.
    blobs: BTreeMap<Digest, BlobRecord>,
    /// Raw source bytes keyed by content digest.
    bytes: BTreeMap<Digest, Vec<u8>>,
    /// Repository-relative paths currently observed, mapped to their blob digest.
    paths: BTreeMap<PathBuf, Digest>,
    /// Indexed symbols keyed by stable symbol id.
    symbols: BTreeMap<SymbolId, SymbolRecord>,
    /// Per-blob reference records.
    references: Vec<ReferenceRecord>,
    /// Call edges between indexed symbols.
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
    /// Returns all blob records indexed by content digest.
    #[must_use]
    pub const fn blobs(&self) -> &BTreeMap<Digest, BlobRecord> {
        &self.blobs
    }

    /// Returns all repository-relative paths currently visible in the index, mapped to their blob digest.
    #[must_use]
    pub const fn paths(&self) -> &BTreeMap<PathBuf, Digest> {
        &self.paths
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

    /// Returns source bytes for a repository-relative path by resolving through the digest map.
    #[must_use]
    pub fn source_bytes(&self, path: &Path) -> Option<&[u8]> {
        let digest = self.paths.get(path)?;
        self.bytes.get(digest).map(Vec::as_slice)
    }

    /// Returns the blob record associated with a content digest, if present.
    #[must_use]
    pub fn blob_for_digest(&self, digest: &Digest) -> Option<&BlobRecord> {
        self.blobs.get(digest)
    }

    pub(crate) fn insert_symbol(&mut self, symbol: SymbolRecord) {
        self.symbols.insert(symbol.id.clone(), symbol);
    }

    /// Drops the symbols, references, and stored bytes belonging to a digest without
    /// touching the path map. Caller is responsible for re-associating paths.
    fn purge_digest_data(&mut self, digest: &Digest) {
        self.symbols
            .retain(|_, symbol| symbol.blob_digest != *digest);
        self.references
            .retain(|reference| reference.blob_digest != *digest);
        self.calls.retain(|edge| {
            self.symbols.contains_key(&edge.caller) && self.symbols.contains_key(&edge.callee)
        });
        self.bytes.remove(digest);
        self.blobs.remove(digest);
    }

    /// Detaches a path from the index. If the path's digest has no remaining visible paths,
    /// the blob, its bytes, and any symbols it owned are removed.
    fn remove_path(&mut self, path: &Path) {
        let Some(digest) = self.paths.remove(path) else {
            return;
        };
        if self.paths.values().any(|existing| existing == &digest) {
            // Another path still references this digest; the blob and its symbols survive.
            return;
        }
        self.purge_digest_data(&digest);
    }

    /// Re-associates a path with a digest, removing the previous path entry for the same digest
    /// and any other path that pointed to the same digest.
    fn reassociate_path(&mut self, path: PathBuf, digest: &Digest) {
        let stale: Vec<PathBuf> = self
            .paths
            .iter()
            .filter(|(existing_path, existing_digest)| {
                *existing_digest == digest && *existing_path != &path
            })
            .map(|(existing_path, _)| existing_path.clone())
            .collect();
        for entry in stale {
            self.paths.remove(&entry);
        }
        self.paths.insert(path, digest.clone());
    }

    /// Removes paths that were observed in the previous index but are no longer in the
    /// freshly traversed repository, then purges any orphaned blobs.
    fn evict_orphaned_paths(&mut self, traversed: &[PathBuf], repo_root: &Path) {
        let traversed_set: Vec<PathBuf> = traversed
            .iter()
            .filter_map(|path| relative_path(repo_root, path).ok())
            .collect();
        let removed: Vec<PathBuf> = self
            .paths
            .keys()
            .filter(|existing| !traversed_set.contains(existing))
            .cloned()
            .collect();
        for path in removed {
            self.remove_path(&path);
        }
    }

    fn rebuild_references_and_calls(&mut self) {
        self.references.clear();
        self.calls.clear();
        let symbols: Vec<SymbolRecord> = self.symbols.values().cloned().collect();
        for symbol in &symbols {
            let Some(bytes) = self.bytes.get(&symbol.blob_digest) else {
                continue;
            };
            let Some(occurrence) = self.blobs.get(&symbol.blob_digest) else {
                continue;
            };
            let source = String::from_utf8_lossy(bytes);
            self.references
                .extend(references_for_symbol(symbol, occurrence, &source));
        }
        for caller in &symbols {
            let Some(bytes) = self.bytes.get(&caller.blob_digest) else {
                continue;
            };
            let source = String::from_utf8_lossy(bytes);
            self.calls
                .extend(calls_from_symbol(caller, &symbols, &source));
        }
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
    /// Blobs are keyed by SHA-256 content digest: a file move to a new path with the same
    /// content is recognised as a reuse, not a reparse. Paths present in the previous
    /// index but absent from the new traversal are removed.
    ///
    /// # Errors
    /// Returns [`ContextError`] when repository traversal or source reads fail.
    pub fn index_repository(&mut self, repo_path: &Path) -> Result<IndexReport, ContextError> {
        let _repo_discovery_marker = std::mem::size_of::<gix::Repository>();
        let mut report = IndexReport::default();
        let mut next = self.index.clone();
        let traversed = repository_files(repo_path)?;
        for path in &traversed {
            let relative = relative_path(repo_path, path)?;
            index_path(path, &relative, &mut next, &mut report)?;
        }
        next.evict_orphaned_paths(&traversed, repo_path);
        next.rebuild_references_and_calls();
        self.index = next;
        Ok(report)
    }
}

fn index_path(
    absolute_path: &Path,
    relative: &Path,
    index: &mut ContextIndex,
    report: &mut IndexReport,
) -> Result<(), ContextError> {
    let bytes = fs::read(absolute_path).map_err(|source| ContextError::ReadSource {
        path: absolute_path.to_path_buf(),
        source,
    })?;
    let digest = digest_bytes(&bytes);
    report.observed_blobs = report.observed_blobs.saturating_add(1);
    let age = classify_age(index, relative, &digest, report);
    upsert_blob(index, relative, &bytes, &digest, age)?;
    Ok(())
}

/// Decides whether a path needs a fresh parse, and updates the report counters.
fn classify_age(
    index: &mut ContextIndex,
    relative: &Path,
    digest: &Digest,
    report: &mut IndexReport,
) -> ContextAge {
    match index.paths.get(relative).cloned() {
        Some(previous) if &previous == digest => {
            report.skipped_unchanged_blobs = report.skipped_unchanged_blobs.saturating_add(1);
            ContextAge::ReusedUnchanged
        }
        Some(_) => {
            // Same path, content changed: drop the previous digest's data and parse again.
            index.remove_path(relative);
            if index.blobs.contains_key(digest) {
                // The new digest is already in the index under another path; treat as reuse.
                report.skipped_unchanged_blobs = report.skipped_unchanged_blobs.saturating_add(1);
                ContextAge::ReusedUnchanged
            } else {
                report.processed_blobs = report.processed_blobs.saturating_add(1);
                ContextAge::Current
            }
        }
        None => {
            if index.blobs.contains_key(digest) {
                // Content moved to a new path: reuse the blob, just re-anchor the path.
                report.skipped_unchanged_blobs = report.skipped_unchanged_blobs.saturating_add(1);
                ContextAge::ReusedUnchanged
            } else {
                report.processed_blobs = report.processed_blobs.saturating_add(1);
                ContextAge::Current
            }
        }
    }
}

fn upsert_blob(
    index: &mut ContextIndex,
    relative: &Path,
    bytes: &[u8],
    digest: &Digest,
    age: ContextAge,
) -> Result<(), ContextError> {
    let source_ref = SourceRef(relative.display().to_string());
    let provenance = ProvenanceEnvelope::repository_blob(digest.clone(), age, source_ref);
    let language = spec_for_path(relative)
        .and_then(Result::ok)
        .map(|spec| spec.kind);
    let process_count = u64::from(!matches!(age, ContextAge::ReusedUnchanged));
    let blob = BlobRecord {
        path: relative.to_path_buf(),
        digest: digest.clone(),
        language,
        process_count,
        provenance,
    };
    index.bytes.insert(digest.clone(), bytes.to_owned());
    index.blobs.insert(digest.clone(), blob);
    index.reassociate_path(relative.to_path_buf(), digest);
    if matches!(age, ContextAge::Current) {
        parse_blob(index, relative, bytes, digest)?;
    }
    Ok(())
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

fn references_for_symbol(
    symbol: &SymbolRecord,
    occurrence: &BlobRecord,
    source: &str,
) -> Vec<ReferenceRecord> {
    let mut refs = Vec::new();
    let definition_line = symbol.range.start_line.saturating_sub(1);
    for (line_index, line) in source.lines().enumerate() {
        if line_index == definition_line as usize {
            continue;
        }
        for (column, _) in line.match_indices(&symbol.name) {
            refs.push(reference_record(symbol, occurrence, line_index, column));
        }
    }
    refs
}

fn reference_record(
    symbol: &SymbolRecord,
    occurrence: &BlobRecord,
    line_index: usize,
    column: usize,
) -> ReferenceRecord {
    let start_line = u32::try_from(line_index.saturating_add(1)).unwrap_or(u32::MAX);
    let start_column = u32::try_from(column).unwrap_or(u32::MAX);
    let end_column = u32::try_from(column.saturating_add(symbol.name.len())).unwrap_or(u32::MAX);
    // TODO(spec §9.15): future revisions should key references by the
    // occurrence's content digest rather than via the symbol record, so a
    // reference's provenance stays anchored to the blob where it appears even
    // when the target symbol lives in a different blob.
    let source_ref = SourceRef(format!("{}:{}", occurrence.path.display(), start_line));
    let provenance = ProvenanceEnvelope::repository_blob(
        occurrence.digest.clone(),
        occurrence.provenance.age,
        source_ref,
    );
    ReferenceRecord {
        target: Some(symbol.id.clone()),
        name: symbol.name.clone(),
        path: occurrence.path.clone(),
        blob_digest: occurrence.digest.clone(),
        range: LocationRange {
            start_line,
            start_column,
            end_line: start_line,
            end_column,
        },
        provenance,
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

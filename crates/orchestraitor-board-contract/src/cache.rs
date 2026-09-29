//! Write-through read-cache wrapper (Ledger D5, spec `10-orchestrator.md`
//! §9.43 "Single canonical provider" / "Reconcile").
//!
//! Semantics:
//!
//! - **Single canonical provider.** Work items live ON the wrapped
//!   [`BoardProvider`]; the cache is never authoritative and never assigns
//!   identity.
//! - **Write-through.** Every mutation goes to the provider FIRST; only a
//!   successful provider write refreshes the cache from provider state
//!   (write-through, never write-behind).
//! - **`last_synced` stamps.** Cached reads carry the wall-clock time of the
//!   cache's most recent refresh from the provider, so stale reads are
//!   visible to consumers.
//! - **Board-wins reconcile.** After a write-through, the refresh step
//!   compares provider state with what the write assumed; any mismatch
//!   emits a [`CacheEvent::BoardDiverged`] event, and the provider's state
//!   wins — the cache refreshes to it.
//!
//! Runtime state (decisions, leases, heartbeats, events, receipts, budgets)
//! is local-only elsewhere, keyed by [`BoardItemId`] — it never syncs to the
//! board.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::BoardContractError;
use crate::provider::BoardProvider;
use crate::types::{
    BoardField, BoardFieldValue, BoardItem, BoardItemId, BoardItemType, BoardSearch, BoardStatus,
    CrossReference, DependencyEdge,
};

/// Milliseconds since the Unix epoch, best-effort (0 when the clock is
/// before the epoch; stamps only need to be monotonically comparable).
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

/// Events emitted by the write-through cache (Ledger D5).
///
/// Typed and observable: tests and the local event store match on the
/// variant and payload; board content is never embedded (spec §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CacheEvent {
    /// The provider's state after a write-through differs from what the
    /// write assumed: the board wins, the cache refreshes to provider
    /// state, and this event records the divergence.
    BoardDiverged {
        /// The item whose state diverged.
        item: BoardItemId,
        /// What the write-through assumed.
        expected: Divergence,
        /// What the provider actually holds.
        actual: Divergence,
    },
}

/// Structural description of one divergence axis, from ONE side of the
/// comparison (the event pairs `expected` vs `actual`). Values name the
/// axis and its plain data only — bodies (untrusted content, spec §6.1) are
/// compared by version, never embedded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "axis", rename_all = "snake_case")]
pub enum Divergence {
    /// Status: the status name one side holds.
    Status {
        /// The status name.
        status: String,
    },
    /// Field: the typed field value one side holds.
    Field {
        /// The field name.
        field: String,
        /// The typed value.
        value: BoardFieldValue,
    },
    /// Field cleared: one side holds no value for the field (an UNSET
    /// field is never substituted with the written value).
    FieldCleared {
        /// The field name.
        field: String,
    },
    /// Body version: the body version proxy one side holds (content itself
    /// is never embedded).
    Body {
        /// The body version proxy.
        version: u64,
    },
}

/// A cached read: the item as of `last_synced_ms`, plus its status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedItem {
    /// The provider's item data as of `last_synced_ms`.
    pub item: BoardItem,
    /// Milliseconds since the Unix epoch when this entry was last refreshed
    /// from the provider. Stale reads carry an older stamp; the cache is
    /// never authoritative (Ledger D5).
    pub last_synced_ms: u64,
}

/// Internal cache state. Locked briefly; poison maps to a typed transport
/// error, never a panic or unwrap.
#[derive(Default)]
struct CacheState {
    items: BTreeMap<BoardItemId, CachedItem>,
    /// Provider-observed body version per item, captured at the last
    /// refresh (the only version source: never an internal write counter).
    provider_body_versions: BTreeMap<BoardItemId, u64>,
    pending_events: Vec<CacheEvent>,
}

/// A write-through, board-wins read-cache over any [`BoardProvider`].
///
/// The cache implements [`BoardProvider`] itself, so it composes anywhere a
/// provider is expected while preserving the single-canonical-provider
/// invariant: mutations reach the wrapped provider first, then the cache
/// refreshes from provider state.
pub struct CachedBoard<P> {
    provider: P,
    state: Mutex<CacheState>,
    /// Monotonic generation, incremented on every refresh from the provider.
    refresh_generation: AtomicU64,
}

impl<P: BoardProvider> CachedBoard<P> {
    /// Wraps a canonical provider with the write-through cache.
    #[must_use]
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            state: Mutex::new(CacheState::default()),
            refresh_generation: AtomicU64::new(0),
        }
    }

    /// Returns the cache's view of one item with its last-synced stamp.
    /// The cache re-reads the provider on every call (refresh-on-read), so
    /// the returned state is fresh and the stamp always reflects the most
    /// recent provider contact; consumers should still treat the stamp as
    /// the read's authority provenance (Ledger D5: the cache is never
    /// authoritative).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when the provider does not know
    /// the id; [`BoardContractError::Transport`] on cache-lock poisoning.
    pub async fn cached_item(&self, id: &BoardItemId) -> Result<CachedItem, BoardContractError> {
        let item = self.provider.item(id).await?;
        let stamp = unix_millis();
        self.refresh_generation.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.lock("cache read")?;
            state.items.insert(
                id.clone(),
                CachedItem {
                    item: item.clone(),
                    last_synced_ms: stamp,
                },
            );
            state
                .provider_body_versions
                .insert(id.clone(), body_version(&item));
        }
        Ok(CachedItem {
            item,
            last_synced_ms: stamp,
        })
    }

    /// Returns the cache's last-synced stamp for one item, if cached.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] on cache-lock poisoning.
    pub fn last_synced(&self, id: &BoardItemId) -> Result<Option<u64>, BoardContractError> {
        Ok(self
            .lock("cache stamp read")?
            .items
            .get(id)
            .map(|cached| cached.last_synced_ms))
    }

    /// Drains and returns all cache events emitted since the last drain.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] on cache-lock poisoning.
    pub fn drain_events(&self) -> Result<Vec<CacheEvent>, BoardContractError> {
        let mut state = self.lock("cache event drain")?;
        Ok(std::mem::take(&mut state.pending_events))
    }

    /// The monotonic refresh generation, incremented on every refresh from
    /// the provider (proves refresh happens after write-through).
    #[must_use]
    pub fn refresh_generation(&self) -> u64 {
        self.refresh_generation.load(Ordering::Relaxed)
    }

    fn lock(
        &self,
        operation: &'static str,
    ) -> Result<MutexGuard<'_, CacheState>, BoardContractError> {
        self.state
            .lock()
            .map_err(|_| BoardContractError::Transport { operation })
    }

    /// Refreshes the cache entry for `id` from provider state and returns
    /// the fresh item.
    async fn refresh(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError> {
        let actual = self.provider.item(id).await?;
        let stamp = unix_millis();
        self.refresh_generation.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.lock("cache refresh")?;
            state.items.insert(
                id.clone(),
                CachedItem {
                    item: actual.clone(),
                    last_synced_ms: stamp,
                },
            );
            state
                .provider_body_versions
                .insert(id.clone(), body_version(&actual));
        }
        Ok(actual)
    }

    /// Records one divergence event and refreshes the cache to provider
    /// state (board wins). Lock poison maps to the same typed Transport
    /// error every other lock site uses — the event and the refresh are
    /// never silently dropped.
    fn record_divergence(
        &self,
        event: CacheEvent,
        actual: &BoardItem,
    ) -> Result<(), BoardContractError> {
        let stamp = unix_millis();
        {
            let mut state = self.lock("cache reconcile")?;
            state.pending_events.push(event);
            state.items.insert(
                actual.id.clone(),
                CachedItem {
                    item: actual.clone(),
                    last_synced_ms: stamp,
                },
            );
            state
                .provider_body_versions
                .insert(actual.id.clone(), body_version(actual));
        }
        self.refresh_generation.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// The provider-observed body version for `id`, if the cache has
    /// observed one.
    fn observed_body_version(&self, id: &BoardItemId) -> Option<u64> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.provider_body_versions.get(id).copied())
    }
}

/// Structural body version: bodies are untrusted content (spec §6.1), so
/// divergence is described by a version proxy, never by embedding content.
fn body_version(item: &BoardItem) -> u64 {
    body_len_version(item.body.len())
}

/// The observed-version proxy for a body of `length` bytes.
fn body_len_version(length: usize) -> u64 {
    u64::try_from(length).unwrap_or(u64::MAX)
}

/// The version the write assumes: the previously OBSERVED body version
/// adjusted by the write's own content delta. A clean write lands exactly
/// here; board-side drift between write and read-back moves the version
/// anywhere else.
fn expected_body_version(observed_before: u64, written_body: &str) -> u64 {
    // observed_before proxies the pre-write body length in bytes; the write
    // replaces that body with `written_body`.
    let before_len = usize::try_from(observed_before).unwrap_or(usize::MAX);
    let delta = isize::try_from(written_body.len())
        .ok()
        .and_then(|written| {
            isize::try_from(before_len)
                .ok()
                .map(|before| written - before)
        });
    match delta {
        Some(delta) if delta >= 0 => {
            observed_before.saturating_add(u64::try_from(delta).unwrap_or(u64::MAX))
        }
        Some(delta) => {
            observed_before.saturating_sub(u64::try_from(delta.unsigned_abs()).unwrap_or(u64::MAX))
        }
        None => body_len_version(written_body.len()),
    }
}

#[async_trait]
impl<P: BoardProvider> BoardProvider for CachedBoard<P> {
    async fn item(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError> {
        self.provider.item(id).await
    }

    async fn items(&self) -> Result<Vec<BoardItem>, BoardContractError> {
        self.provider.items().await
    }

    async fn create_item(
        &self,
        item_type: BoardItemType,
        title: &str,
        body: &str,
    ) -> Result<BoardItemId, BoardContractError> {
        // The provider is canonical and assigns identity; the cache never
        // mints ids (Ledger D5).
        self.provider.create_item(item_type, title, body).await
    }

    async fn update_item_body(
        &self,
        id: &BoardItemId,
        title: &str,
        body: &str,
    ) -> Result<(), BoardContractError> {
        // The baseline is the provider-observed body version from the last
        // refresh — an observed provider state, never an internal write
        // counter. Without a prior observation there is nothing to
        // reconcile against.
        let observed_before = self.observed_body_version(id);
        // Write-through FIRST: the provider write must succeed before the
        // cache changes (write-through, never write-behind — Ledger D5).
        self.provider.update_item_body(id, title, body).await?;
        // Refresh from provider state, then reconcile board-wins by
        // comparing the two OBSERVED provider states (before vs after the
        // write): a clean write changes the body exactly as written; board-
        // side drift between write and read-back changes it differently.
        let actual = self.refresh(id).await?;
        let observed_after = body_version(&actual);
        if let Some(before) = observed_before {
            // What the write assumed: the old observed body replaced by the
            // written body (version proxy over opaque content, spec §6.1).
            let expected_version = expected_body_version(before, body);
            if observed_after != expected_version {
                self.record_divergence(
                    CacheEvent::BoardDiverged {
                        item: id.clone(),
                        expected: Divergence::Body {
                            version: expected_version,
                        },
                        actual: Divergence::Body {
                            version: observed_after,
                        },
                    },
                    &actual,
                )?;
            }
        }
        Ok(())
    }

    async fn statuses(&self) -> Result<Vec<BoardStatus>, BoardContractError> {
        self.provider.statuses().await
    }

    async fn set_item_status(
        &self,
        id: &BoardItemId,
        status: &str,
    ) -> Result<(), BoardContractError> {
        self.provider.set_item_status(id, status).await?;
        // Refresh: if the provider's post-write state disagrees with the
        // write (board-side drift landed between write and read-back), the
        // board wins and the divergence is recorded.
        let actual = self.refresh(id).await?;
        if actual.status != status {
            self.record_divergence(
                CacheEvent::BoardDiverged {
                    item: id.clone(),
                    expected: Divergence::Status {
                        status: status.to_string(),
                    },
                    actual: Divergence::Status {
                        status: actual.status.clone(),
                    },
                },
                &actual,
            )?;
        }
        Ok(())
    }

    async fn fields(&self) -> Result<Vec<BoardField>, BoardContractError> {
        self.provider.fields().await
    }

    async fn field_value(
        &self,
        id: &BoardItemId,
        field: &str,
    ) -> Result<Option<BoardFieldValue>, BoardContractError> {
        self.provider.field_value(id, field).await
    }

    async fn set_field_value(
        &self,
        id: &BoardItemId,
        field: &str,
        value: BoardFieldValue,
    ) -> Result<(), BoardContractError> {
        self.provider
            .set_field_value(id, field, value.clone())
            .await?;
        let actual = self.refresh(id).await?;
        let provider_value = self.provider.field_value(id, field).await?;
        if provider_value.as_ref() != Some(&value) {
            // Report the provider's actual state honestly: a cleared field
            // is UNSET (FieldCleared), never substituted with the written
            // value.
            let actual_divergence = match provider_value {
                Some(actual_value) => Divergence::Field {
                    field: field.to_string(),
                    value: actual_value,
                },
                None => Divergence::FieldCleared {
                    field: field.to_string(),
                },
            };
            self.record_divergence(
                CacheEvent::BoardDiverged {
                    item: id.clone(),
                    expected: Divergence::Field {
                        field: field.to_string(),
                        value,
                    },
                    actual: actual_divergence,
                },
                &actual,
            )?;
        }
        Ok(())
    }

    async fn dependency_edges(&self) -> Result<Vec<DependencyEdge>, BoardContractError> {
        self.provider.dependency_edges().await
    }

    async fn add_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        self.provider.add_dependency_edge(blocked, blocks).await
    }

    async fn remove_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        self.provider.remove_dependency_edge(blocked, blocks).await
    }

    async fn cross_references(
        &self,
        id: &BoardItemId,
    ) -> Result<Vec<CrossReference>, BoardContractError> {
        self.provider.cross_references(id).await
    }

    async fn add_cross_reference(
        &self,
        from: &BoardItemId,
        to: &str,
    ) -> Result<(), BoardContractError> {
        self.provider.add_cross_reference(from, to).await
    }

    async fn search(&self, filter: &BoardSearch) -> Result<Vec<BoardItem>, BoardContractError> {
        self.provider.search(filter).await
    }
}

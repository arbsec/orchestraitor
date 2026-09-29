//! Conformance + cache-semantics integration tests (issue #318, spec §9.43,
//! Ledger D5): the in-memory reference provider passes the reusable
//! conformance suite; the write-through cache is proven to never be
//! authoritative; board-wins reconcile emits `board-diverged` events.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror the CLI test harness: a failed assertion must
// fail the test loudly.

mod conformance;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use conformance::run_conformance;
use orchestraitor_board_contract::{
    BoardContractError, BoardFieldKind, BoardFieldValue, BoardItem, BoardItemId, BoardItemType,
    BoardProvider, BoardSearch, BoardSetup, CacheEvent, CachedBoard, Divergence,
    InMemoryBoardProvider,
};

/// A canonical board shaped for the conformance suite: two statuses and
/// three typed fields (priority single-select, points number, notes text).
fn conformance_board() -> InMemoryBoardProvider {
    InMemoryBoardProvider::new(|setup: &mut BoardSetup<'_>| {
        setup
            .status("Todo")
            .status("Ready")
            .field("priority", BoardFieldKind::SingleSelect)
            .field("points", BoardFieldKind::Number)
            .field("notes", BoardFieldKind::Text)
            .item(
                "epic-1",
                BoardItemType::Epic,
                "Board abstraction epic",
                "epic body",
                "Todo",
                &[],
            )
            .item(
                "task-1",
                BoardItemType::Task,
                "Ship the contract",
                "task body",
                "Ready",
                &[("points", BoardFieldValue::Number { value: 5 })],
            )
            .item(
                "bug-1",
                BoardItemType::Bug,
                "Cache drift bug",
                "bug body",
                "Todo",
                &[(
                    "priority",
                    BoardFieldValue::SingleSelect {
                        option: "P0".to_string(),
                    },
                )],
            )
            .edge("task-1", "epic-1");
    })
}

#[tokio::test]
async fn in_memory_provider_passes_full_conformance() {
    let provider = conformance_board();
    run_conformance(&provider)
        .await
        .unwrap_or_else(|failure| panic!("conformance failed: {failure}"));
}

#[tokio::test]
async fn item_id_rejects_empty() {
    assert!(matches!(
        BoardItemId::new(""),
        Err(BoardContractError::InvalidItemId)
    ));
    assert!(BoardItemId::new("PVTI_1").is_ok());
}

// ---------------------------------------------------------------------------
// D5: cache-authority negative tests
// ---------------------------------------------------------------------------

/// The canonical provider the cache wraps, plus a divergence injector: tests
/// flip provider state DIRECTLY (simulating board-side drift) and observe
/// the cache's reconcile.
struct DivergentBoard {
    inner: InMemoryBoardProvider,
    /// When set, the next read of this item reports this status instead of
    /// the real one (board-side drift between write and read-back).
    drift: Mutex<Option<(BoardItemId, String)>>,
}

impl DivergentBoard {
    fn new(setup: impl FnOnce(&mut BoardSetup<'_>)) -> Self {
        Self {
            inner: InMemoryBoardProvider::new(setup),
            drift: Mutex::new(None),
        }
    }

    fn inject_status_drift(&self, id: &BoardItemId, status: &str) {
        *self.drift.lock().unwrap() = Some((id.clone(), status.to_string()));
    }
}

#[async_trait]
impl BoardProvider for DivergentBoard {
    async fn item(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError> {
        let mut item = self.inner.item(id).await?;
        let drifted_status = self.drift.lock().ok().and_then(|guard| {
            guard
                .as_ref()
                .filter(|(drift_id, _)| drift_id == id)
                .map(|(_, status)| status.clone())
        });
        if let Some(status) = drifted_status {
            item.status = status;
        }
        Ok(item)
    }

    async fn items(&self) -> Result<Vec<BoardItem>, BoardContractError> {
        self.inner.items().await
    }

    async fn create_item(
        &self,
        item_type: BoardItemType,
        title: &str,
        body: &str,
    ) -> Result<BoardItemId, BoardContractError> {
        self.inner.create_item(item_type, title, body).await
    }

    async fn update_item_body(
        &self,
        id: &BoardItemId,
        title: &str,
        body: &str,
    ) -> Result<(), BoardContractError> {
        self.inner.update_item_body(id, title, body).await
    }

    async fn statuses(
        &self,
    ) -> Result<Vec<orchestraitor_board_contract::BoardStatus>, BoardContractError> {
        self.inner.statuses().await
    }

    async fn set_item_status(
        &self,
        id: &BoardItemId,
        status: &str,
    ) -> Result<(), BoardContractError> {
        self.inner.set_item_status(id, status).await
    }

    async fn fields(
        &self,
    ) -> Result<Vec<orchestraitor_board_contract::BoardField>, BoardContractError> {
        self.inner.fields().await
    }

    async fn field_value(
        &self,
        id: &BoardItemId,
        field: &str,
    ) -> Result<Option<BoardFieldValue>, BoardContractError> {
        self.inner.field_value(id, field).await
    }

    async fn set_field_value(
        &self,
        id: &BoardItemId,
        field: &str,
        value: BoardFieldValue,
    ) -> Result<(), BoardContractError> {
        self.inner.set_field_value(id, field, value).await
    }

    async fn dependency_edges(
        &self,
    ) -> Result<Vec<orchestraitor_board_contract::DependencyEdge>, BoardContractError> {
        self.inner.dependency_edges().await
    }

    async fn add_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        self.inner.add_dependency_edge(blocked, blocks).await
    }

    async fn remove_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        self.inner.remove_dependency_edge(blocked, blocks).await
    }

    async fn cross_references(
        &self,
        id: &BoardItemId,
    ) -> Result<Vec<orchestraitor_board_contract::CrossReference>, BoardContractError> {
        self.inner.cross_references(id).await
    }

    async fn add_cross_reference(
        &self,
        from: &BoardItemId,
        to: &str,
    ) -> Result<(), BoardContractError> {
        self.inner.add_cross_reference(from, to).await
    }

    async fn search(&self, filter: &BoardSearch) -> Result<Vec<BoardItem>, BoardContractError> {
        self.inner.search(filter).await
    }
}

fn drifted_board() -> DivergentBoard {
    DivergentBoard::new(|setup: &mut BoardSetup<'_>| {
        setup
            .status("Todo")
            .status("Ready")
            .field("points", BoardFieldKind::Number)
            .item(
                "task-1",
                BoardItemType::Task,
                "Drift probe",
                "body-v1",
                "Todo",
                &[],
            );
    })
}

/// NEGATIVE (write-behind detector): after a cache write, a DIRECT provider
/// read must already reflect the write. Write-through, never write-behind.
#[tokio::test]
async fn cache_write_is_visible_on_direct_provider_read() {
    let board = drifted_board();
    let cache = CachedBoard::new(&board);
    let item = BoardItemId::new("task-1").unwrap();
    cache
        .set_item_status(&item, "Ready")
        .await
        .expect("write-through status");
    // Read through the RAW provider, bypassing the cache entirely.
    let direct = board.inner.item(&item).await.expect("direct provider read");
    assert_eq!(
        direct.status, "Ready",
        "provider must hold the write immediately: cache wrote behind"
    );
    assert_eq!(
        cache.refresh_generation(),
        1,
        "cache must refresh from the provider after the write-through"
    );
}

/// Stale reads carry a last-synced stamp, and the stamp updates on refresh.
#[tokio::test]
async fn last_synced_stamp_present_and_updates_on_refresh() {
    let board = drifted_board();
    let cache = CachedBoard::new(&board);
    let item = BoardItemId::new("task-1").unwrap();
    let first = cache.cached_item(&item).await.expect("first cached read");
    assert!(
        first.last_synced_ms > 0,
        "last-synced stamp must be present on cached reads"
    );
    // A second read refreshes: the generation advances; the stamp stays
    // >= the first (wall clock may or may not have ticked).
    let second = cache.cached_item(&item).await.expect("second cached read");
    assert!(
        second.last_synced_ms >= first.last_synced_ms,
        "stamps must be monotonically comparable"
    );
    assert!(cache.refresh_generation() >= 2);
    assert_eq!(
        cache.last_synced(&item).expect("stamp lookup"),
        Some(second.last_synced_ms),
        "cache must expose the latest last-synced stamp"
    );
}

/// BOARD-DIVERGED: the provider returns divergent state after a local write
/// — assert the typed `board-diverged` EVENT with correct payload, and that
/// the board won (the cache refreshed to provider state).
#[tokio::test]
async fn board_wins_reconcile_emits_board_diverged_event() {
    let board = drifted_board();
    let item = BoardItemId::new("task-1").unwrap();
    // Seed the cache's view of the pre-write status.
    let cache = CachedBoard::new(&board);
    cache.cached_item(&item).await.expect("seed cache");

    // Inject board-side drift FIRST: the cache's write-through then lands
    // "Ready" on the provider, but its read-back observes the injected
    // "In Review" — divergence between what the write assumed and what the
    // provider reports.
    board.inject_status_drift(&item, "In Review");
    cache
        .set_item_status(&item, "Ready")
        .await
        .expect("write-through with drift");

    // Exercise the reconcile path through the cache write API.
    let events = cache.drain_events().expect("drain events");
    assert!(
        events.iter().any(
            |event| matches!(event, CacheEvent::BoardDiverged { item: diverged, .. }
                if *diverged == item)
        ),
        "expected a board-diverged event for the drifted item, got {events:?}"
    );
}

/// The board-diverged event carries the correct expected/actual payload and
/// the cache refreshes to provider state (board wins, not just "no crash").
#[tokio::test]
async fn board_diverged_event_payload_and_board_wins_refresh() {
    let board = drifted_board();
    let item = BoardItemId::new("task-1").unwrap();
    let cache = CachedBoard::new(&board);

    // The provider's set_item_status succeeds writing "Ready", but the
    // subsequent read-back observes the injected "In Review".
    board.inject_status_drift(&item, "In Review");
    cache
        .set_item_status(&item, "Ready")
        .await
        .expect("write-through");

    let events = cache.drain_events().expect("drain events");
    let diverged = events
        .iter()
        .find_map(|event| match event {
            CacheEvent::BoardDiverged {
                item: diverged,
                expected,
                actual,
            } if *diverged == item => Some((expected, actual)),
            CacheEvent::BoardDiverged { .. } => None,
        })
        .unwrap_or_else(|| panic!("board-diverged event missing, got {events:?}"));
    assert_eq!(
        diverged.0,
        &Divergence::Status {
            status: "Ready".to_string()
        },
        "expected side must record the status the write assumed"
    );
    assert_eq!(
        diverged.1,
        &Divergence::Status {
            status: "In Review".to_string()
        },
        "actual side must record the provider's divergent status"
    );
    // Board wins: the cache's refreshed view matches the provider.
    let cached = cache
        .cached_item(&item)
        .await
        .expect("cached read after reconcile");
    assert_eq!(
        cached.item.status, "In Review",
        "cache must refresh to provider state (board wins)"
    );
}

/// The provider accepts the write but its read-back reports a different
/// typed value (simulated board-side field change).
struct FieldDriftProvider {
    inner: InMemoryBoardProvider,
    drift_to: AtomicU64,
}

#[async_trait]
impl BoardProvider for FieldDriftProvider {
    async fn item(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError> {
        self.inner.item(id).await
    }
    async fn items(&self) -> Result<Vec<BoardItem>, BoardContractError> {
        self.inner.items().await
    }
    async fn create_item(
        &self,
        item_type: BoardItemType,
        title: &str,
        body: &str,
    ) -> Result<BoardItemId, BoardContractError> {
        self.inner.create_item(item_type, title, body).await
    }
    async fn update_item_body(
        &self,
        id: &BoardItemId,
        title: &str,
        body: &str,
    ) -> Result<(), BoardContractError> {
        self.inner.update_item_body(id, title, body).await
    }
    async fn statuses(
        &self,
    ) -> Result<Vec<orchestraitor_board_contract::BoardStatus>, BoardContractError> {
        self.inner.statuses().await
    }
    async fn set_item_status(
        &self,
        id: &BoardItemId,
        status: &str,
    ) -> Result<(), BoardContractError> {
        self.inner.set_item_status(id, status).await
    }
    async fn fields(
        &self,
    ) -> Result<Vec<orchestraitor_board_contract::BoardField>, BoardContractError> {
        self.inner.fields().await
    }
    async fn field_value(
        &self,
        id: &BoardItemId,
        field: &str,
    ) -> Result<Option<BoardFieldValue>, BoardContractError> {
        let drift = self.drift_to.load(Ordering::Relaxed);
        if drift > 0 {
            return Ok(Some(BoardFieldValue::Number { value: drift }));
        }
        self.inner.field_value(id, field).await
    }
    async fn set_field_value(
        &self,
        id: &BoardItemId,
        field: &str,
        value: BoardFieldValue,
    ) -> Result<(), BoardContractError> {
        self.inner.set_field_value(id, field, value).await
    }
    async fn dependency_edges(
        &self,
    ) -> Result<Vec<orchestraitor_board_contract::DependencyEdge>, BoardContractError> {
        self.inner.dependency_edges().await
    }
    async fn add_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        self.inner.add_dependency_edge(blocked, blocks).await
    }
    async fn remove_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        self.inner.remove_dependency_edge(blocked, blocks).await
    }
    async fn cross_references(
        &self,
        id: &BoardItemId,
    ) -> Result<Vec<orchestraitor_board_contract::CrossReference>, BoardContractError> {
        self.inner.cross_references(id).await
    }
    async fn add_cross_reference(
        &self,
        from: &BoardItemId,
        to: &str,
    ) -> Result<(), BoardContractError> {
        self.inner.add_cross_reference(from, to).await
    }
    async fn search(&self, filter: &BoardSearch) -> Result<Vec<BoardItem>, BoardContractError> {
        self.inner.search(filter).await
    }
}

/// Field writes also reconcile board-wins with a typed event.
#[tokio::test]
async fn field_divergence_emits_typed_event() {
    let drifted = FieldDriftProvider {
        inner: InMemoryBoardProvider::new(|setup: &mut BoardSetup<'_>| {
            setup
                .status("Todo")
                .field("points", BoardFieldKind::Number)
                .item(
                    "task-1",
                    BoardItemType::Task,
                    "Field probe",
                    "",
                    "Todo",
                    &[],
                );
        }),
        drift_to: AtomicU64::new(9),
    };
    let cache = CachedBoard::new(&drifted);
    let item = BoardItemId::new("task-1").unwrap();
    cache
        .set_field_value(&item, "points", BoardFieldValue::Number { value: 5 })
        .await
        .expect("write-through field");
    let events = cache.drain_events().expect("drain events");
    let diverged = events
        .iter()
        .find_map(|event| match event {
            CacheEvent::BoardDiverged {
                item: diverged,
                expected,
                actual,
            } if *diverged == item => Some((expected, actual)),
            CacheEvent::BoardDiverged { .. } => None,
        })
        .unwrap_or_else(|| panic!("board-diverged event missing, got {events:?}"));
    assert_eq!(
        diverged.0,
        &Divergence::Field {
            field: "points".to_string(),
            value: BoardFieldValue::Number { value: 5 },
        },
        "expected side must record the typed value the write assumed"
    );
    assert_eq!(
        diverged.1,
        &Divergence::Field {
            field: "points".to_string(),
            value: BoardFieldValue::Number { value: 9 },
        },
        "actual side must record the provider's typed value"
    );
}

/// The event serializes with the contract's `board-diverged` shape (the
/// local event store keys on it).
#[test]
fn board_diverged_event_serializes_typed() {
    let event = CacheEvent::BoardDiverged {
        item: BoardItemId::new("task-1").unwrap(),
        expected: Divergence::Status {
            status: "Ready".to_string(),
        },
        actual: Divergence::Status {
            status: "In Review".to_string(),
        },
    };
    // The serde tag derives from the variant name in snake_case; assert the
    // typed round-trip that the local event store relies on.
    let encoded = serde_json::to_string(&event).unwrap();
    assert!(
        encoded.contains("board_diverged"),
        "serialized event must carry the board-diverged kind, got {encoded}"
    );
    let round_trip: CacheEvent = serde_json::from_str(&encoded).unwrap();
    assert_eq!(round_trip, event, "typed event must round-trip");
}

/// A provider write failure leaves the cache untouched (no cache-first
/// authority): the provider error propagates and no refresh fires.
#[tokio::test]
async fn failed_provider_write_never_touches_cache() {
    let board = drifted_board();
    let cache = CachedBoard::new(&board);
    let missing = BoardItemId::new("nonexistent-item").unwrap();
    let before = cache.refresh_generation();
    let result = cache.set_item_status(&missing, "Ready").await;
    assert!(
        matches!(result, Err(BoardContractError::ItemNotFound { .. })),
        "provider refusal must surface as a typed error"
    );
    assert_eq!(
        cache.refresh_generation(),
        before,
        "failed write must not refresh the cache: the provider is canonical"
    );
}

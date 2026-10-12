//! Deterministic in-memory reference provider — the conformance target for
//! the [`BoardProvider`] contract (spec §9.43).
//!
//! No I/O, no network, no clock: the same operation sequence always yields
//! the same state, satisfying the deterministic-simulator rule (spec
//! `50-contracts-data.md` §21.3). Future providers (GitHub, sqlite) are
//! validated against the same conformance suite via this reference.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::BoardContractError;
use crate::provider::BoardProvider;
use crate::types::{
    BoardField, BoardFieldKind, BoardFieldValue, BoardItem, BoardItemId, BoardItemType,
    BoardSearch, BoardStatus, CrossReference, DependencyEdge,
};

/// A deterministic, in-memory board.
#[derive(Default)]
struct BoardState {
    items: BTreeMap<BoardItemId, BoardItem>,
    statuses: Vec<BoardStatus>,
    fields: Vec<BoardField>,
    field_values: BTreeMap<(BoardItemId, String), BoardFieldValue>,
    edges: BTreeSet<DependencyEdge>,
    cross_references: BTreeMap<BoardItemId, Vec<CrossReference>>,
    /// Monotonic counter for provider-assigned ids.
    next_id: u64,
}

/// The in-memory reference [`BoardProvider`].
///
/// All shared state sits behind one mutex so the provider is `Send + Sync`;
/// operations are short and non-blocking (no I/O), so lock contention
/// cannot starve the async runtime. Poison is mapped to a typed transport
/// error, never an `unwrap`.
pub struct InMemoryBoardProvider {
    state: Mutex<BoardState>,
}

impl InMemoryBoardProvider {
    /// Creates a provider with the given initial catalog. The setup closure
    /// runs once, before the provider is shared.
    #[must_use]
    pub fn new(setup: impl FnOnce(&mut BoardSetup<'_>)) -> Self {
        let mut state = BoardState::default();
        setup(&mut BoardSetup { state: &mut state });
        Self {
            state: Mutex::new(state),
        }
    }

    fn lock(
        &self,
        operation: &'static str,
    ) -> Result<std::sync::MutexGuard<'_, BoardState>, BoardContractError> {
        self.state
            .lock()
            .map_err(|_| BoardContractError::Transport { operation })
    }
}

/// Setup builder handed to [`InMemoryBoardProvider::new`].
pub struct BoardSetup<'a> {
    state: &'a mut BoardState,
}

impl BoardSetup<'_> {
    /// Adds a status column and returns its name for later reference.
    pub fn status(&mut self, name: &str) -> &mut Self {
        let id = BoardItemId::new(format!("status-{}", self.state.statuses.len()))
            .unwrap_or_else(|_| BoardItemId::unchecked(String::from("status")));
        self.state.statuses.push(BoardStatus {
            id,
            name: name.to_string(),
        });
        self
    }

    /// Adds a typed custom field and returns its name for later reference.
    pub fn field(&mut self, name: &str, kind: BoardFieldKind) -> &mut Self {
        let id = BoardItemId::new(format!("field-{}", self.state.fields.len()))
            .unwrap_or_else(|_| BoardItemId::unchecked(String::from("field")));
        self.state.fields.push(BoardField {
            id,
            name: name.to_string(),
            kind,
        });
        self
    }

    /// Adds an item already placed in a status, with typed field values.
    pub fn item(
        &mut self,
        id: &str,
        item_type: BoardItemType,
        title: &str,
        body: &str,
        status: &str,
        fields: &[(&str, BoardFieldValue)],
    ) -> &mut Self {
        let item_id =
            BoardItemId::new(id).unwrap_or_else(|_| BoardItemId::unchecked(String::from(id)));
        self.state.items.insert(
            item_id.clone(),
            BoardItem {
                id: item_id.clone(),
                item_type,
                title: title.to_string(),
                body: body.to_string(),
                status: status.to_string(),
            },
        );
        for (field, value) in fields {
            self.state
                .field_values
                .insert((item_id.clone(), (*field).to_string()), value.clone());
        }
        self
    }

    /// Adds a native `blockedBy` edge.
    pub fn edge(&mut self, blocked: &str, blocks: &str) -> &mut Self {
        let Ok(blocked_id) = BoardItemId::new(blocked) else {
            return self;
        };
        let Ok(blocks_id) = BoardItemId::new(blocks) else {
            return self;
        };
        self.state.edges.insert(DependencyEdge {
            blocked: blocked_id,
            blocks: blocks_id,
        });
        self
    }
}

#[async_trait]
impl BoardProvider for InMemoryBoardProvider {
    async fn item(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError> {
        let state = self.lock("item read")?;
        state
            .items
            .get(id)
            .cloned()
            .ok_or_else(|| BoardContractError::ItemNotFound { id: id.to_string() })
    }

    async fn items(&self) -> Result<Vec<BoardItem>, BoardContractError> {
        let state = self.lock("items read")?;
        Ok(state.items.values().cloned().collect())
    }

    async fn create_item(
        &self,
        item_type: BoardItemType,
        title: &str,
        body: &str,
    ) -> Result<BoardItemId, BoardContractError> {
        let mut state = self.lock("item create")?;
        state.next_id += 1;
        let id = BoardItemId::new(format!("item-{}", state.next_id))?;
        state.items.insert(
            id.clone(),
            BoardItem {
                id: id.clone(),
                item_type,
                title: title.to_string(),
                body: body.to_string(),
                status: String::new(),
            },
        );
        Ok(id)
    }

    async fn update_item_body(
        &self,
        id: &BoardItemId,
        title: &str,
        body: &str,
    ) -> Result<(), BoardContractError> {
        let mut state = self.lock("item update")?;
        let item = state
            .items
            .get_mut(id)
            .ok_or_else(|| BoardContractError::ItemNotFound { id: id.to_string() })?;
        item.title = title.to_string();
        item.body = body.to_string();
        Ok(())
    }

    async fn statuses(&self) -> Result<Vec<BoardStatus>, BoardContractError> {
        let state = self.lock("statuses read")?;
        Ok(state.statuses.clone())
    }

    async fn set_item_status(
        &self,
        id: &BoardItemId,
        status: &str,
    ) -> Result<(), BoardContractError> {
        let mut state = self.lock("status write")?;
        if !state.items.contains_key(id) {
            return Err(BoardContractError::ItemNotFound { id: id.to_string() });
        }
        if !state.statuses.iter().any(|known| known.name == status) {
            return Err(BoardContractError::StatusNotFound {
                name: status.to_string(),
            });
        }
        if let Some(item) = state.items.get_mut(id) {
            item.status = status.to_string();
        }
        Ok(())
    }

    async fn fields(&self) -> Result<Vec<BoardField>, BoardContractError> {
        let state = self.lock("fields read")?;
        Ok(state.fields.clone())
    }

    async fn field_value(
        &self,
        id: &BoardItemId,
        field: &str,
    ) -> Result<Option<BoardFieldValue>, BoardContractError> {
        let state = self.lock("field value read")?;
        if !state.items.contains_key(id) {
            return Err(BoardContractError::ItemNotFound { id: id.to_string() });
        }
        if !state.fields.iter().any(|known| known.name == field) {
            return Err(BoardContractError::FieldNotFound {
                name: field.to_string(),
            });
        }
        Ok(state
            .field_values
            .get(&(id.clone(), field.to_string()))
            .cloned())
    }

    async fn set_field_value(
        &self,
        id: &BoardItemId,
        field: &str,
        value: BoardFieldValue,
    ) -> Result<(), BoardContractError> {
        let mut state = self.lock("field value write")?;
        if !state.items.contains_key(id) {
            return Err(BoardContractError::ItemNotFound { id: id.to_string() });
        }
        let known = state
            .fields
            .iter()
            .find(|candidate| candidate.name == field)
            .ok_or_else(|| BoardContractError::FieldNotFound {
                name: field.to_string(),
            })?;
        if known.kind != value.kind() {
            return Err(BoardContractError::FieldTypeMismatch {
                field: field.to_string(),
                expected: kind_name(known.kind),
                actual: kind_name(value.kind()),
            });
        }
        state
            .field_values
            .insert((id.clone(), field.to_string()), value);
        Ok(())
    }

    async fn dependency_edges(&self) -> Result<Vec<DependencyEdge>, BoardContractError> {
        let state = self.lock("edges read")?;
        Ok(state.edges.iter().cloned().collect())
    }

    async fn add_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        let mut state = self.lock("edge add")?;
        for id in [blocked, blocks] {
            if !state.items.contains_key(id) {
                return Err(BoardContractError::ItemNotFound { id: id.to_string() });
            }
        }
        let edge = DependencyEdge {
            blocked: blocked.clone(),
            blocks: blocks.clone(),
        };
        if !state.edges.insert(edge) {
            return Err(BoardContractError::DuplicateEdge {
                blocked: blocked.to_string(),
                blocks: blocks.to_string(),
            });
        }
        Ok(())
    }

    async fn remove_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        let mut state = self.lock("edge remove")?;
        for id in [blocked, blocks] {
            if !state.items.contains_key(id) {
                return Err(BoardContractError::ItemNotFound { id: id.to_string() });
            }
        }
        state.edges.remove(&DependencyEdge {
            blocked: blocked.clone(),
            blocks: blocks.clone(),
        });
        Ok(())
    }

    async fn cross_references(
        &self,
        id: &BoardItemId,
    ) -> Result<Vec<CrossReference>, BoardContractError> {
        let state = self.lock("cross-references read")?;
        if !state.items.contains_key(id) {
            return Err(BoardContractError::ItemNotFound { id: id.to_string() });
        }
        Ok(state.cross_references.get(id).cloned().unwrap_or_default())
    }

    async fn add_cross_reference(
        &self,
        from: &BoardItemId,
        to: &str,
    ) -> Result<(), BoardContractError> {
        let mut state = self.lock("cross-reference add")?;
        if !state.items.contains_key(from) {
            return Err(BoardContractError::ItemNotFound {
                id: from.to_string(),
            });
        }
        let references = state.cross_references.entry(from.clone()).or_default();
        if !references.iter().any(|reference| reference.to == to) {
            references.push(CrossReference {
                from: from.clone(),
                to: to.to_string(),
            });
        }
        Ok(())
    }

    async fn search(&self, filter: &BoardSearch) -> Result<Vec<BoardItem>, BoardContractError> {
        let state = self.lock("search")?;
        let mut matches = Vec::new();
        for item in state.items.values() {
            if filter
                .item_type
                .is_some_and(|wanted| item.item_type != wanted)
            {
                continue;
            }
            if filter
                .status
                .as_ref()
                .is_some_and(|wanted| item.status != *wanted)
            {
                continue;
            }
            if filter.fields.iter().all(|(name, value)| {
                state.field_values.get(&(item.id.clone(), name.clone())) == Some(value)
            }) {
                matches.push(item.clone());
            }
        }
        Ok(matches)
    }
}

/// The stable-name string for a field kind, used in mismatch errors.
const fn kind_name(kind: BoardFieldKind) -> &'static str {
    match kind {
        BoardFieldKind::SingleSelect => "single-select",
        BoardFieldKind::Text => "text",
        BoardFieldKind::Number => "number",
        BoardFieldKind::Date => "date",
    }
}

//! The [`BoardProvider`] trait: the six contract areas of spec
//! `10-orchestrator.md` §9.43 as one async object-safe surface.
//!
//! Work items live ON the provider — the provider is the single canonical
//! authority per workspace, never dual-master (Ledger D5). Every mutation
//! goes through the provider first; read-caches refresh from provider state.

use async_trait::async_trait;

use crate::error::BoardContractError;
use crate::types::{
    BoardField, BoardFieldValue, BoardItem, BoardItemId, BoardItemType, BoardSearch, BoardStatus,
    CrossReference, DependencyEdge,
};

/// The kanban board contract (spec `10-orchestrator.md` §9.43).
///
/// Six areas, each with typed errors via [`BoardContractError`]:
///
/// 1. **items** — tasks, bugs, epics, features with stable identity, type,
///    title, body. Bodies are opaque untrusted data (§6.1): the contract
///    never parses them.
/// 2. **statuses** — board columns / status field values.
/// 3. **fields** — typed custom fields with typed values, not stringly.
/// 4. **dependency edges** — native `blockedBy` edges, cross-board within a
///    workspace (§9.42). The board's edges ARE the dependency graph (§9.40):
///    edges are read and written here, never mirrored into a second graph.
/// 5. **cross-references** — links between items, including cross-repository
///    links.
/// 6. **search** — typed, conjunctive filters over items and fields (not a
///    query language).
///
/// A workspace has exactly one canonical provider; switching is an explicit
/// `orc board import/export` migration, not a live mirror. Implementations
/// must be deterministic or honestly I/O-backed, but always provider-side
/// authoritative: a local read-cache never becomes a second master.
#[async_trait]
pub trait BoardProvider: Send + Sync {
    /// Returns one item by stable id.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when the id is unknown.
    async fn item(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError>;

    /// Returns all items visible to the workspace.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] when the backing store or transport
    /// fails.
    async fn items(&self) -> Result<Vec<BoardItem>, BoardContractError>;

    /// Creates an item on the provider and returns its assigned identity.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::WriteRejected`] when the provider refuses the
    /// creation; [`BoardContractError::Transport`] on backing failure.
    async fn create_item(
        &self,
        item_type: BoardItemType,
        title: &str,
        body: &str,
    ) -> Result<BoardItemId, BoardContractError>;

    /// Replaces the item's title and body (opaque untrusted data, §6.1).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when the id is unknown;
    /// [`BoardContractError::WriteRejected`] / [`BoardContractError::Transport`]
    /// on provider refusal or backing failure.
    async fn update_item_body(
        &self,
        id: &BoardItemId,
        title: &str,
        body: &str,
    ) -> Result<(), BoardContractError>;

    /// Returns the board's statuses (columns / status field values).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] when the backing store or transport
    /// fails.
    async fn statuses(&self) -> Result<Vec<BoardStatus>, BoardContractError>;

    /// Moves an item to a status by exact name.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] / [`BoardContractError::StatusNotFound`]
    /// when either side is unknown; [`BoardContractError::WriteRejected`] /
    /// [`BoardContractError::Transport`] on provider refusal or backing
    /// failure.
    async fn set_item_status(
        &self,
        id: &BoardItemId,
        status: &str,
    ) -> Result<(), BoardContractError>;

    /// Returns the board's typed custom fields.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] when the backing store or transport
    /// fails.
    async fn fields(&self) -> Result<Vec<BoardField>, BoardContractError>;

    /// Returns the typed value of one field on one item, if set.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] / [`BoardContractError::FieldNotFound`]
    /// when either side is unknown.
    async fn field_value(
        &self,
        id: &BoardItemId,
        field: &str,
    ) -> Result<Option<BoardFieldValue>, BoardContractError>;

    /// Sets a typed field value on an item (typed, never stringly).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::FieldTypeMismatch`] when the value does not
    /// satisfy the field's domain; [`BoardContractError::ItemNotFound`] /
    /// [`BoardContractError::FieldNotFound`] when either side is unknown;
    /// [`BoardContractError::WriteRejected`] / [`BoardContractError::Transport`]
    /// on provider refusal or backing failure.
    async fn set_field_value(
        &self,
        id: &BoardItemId,
        field: &str,
        value: BoardFieldValue,
    ) -> Result<(), BoardContractError>;

    /// Returns the native `blockedBy` edges visible to the workspace
    /// (§9.40: the board's edges ARE the dependency graph).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] when the backing store or transport
    /// fails.
    async fn dependency_edges(&self) -> Result<Vec<DependencyEdge>, BoardContractError>;

    /// Adds a native `blockedBy` edge (`blocked` is delayed until `blocks`
    /// completes); cross-board within a workspace (§9.42).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when either endpoint is unknown;
    /// [`BoardContractError::DuplicateEdge`] when the edge already exists;
    /// [`BoardContractError::WriteRejected`] / [`BoardContractError::Transport`]
    /// on provider refusal or backing failure.
    async fn add_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError>;

    /// Removes a native `blockedBy` edge.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when either endpoint is unknown.
    async fn remove_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError>;

    /// Returns the item's cross-references (including cross-repository
    /// links).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when the id is unknown.
    async fn cross_references(
        &self,
        id: &BoardItemId,
    ) -> Result<Vec<CrossReference>, BoardContractError>;

    /// Adds a cross-reference from an item to a referenced resource.
    ///
    /// # Errors
    ///
    /// [`BoardContractError::ItemNotFound`] when the id is unknown;
    /// [`BoardContractError::WriteRejected`] / [`BoardContractError::Transport`]
    /// on provider refusal or backing failure.
    async fn add_cross_reference(
        &self,
        from: &BoardItemId,
        to: &str,
    ) -> Result<(), BoardContractError>;

    /// Runs a typed, conjunctive search over items and fields (§9.43
    /// `search`).
    ///
    /// # Errors
    ///
    /// [`BoardContractError::Transport`] when the backing store or transport
    /// fails.
    async fn search(&self, filter: &BoardSearch) -> Result<Vec<BoardItem>, BoardContractError>;
}

#[async_trait]
impl<P: BoardProvider + ?Sized> BoardProvider for &P {
    async fn item(&self, id: &BoardItemId) -> Result<BoardItem, BoardContractError> {
        (**self).item(id).await
    }

    async fn items(&self) -> Result<Vec<BoardItem>, BoardContractError> {
        (**self).items().await
    }

    async fn create_item(
        &self,
        item_type: BoardItemType,
        title: &str,
        body: &str,
    ) -> Result<BoardItemId, BoardContractError> {
        (**self).create_item(item_type, title, body).await
    }

    async fn update_item_body(
        &self,
        id: &BoardItemId,
        title: &str,
        body: &str,
    ) -> Result<(), BoardContractError> {
        (**self).update_item_body(id, title, body).await
    }

    async fn statuses(&self) -> Result<Vec<BoardStatus>, BoardContractError> {
        (**self).statuses().await
    }

    async fn set_item_status(
        &self,
        id: &BoardItemId,
        status: &str,
    ) -> Result<(), BoardContractError> {
        (**self).set_item_status(id, status).await
    }

    async fn fields(&self) -> Result<Vec<BoardField>, BoardContractError> {
        (**self).fields().await
    }

    async fn field_value(
        &self,
        id: &BoardItemId,
        field: &str,
    ) -> Result<Option<BoardFieldValue>, BoardContractError> {
        (**self).field_value(id, field).await
    }

    async fn set_field_value(
        &self,
        id: &BoardItemId,
        field: &str,
        value: BoardFieldValue,
    ) -> Result<(), BoardContractError> {
        (**self).set_field_value(id, field, value).await
    }

    async fn dependency_edges(&self) -> Result<Vec<DependencyEdge>, BoardContractError> {
        (**self).dependency_edges().await
    }

    async fn add_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        (**self).add_dependency_edge(blocked, blocks).await
    }

    async fn remove_dependency_edge(
        &self,
        blocked: &BoardItemId,
        blocks: &BoardItemId,
    ) -> Result<(), BoardContractError> {
        (**self).remove_dependency_edge(blocked, blocks).await
    }

    async fn cross_references(
        &self,
        id: &BoardItemId,
    ) -> Result<Vec<CrossReference>, BoardContractError> {
        (**self).cross_references(id).await
    }

    async fn add_cross_reference(
        &self,
        from: &BoardItemId,
        to: &str,
    ) -> Result<(), BoardContractError> {
        (**self).add_cross_reference(from, to).await
    }

    async fn search(&self, filter: &BoardSearch) -> Result<Vec<BoardItem>, BoardContractError> {
        (**self).search(filter).await
    }
}

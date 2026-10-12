//! Typed errors for the `BoardProvider` contract.
//!
//! Messages are structural: they name the failing operation and identifier
//! class only. Board content (titles, bodies, field values) is untrusted
//! input (spec §6.1) and never appears in error text; errors never embed
//! credentials (spec §9.23.4).

use thiserror::Error;

/// Failure classes of the [`BoardProvider`](crate::BoardProvider) contract
/// (spec §9.43), one variant per class.
#[derive(Debug, Error)]
pub enum BoardContractError {
    /// The item id was empty. Ids are provider-assigned and must be
    /// non-empty to key runtime state (Ledger D5).
    #[error("board item id must be non-empty")]
    InvalidItemId,

    /// The referenced item does not exist on the provider.
    #[error("board item not found: {id}")]
    ItemNotFound {
        /// The missing item id.
        id: String,
    },

    /// The referenced status does not exist on the provider.
    #[error("board status not found: {name}")]
    StatusNotFound {
        /// The missing status name.
        name: String,
    },

    /// The referenced field does not exist on the provider.
    #[error("board field not found: {name}")]
    FieldNotFound {
        /// The missing field name.
        name: String,
    },

    /// The value did not satisfy the field's typed domain (spec §9.43
    /// `fields`: typed values, not stringly).
    #[error("field `{field}` expects {expected} values, got {actual}")]
    FieldTypeMismatch {
        /// The field the value was written to.
        field: String,
        /// The field's value domain.
        expected: &'static str,
        /// The supplied value's domain.
        actual: &'static str,
    },

    /// The item already carries the referenced dependency edge (spec §9.40:
    /// the board's edges are the graph; duplicates add nothing).
    #[error("dependency edge {blocked} blocked by {blocks} already exists")]
    DuplicateEdge {
        /// The blocked item.
        blocked: String,
        /// The blocking item.
        blocks: String,
    },

    /// The referenced cross-reference does not exist on the item.
    #[error("cross-reference from {from} to {to} not found")]
    CrossReferenceNotFound {
        /// The referencing item.
        from: String,
        /// The referenced resource.
        to: String,
    },

    /// The provider rejected a mutation (read-only provider, frozen board,
    /// permission boundary).
    #[error("board provider rejected the write: {operation}")]
    WriteRejected {
        /// The rejected operation, structurally named.
        operation: &'static str,
    },

    /// The provider's backing transport or store failed.
    #[error("board provider transport failed: {operation}")]
    Transport {
        /// The failed operation, structurally named.
        operation: &'static str,
    },
}

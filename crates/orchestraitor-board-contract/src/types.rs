//! Contract data types: item identity, item types, statuses, typed field
//! values, dependency edges, cross-references, and search filters (spec
//! `10-orchestrator.md` §9.43).
//!
//! Item titles and bodies are untrusted board content (spec §6.1): they are
//! carried as opaque payload data — never parsed, never executed, never
//! interpolated into commands. The contract exposes them verbatim.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::BoardContractError;

/// Stable board item identity (spec §9.43 "stable board item identity").
///
/// A provider-assigned opaque identifier newtyped over `String`. Runtime
/// state elsewhere (decisions, leases, heartbeats, events, receipts,
/// budgets) keys by this id and never syncs to the board (Ledger D5).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BoardItemId(String);

impl BoardItemId {
    /// Creates a validated item id.
    ///
    /// # Errors
    ///
    /// Returns [`BoardContractError::InvalidItemId`] when the id is empty
    /// or whitespace-only (ids are provider-assigned and must be non-empty
    /// to key runtime state and search results).
    pub fn new(raw: impl Into<String>) -> Result<Self, BoardContractError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(BoardContractError::InvalidItemId);
        }
        Ok(Self(raw))
    }

    /// Creates an item id without validation. Crate-internal: only the
    /// deterministic setup builders and tests use it, and they always pass
    /// non-empty literals.
    pub(crate) fn unchecked(raw: String) -> Self {
        Self(raw)
    }

    /// The opaque id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BoardItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The four contract item classes (spec §9.43: tasks, bugs, epics, features).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardItemType {
    /// A leaf unit of work.
    Task,
    /// A defect report.
    Bug,
    /// A decomposition parent grouping features and leaf items.
    Epic,
    /// A deliverable scope grouping tasks and bugs.
    Feature,
}

impl BoardItemType {
    /// The lowercase contract name (`task`, `bug`, `epic`, `feature`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Bug => "bug",
            Self::Epic => "epic",
            Self::Feature => "feature",
        }
    }
}

impl fmt::Display for BoardItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A board item: the contract's unit of work (spec §9.43 `items`).
///
/// `title` and `body` are untrusted board content (spec §6.1): opaque data,
/// never parsed by the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardItem {
    /// Stable provider-assigned identity.
    pub id: BoardItemId,
    /// Item class.
    pub item_type: BoardItemType,
    /// Title (untrusted text; inert payload data).
    pub title: String,
    /// Body (untrusted content; opaque data — the contract MUST NOT parse it).
    pub body: String,
    /// Current status (column / status field value name); the board sees
    /// only coarse status transitions (spec §9.43).
    pub status: String,
}

/// A board column / status field value (spec §9.43 `statuses`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoardStatus {
    /// Provider-stable status identifier (option node id, column key, ...).
    pub id: BoardItemId,
    /// Human-readable column / option name (untrusted text; inert data).
    pub name: String,
}

/// A typed custom field defined on the board (spec §9.43 `fields`:
/// priority, target, risk, size, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoardField {
    /// Provider-stable field identifier.
    pub id: BoardItemId,
    /// Field name (untrusted text; inert data).
    pub name: String,
    /// The field's value domain.
    pub kind: BoardFieldKind,
}

/// The typed value domain of a [`BoardField`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardFieldKind {
    /// Single-select: one of the field's option names.
    SingleSelect,
    /// Free text (opaque data, spec §6.1).
    Text,
    /// Unsigned number.
    Number,
    /// Calendar date (`YYYY-MM-DD`).
    Date,
}

/// A typed field value set on an item — never a bare string (spec §9.43
/// `fields`: typed values, not stringly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BoardFieldValue {
    /// A selected option of a single-select field.
    SingleSelect {
        /// The selected option name (untrusted text; inert data).
        option: String,
    },
    /// Free text (opaque data).
    Text {
        /// The text value.
        value: String,
    },
    /// A number.
    Number {
        /// The numeric value.
        value: u64,
    },
    /// A calendar date (`YYYY-MM-DD`).
    Date {
        /// The date string.
        value: String,
    },
}

impl BoardFieldValue {
    /// The field kind this value satisfies.
    #[must_use]
    pub const fn kind(&self) -> BoardFieldKind {
        match self {
            Self::SingleSelect { .. } => BoardFieldKind::SingleSelect,
            Self::Text { .. } => BoardFieldKind::Text,
            Self::Number { .. } => BoardFieldKind::Number,
            Self::Date { .. } => BoardFieldKind::Date,
        }
    }
}

/// A native `blockedBy` dependency edge (spec §9.43 `dependency edges`,
/// §9.40: the board's edges ARE the dependency graph — no mirrored second
/// graph).
///
/// `blocked` is delayed until `blocks` completes. Edges may cross boards
/// within a workspace (§9.42).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DependencyEdge {
    /// The blocked item.
    pub blocked: BoardItemId,
    /// The item blocking it.
    pub blocks: BoardItemId,
}

/// A cross-reference between items, including cross-repository links
/// (spec §9.43 `cross-references`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CrossReference {
    /// The referencing item.
    pub from: BoardItemId,
    /// The referenced item or resource (another board item, an issue URL,
    /// a commit, ... — provider-defined opaque reference).
    pub to: String,
}

/// A typed search filter over items and fields (spec §9.43 `search`).
///
/// Conjunctive: an item matches when every set criterion matches. This is
/// deliberately NOT a query language.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSearch {
    /// Restrict to one item type, when set.
    pub item_type: Option<BoardItemType>,
    /// Restrict to one status name (exact match), when set.
    pub status: Option<String>,
    /// Required typed field values (name → value); every listed field must
    /// match exactly.
    pub fields: Vec<(String, BoardFieldValue)>,
}

impl BoardSearch {
    /// An empty filter matching every item.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            item_type: None,
            status: None,
            fields: Vec::new(),
        }
    }

    /// Restricts the filter to one item type.
    #[must_use]
    pub const fn with_type(mut self, item_type: BoardItemType) -> Self {
        self.item_type = Some(item_type);
        self
    }

    /// Restricts the filter to one status name (exact match).
    #[must_use]
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    /// Requires one typed field value (name → value, exact match).
    #[must_use]
    pub fn with_field(mut self, name: impl Into<String>, value: BoardFieldValue) -> Self {
        self.fields.push((name.into(), value));
        self
    }
}

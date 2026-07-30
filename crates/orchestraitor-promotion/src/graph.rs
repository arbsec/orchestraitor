//! Versioned transaction graph and history model (spec §9.4.3).
//!
//! The public abstraction is an Orchestraitor history graph — a versioned
//! transaction DAG that tracks every workspace mutation, normalization,
//! verification, review, and promotion as a node with a parent pointer. This
//! lets the TUI provide branching and time-travel without exposing Git ref
//! internals.
//!
//! Each node stores: parent, generation counter, changed-file digests, patch,
//! authoring principal, tool/agent responsible, verification evidence, and an
//! optional event hash from the tamper-evident audit log (spec §9.4.3, §9.17).

use orchestraitor_events::HashDigest;
use orchestraitor_model::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::PromotionError;
use crate::controller::{FileState, TrustedController};
use crate::diff::{TextualDiff, compute_textual_diff};

/// Opaque node identifier within a transaction graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(
    /// Underlying node identifier string.
    pub String,
);

impl NodeId {
    /// Returns the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Verification evidence captured at a transaction node (spec §9.4.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEvidence {
    /// Whether the test suite passed at this node.
    pub tests_passed: Option<bool>,
    /// Whether a formatter was applied.
    pub formatter_applied: bool,
    /// Number of lint findings at this node.
    pub lint_findings: u32,
    /// Free-form notes (e.g. review status).
    pub notes: String,
}

/// A node in the versioned transaction graph (spec §9.4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionNode {
    /// Unique node identifier.
    pub id: NodeId,
    /// Parent node, or `None` for the workspace base.
    pub parent: Option<NodeId>,
    /// Monotonic generation counter per session.
    pub generation: u64,
    /// Changed files and their content-addressed digests at this node.
    pub changed_files: Vec<FileState>,
    /// Authoring principal (spec §9.25 delegation chain).
    pub principal: String,
    /// Tool or agent responsible (domain + role + agent identity).
    pub agent: String,
    /// Verification evidence (test results, formatter, lint).
    pub verification: VerificationEvidence,
    /// Timestamp at which the node was committed.
    pub timestamp: Timestamp,
    /// Event hash from the tamper-evident audit log, when recorded.
    pub event_hash: Option<HashDigest>,
}

/// In-memory versioned transaction graph.
///
/// Provides `history`, `checkpoint`, `restore`, `branch`, `compare`, `undo`,
/// and `redo` operations mirroring the `orc` CLI (spec §9.4.3). The production
/// daemon persists this graph to the `SQLite` WAL event store + filesystem CAS
/// (spec §9.17); this in-memory implementation is the core data structure.
#[derive(Debug, Default)]
pub struct TransactionGraph {
    /// All nodes in commit order.
    nodes: Vec<TransactionNode>,
    /// Current HEAD node id.
    head: Option<NodeId>,
    /// Nodes undone via `undo`, available for `redo` (LIFO).
    redo_stack: Vec<NodeId>,
    /// Child count per node, for branch tracking.
    children: HashMap<NodeId, u32>,
    /// Monotonic ID counter.
    counter: u64,
}

impl TransactionGraph {
    /// Creates an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns all nodes in commit order.
    #[must_use]
    pub fn history(&self) -> &[TransactionNode] {
        &self.nodes
    }

    /// Returns the current HEAD node, if any.
    #[must_use]
    pub fn head(&self) -> Option<&TransactionNode> {
        self.head.as_ref().and_then(|id| self.node(id))
    }

    /// Looks up a node by id.
    #[must_use]
    pub fn node(&self, id: &NodeId) -> Option<&TransactionNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// Creates a checkpoint node capturing the current file states.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError`] only if the internal node store is in an
    /// inconsistent state (should not occur in normal operation).
    pub fn checkpoint(
        &mut self,
        files: Vec<FileState>,
        principal: &str,
        agent: &str,
    ) -> Result<&TransactionNode, PromotionError> {
        self.commit_node(files, principal, agent, VerificationEvidence::default())
    }

    /// Records a mutation node with changed files and verification evidence.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError`] only if the internal node store is in an
    /// inconsistent state (should not occur in normal operation).
    pub fn record_mutation(
        &mut self,
        files: Vec<FileState>,
        principal: &str,
        agent: &str,
        verification: VerificationEvidence,
    ) -> Result<&TransactionNode, PromotionError> {
        self.commit_node(files, principal, agent, verification)
    }

    /// Restores the workspace to a specific node through the trusted controller.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::NodeNotFound`] when the node does not exist,
    /// or [`PromotionError::ApplyFailed`] (via the controller) on restore failure.
    pub fn restore(
        &mut self,
        node_id: &NodeId,
        controller: &mut dyn TrustedController,
    ) -> Result<&TransactionNode, PromotionError> {
        let files = self
            .node(node_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: node_id.to_string(),
            })?
            .changed_files
            .clone();
        controller.restore(&files)?;
        self.head = Some(node_id.clone());
        self.redo_stack.clear();
        self.node(node_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: node_id.to_string(),
            })
    }

    /// Creates a divergent branch from a node (graph-only; does not touch the
    /// filesystem). Subsequent mutations extend from the branch point.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::NodeNotFound`] when the node does not exist.
    pub fn branch(&mut self, node_id: &NodeId) -> Result<NodeId, PromotionError> {
        if self.node(node_id).is_none() {
            return Err(PromotionError::NodeNotFound {
                node: node_id.to_string(),
            });
        }
        self.head = Some(node_id.clone());
        self.redo_stack.clear();
        Ok(node_id.clone())
    }

    /// Diffs the file states of two nodes.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::NodeNotFound`] when either node does not exist.
    pub fn compare(&self, a: &NodeId, b: &NodeId) -> Result<Vec<TextualDiff>, PromotionError> {
        let node_a = self.node(a).ok_or_else(|| PromotionError::NodeNotFound {
            node: a.to_string(),
        })?;
        let node_b = self.node(b).ok_or_else(|| PromotionError::NodeNotFound {
            node: b.to_string(),
        })?;
        let map_a = file_map(&node_a.changed_files);
        let map_b = file_map(&node_b.changed_files);
        let mut paths: Vec<PathBuf> = map_a.keys().chain(map_b.keys()).cloned().collect();
        paths.sort();
        paths.dedup();
        let mut diffs = Vec::with_capacity(paths.len());
        for path in paths {
            let old = map_a.get(&path).cloned().unwrap_or_default();
            let new = map_b.get(&path).cloned().unwrap_or_default();
            diffs.push(compute_textual_diff(path, &old, &new));
        }
        Ok(diffs)
    }

    /// Reverts to the parent of HEAD, pushing HEAD onto the redo stack.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::InvalidOperation`] when HEAD has no parent.
    pub fn undo(
        &mut self,
        controller: &mut dyn TrustedController,
    ) -> Result<&TransactionNode, PromotionError> {
        let head_id = self.head.clone().ok_or(PromotionError::InvalidOperation {
            reason: "no HEAD to undo".to_string(),
        })?;
        let parent_id = self
            .node(&head_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: head_id.to_string(),
            })?
            .parent
            .clone()
            .ok_or_else(|| PromotionError::InvalidOperation {
                reason: "HEAD has no parent".to_string(),
            })?;
        let files = self
            .node(&parent_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: parent_id.to_string(),
            })?
            .changed_files
            .clone();
        controller.restore(&files)?;
        self.redo_stack.push(head_id);
        self.head = Some(parent_id.clone());
        self.node(&parent_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: parent_id.to_string(),
            })
    }

    /// Re-applies the most recently undone node.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::InvalidOperation`] when there is nothing to redo.
    pub fn redo(
        &mut self,
        controller: &mut dyn TrustedController,
    ) -> Result<&TransactionNode, PromotionError> {
        let node_id = self
            .redo_stack
            .pop()
            .ok_or_else(|| PromotionError::InvalidOperation {
                reason: "nothing to redo".to_string(),
            })?;
        let files = self
            .node(&node_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: node_id.to_string(),
            })?
            .changed_files
            .clone();
        controller.restore(&files)?;
        self.head = Some(node_id.clone());
        self.node(&node_id)
            .ok_or_else(|| PromotionError::NodeNotFound {
                node: node_id.to_string(),
            })
    }

    /// Returns the number of nodes in the graph.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` when the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn commit_node(
        &mut self,
        files: Vec<FileState>,
        principal: &str,
        agent: &str,
        verification: VerificationEvidence,
    ) -> Result<&TransactionNode, PromotionError> {
        self.counter = self.counter.saturating_add(1);
        let id = NodeId(format!("node-{}", self.counter));
        let parent = self.head.clone();
        if let Some(ref pid) = parent {
            self.children
                .entry(pid.clone())
                .and_modify(|c| *c = c.saturating_add(1))
                .or_insert(1);
        }
        let node = TransactionNode {
            id: id.clone(),
            parent,
            generation: self.counter,
            changed_files: files,
            principal: principal.to_string(),
            agent: agent.to_string(),
            verification,
            timestamp: chrono::Utc::now(),
            event_hash: None,
        };
        self.nodes.push(node);
        self.head = Some(id);
        self.redo_stack.clear();
        self.nodes
            .last()
            .ok_or_else(|| PromotionError::InvalidOperation {
                reason: "internal: node was pushed but not found".to_string(),
            })
    }
}

fn file_map(files: &[FileState]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|f| (f.path.clone(), f.content.clone()))
        .collect()
}

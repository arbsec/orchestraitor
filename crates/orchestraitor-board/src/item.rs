//! Wire shapes for GraphQL responses and the untrusted-content conversion
//! into typed board item facts (spec §9.43).
//!
//! Board content is untrusted input (spec §6.1): titles and bodies are payload
//! data only. This module never reads issue bodies, never interpolates content
//! into commands, and never executes it.

use serde::Deserialize;

/// Raw GraphQL envelope: `data` plus optional top-level `errors`.
#[derive(Debug, Deserialize)]
pub(crate) struct GraphQlEnvelope {
    pub(crate) data: Option<serde_json::Value>,
    pub(crate) errors: Option<Vec<GraphQlErrorMessage>>,
}

/// Top-level GraphQL error entry.
#[derive(Debug, Deserialize)]
pub(crate) struct GraphQlErrorMessage {
    pub(crate) message: String,
}

/// Page info for the items connection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageInfo {
    pub(crate) has_next_page: bool,
    pub(crate) end_cursor: Option<String>,
}

/// One page of board items.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemPage {
    pub(crate) page_info: PageInfo,
    pub(crate) nodes: Vec<RawItem>,
}

/// A single project item as returned by the board query.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawItem {
    pub(crate) id: String,
    pub(crate) content: Option<ItemContent>,
    pub(crate) field_values: FieldValueConnection,
}

/// Item content; issue fields are absent for draft issues and pull requests.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemContent {
    #[serde(rename = "__typename")]
    pub(crate) typename: String,
    pub(crate) number: Option<u64>,
    pub(crate) state: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) repository: Option<RepositoryRef>,
    pub(crate) issue_type: Option<NameRef>,
    pub(crate) labels: Option<LabelConnection>,
    pub(crate) blocked_by: Option<BlockerConnection>,
}

/// Repository reference on an issue.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepositoryRef {
    pub(crate) name_with_owner: String,
}

/// A bare `name` object.
#[derive(Debug, Deserialize)]
pub(crate) struct NameRef {
    pub(crate) name: String,
}

/// Label connection with `totalCount`-based truncation detection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LabelConnection {
    pub(crate) nodes: Vec<NameRef>,
    pub(crate) total_count: u64,
}

/// Blocked-by connection with `totalCount`-based truncation detection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlockerConnection {
    pub(crate) nodes: Vec<StateRef>,
    pub(crate) total_count: u64,
}

/// A bare `state` object on a blocking issue.
#[derive(Debug, Deserialize)]
pub(crate) struct StateRef {
    pub(crate) state: String,
}

/// Field-value connection with `totalCount`-based truncation detection.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FieldValueConnection {
    pub(crate) nodes: Vec<FieldValue>,
    pub(crate) total_count: u64,
}

/// One field value; only single-select values carry our queried members.
#[derive(Debug, Deserialize)]
pub(crate) struct FieldValue {
    pub(crate) name: Option<String>,
    pub(crate) field: Option<OptionalNameRef>,
}

/// `field { name }` may be an empty object for non-single-select values.
#[derive(Debug, Deserialize)]
pub(crate) struct OptionalNameRef {
    pub(crate) name: Option<String>,
}

/// Why an item was excluded before predicate evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemSkip {
    /// Item content is not an issue on a configured repository or lacks
    /// required issue fields (malformed board data).
    Malformed(&'static str),
    /// Truncated connection windows make the item's safety undecidable.
    Truncated(&'static str),
    /// The item's issue state is not `OPEN`; closed issues never enter the
    /// ready queue even when board field values say otherwise.
    NotOpen,
}

/// Flat, validated view of one board item from a configured repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFacts {
    /// `ProjectV2Item` node id (runtime identity, never persisted to git).
    pub item_node_id: String,
    /// `org/name` of the issue's repository, lowercase.
    pub repo: String,
    /// Issue number within `repo`.
    pub number: u64,
    /// Issue title (untrusted text; never executed).
    pub title: String,
    /// Issue URL.
    pub url: String,
    /// Native issue type name, when set on the issue.
    pub issue_type: Option<String>,
    /// Label names on the issue; empty when the label window was truncated.
    pub labels: Vec<String>,
    /// Number of blocking issues whose state is not `CLOSED`.
    pub open_blockers: u64,
    /// Single-select value for the configured target field, when set.
    pub target: Option<String>,
    /// Single-select value for the configured status field, when set.
    pub status: Option<String>,
}

impl RawItem {
    /// Converts a raw item into [`ItemFacts`] scoped to configured
    /// repositories, or explains why the item cannot be trusted to evaluate.
    ///
    /// `repoless_ok` entries (draft issues, pull requests) return
    /// `Ok(None)` — a silent non-issue skip rather than a warning.
    pub(crate) fn facts(
        &self,
        repos: &[String],
        target_field: &str,
        ready_field: &str,
    ) -> Result<Option<ItemFacts>, ItemSkip> {
        if truncated(self.field_values.nodes.len(), self.field_values.total_count) {
            return Err(ItemSkip::Truncated("fieldValues"));
        }
        let Some(content) = &self.content else {
            return Err(ItemSkip::Malformed("missing content"));
        };
        if content.typename != "Issue" {
            return Ok(None);
        }
        let repository = content
            .repository
            .as_ref()
            .ok_or(ItemSkip::Malformed("issue has no repository"))?;
        let repo = repository.name_with_owner.to_lowercase();
        if !repos.iter().any(|known| known == &repo) {
            return Ok(None);
        }
        let number = content
            .number
            .ok_or(ItemSkip::Malformed("issue has no number"))?;
        let state = content
            .state
            .as_deref()
            .ok_or(ItemSkip::Malformed("issue has no state"))?;
        if state != "OPEN" {
            return Err(ItemSkip::NotOpen);
        }
        let title = content
            .title
            .clone()
            .ok_or(ItemSkip::Malformed("issue has no title"))?;
        let url = content
            .url
            .clone()
            .ok_or(ItemSkip::Malformed("issue has no url"))?;
        let labels = match &content.labels {
            Some(labels) if truncated(labels.nodes.len(), labels.total_count) => {
                return Err(ItemSkip::Truncated("labels"));
            }
            Some(labels) => labels.nodes.iter().map(|node| node.name.clone()).collect(),
            None => Vec::new(),
        };
        let open_blockers = match &content.blocked_by {
            Some(blockers) if truncated(blockers.nodes.len(), blockers.total_count) => {
                return Err(ItemSkip::Truncated("blockedBy"));
            }
            Some(blockers) => u64::try_from(
                blockers
                    .nodes
                    .iter()
                    .filter(|node| node.state != "CLOSED")
                    .count(),
            )
            .unwrap_or(u64::MAX),
            None => 0,
        };
        let (target, status) =
            single_select_values(&self.field_values.nodes, target_field, ready_field);
        Ok(Some(ItemFacts {
            item_node_id: self.id.clone(),
            repo,
            number,
            title,
            url,
            issue_type: content
                .issue_type
                .as_ref()
                .map(|issue_type| issue_type.name.clone()),
            labels,
            open_blockers,
            target,
            status,
        }))
    }
}

/// Fail-closed truncation check matching the ready-queue script's
/// `len(nodes) == totalCount` rule: any mismatch means the window cannot prove
/// eligibility, so the item is excluded.
pub(crate) fn truncated(nodes_len: usize, total_count: u64) -> bool {
    u64::try_from(nodes_len) != Ok(total_count)
}

/// Extracts the requested single-select field values from a node's value list.
fn single_select_values(
    nodes: &[FieldValue],
    target_field: &str,
    ready_field: &str,
) -> (Option<String>, Option<String>) {
    let mut target = None;
    let mut status = None;
    for node in nodes {
        let Some(field_name) = node.field.as_ref().and_then(|field| field.name.as_deref()) else {
            continue;
        };
        let Some(value_name) = node.name.clone() else {
            continue;
        };
        if field_name == target_field {
            target = Some(value_name);
        } else if field_name == ready_field {
            status = Some(value_name);
        }
    }
    (target, status)
}

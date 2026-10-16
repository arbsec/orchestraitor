//! GraphQL client for the GitHub Projects v2 board (spec §9.43).
//!
//! Node IDs are resolved at runtime from the human-readable names in the
//! board config and cached outside the repository. All reads page through the
//! item connection; the Status write round-trips through a read-back before
//! reporting success.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, USER_AGENT};
use secrecy::ExposeSecret;
use serde::Deserialize;
use zeroize::Zeroize;

use crate::auth::BoardAuth;
use crate::cache::{FieldNodeIds, NodeIdCacheFile, ProjectNodeIds};
use crate::config::BoardProjectConfig;
use crate::error::BoardError;
use crate::item::{GraphQlEnvelope, ItemFacts, ItemPage};
use crate::queue::{self, ReadyItem, SkipWarning};

const DEFAULT_ENDPOINT: &str = "https://api.github.com/graphql";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_mins(1);
const MAX_REDIRECTS: usize = 3;
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const API_VERSION: &str = "2022-11-28";

const RESOLVE_PROJECT_QUERY: &str = r"query($org: String!, $num: Int!) {
  organization(login: $org) {
    projectV2(number: $num) { id title }
  }
}";

const RESOLVE_FIELDS_QUERY: &str = r"query($id: ID!) {
  node(id: $id) {
    ... on ProjectV2 {
      fields(first: 50) {
        nodes {
          ... on ProjectV2SingleSelectField { id name options { id name } }
        }
      }
    }
  }
}";

const ITEMS_QUERY: &str = r"query($id: ID!, $after: String) {
  node(id: $id) {
    ... on ProjectV2 {
      items(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          content {
            __typename
            ... on Issue {
              number
              state
              title
              url
              repository { nameWithOwner }
              issueType { name }
              labels(first: 50) { nodes { name } totalCount }
              blockedBy(first: 100) { nodes { state } totalCount }
            }
          }
          fieldValues(first: 25) {
            totalCount
            nodes {
              ... on ProjectV2ItemFieldSingleSelectValue {
                name
                field { ... on ProjectV2SingleSelectField { name } }
              }
            }
          }
        }
      }
    }
  }
}";

const FIND_ITEM_QUERY: &str = r"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      id
      projectItems(first: 20) { totalCount nodes { id project { id } } }
    }
  }
}";

const ISSUE_BODY_QUERY: &str = r"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      title
      body
    }
  }
}";

const UPDATE_STATUS_MUTATION: &str = r"mutation($projectId: ID!, $itemId: ID!, $fieldId: ID!, $optionId: ID!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $projectId
    itemId: $itemId
    fieldId: $fieldId
    value: { singleSelectOptionId: $optionId }
  }) {
    projectV2Item { id }
  }
}";

const ADD_PROJECT_ITEM_MUTATION: &str = r"mutation($projectId: ID!, $contentId: ID!) {
  addProjectV2ItemById(input: { projectId: $projectId, contentId: $contentId }) {
    item { id }
  }
}";

const ISSUE_TYPE_ID_QUERY: &str = r"query($org: String!, $name: String!) {
  organization(login: $org) {
    issueTypes(first: 100) { nodes { id name } }
  }
  repository(owner: $org, name: $name) { issueTypes(first: 100) { nodes { id name } } }
}";

const UPDATE_ISSUE_TYPE_MUTATION: &str = r"mutation($issueId: ID!, $issueTypeId: ID!) {
  updateIssueIssueType(input: { issueId: $issueId, issueTypeId: $issueTypeId }) {
    issue { id }
  }
}";

const LABEL_ID_QUERY: &str = r"query($owner: String!, $name: String!, $labelName: String!) {
  repository(owner: $owner, name: $name) {
    label(name: $labelName) { id }
  }
}";

const REPOSITORY_ID_QUERY: &str = r"query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) { id }
}";

const CREATE_LABEL_MUTATION: &str = r#"mutation($repositoryId: ID!, $labelName: String!) {
  createLabel(input: { repositoryId: $repositoryId, name: $labelName, color: "1D76DB" }) {
    label { id }
  }
}"#;

const ADD_LABEL_MUTATION: &str = r"mutation($labelableId: ID!, $labelIds: [ID!]!) {
  addLabelsToLabelable(input: { labelableId: $labelableId, labelIds: $labelIds }) {
    labelable { __typename }
  }
}";

const READ_BACK_QUERY: &str = r"query($id: ID!, $name: String!) {
  node(id: $id) {
    ... on ProjectV2Item {
      fieldValueByName(name: $name) {
        ... on ProjectV2ItemFieldSingleSelectValue { name }
      }
    }
  }
}";

/// Outcome of a verified status write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOutcome {
    /// Issue number whose board item moved.
    pub number: u64,
    /// `ProjectV2Item` node id that was updated.
    pub item_id: String,
    /// Status name confirmed by the read-back.
    pub status: String,
}

/// The verbatim content of one board issue (issue #495). Both fields are
/// UNTRUSTED PROVIDER CONTENT (spec `40-arbitraitor-integration.md` §6.1):
/// carried as data, never executed, and bounded by the caller before
/// inlining into a worker-bound payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueContent {
    /// Verbatim issue title.
    pub title: String,
    /// Verbatim issue body (empty when the issue carries none).
    pub body: String,
}

/// GitHub Projects v2 board client.
pub struct BoardClient {
    http: reqwest::Client,
    endpoint: String,
    auth: Arc<dyn BoardAuth>,
    cache_path: Option<PathBuf>,
}

/// The mechanical field set §9.52(a) prescribes for one attached bug
/// report, as the board write path receives it (epic #624, leaf #664).
/// Every value is a lane constant or a launch fact — never content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeFields<'a> {
    /// The native issue type to set (`Bug` for issue reports).
    pub item_type: &'a str,
    /// The status the item lands at (`Triage`, always).
    pub status: &'a str,
    /// The provisional priority (`P1`; §9.52(d)).
    pub priority: &'a str,
    /// The daemon-owned author-class provenance label.
    pub label: &'a str,
}

impl BoardClient {
    /// Creates a client against the public GitHub GraphQL endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be constructed.
    pub fn new(auth: Arc<dyn BoardAuth>) -> Result<Self, BoardError> {
        Self::with_endpoint(DEFAULT_ENDPOINT.to_string(), auth)
    }

    /// Creates a client against an explicit endpoint (GHES, tests).
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be constructed.
    pub fn with_endpoint(endpoint: String, auth: Arc<dyn BoardAuth>) -> Result<Self, BoardError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()
            .map_err(|source| BoardError::HttpClient(Box::new(source)))?;
        Ok(Self {
            http,
            endpoint,
            auth,
            cache_path: crate::cache::default_cache_path(),
        })
    }

    /// Overrides the node-id cache location; `None` disables the disk cache.
    #[must_use]
    pub fn with_cache_path(mut self, cache_path: Option<PathBuf>) -> Self {
        self.cache_path = cache_path;
        self
    }

    /// Fetches the validated facts for every open board item in the
    /// configured repositories, plus the warning channel for items the
    /// conversion could not safely evaluate (closed issues surface as
    /// `NotOpen` warnings; malformed or truncated items fail closed).
    ///
    /// This is the reconciled board state the campaign pass reads (spec
    /// §9.35); `ready_items` applies the ready-queue predicate on top of it.
    ///
    /// # Errors
    ///
    /// Returns a typed error for auth, transport, GraphQL, or cache failures.
    pub async fn item_facts(
        &self,
        config: &BoardProjectConfig,
    ) -> Result<(Vec<ItemFacts>, Vec<SkipWarning>), BoardError> {
        let ids = self.resolved_ids(config).await?;
        let mut after: Option<String> = None;
        let mut all_facts = Vec::new();
        let mut warnings = Vec::new();
        loop {
            let data = self
                .execute(
                    ITEMS_QUERY,
                    serde_json::json!({ "id": ids.project_id, "after": after }),
                )
                .await?;
            let response: ItemsResponse = decode(data, "board items query")?;
            let project = response.node.ok_or_else(|| BoardError::ProjectNotFound {
                org: config.organization.clone(),
                number: config.project_number,
            })?;
            let ItemPage { page_info, nodes } = project.items;
            for raw in &nodes {
                match raw.facts(
                    &config.repos,
                    config.target_field.as_str(),
                    config.ready_field.as_str(),
                    config.priority_field.as_str(),
                ) {
                    Ok(Some(facts)) => all_facts.push(facts),
                    Ok(None) => {}
                    Err(skip) => warnings.push(queue::skip_warning(raw, &skip)),
                }
            }
            if !page_info.has_next_page {
                break;
            }
            let Some(cursor) = page_info.end_cursor else {
                return Err(BoardError::TruncatedConnection { window: "items" });
            };
            after = cursor.into();
        }
        Ok((all_facts, warnings))
    }

    /// Lists the ready queue: leaf Task/Bug items with the configured target
    /// and ready values and no unresolved blockers (spec §9.40).
    ///
    /// # Errors
    ///
    /// Returns a typed error for auth, transport, GraphQL, or cache failures.
    pub async fn ready_items(
        &self,
        config: &BoardProjectConfig,
    ) -> Result<(Vec<ReadyItem>, Vec<SkipWarning>), BoardError> {
        let (all_facts, warnings) = self.item_facts(config).await?;
        Ok((queue::ready_queue(&all_facts, config), warnings))
    }

    /// Writes the Status field of the board item for `issue_number` and
    /// verifies the write by reading the field back.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the issue is unknown, not on the board, the
    /// option name is unresolved, the mutation fails, or the read-back does
    /// not match.
    pub async fn move_item(
        &self,
        config: &BoardProjectConfig,
        issue_number: u64,
        status: &str,
    ) -> Result<MoveOutcome, BoardError> {
        let ids = self.resolved_ids(config).await?;
        let field = ids.fields.get(config.ready_field.as_str()).ok_or_else(|| {
            BoardError::FieldNotFound {
                name: config.ready_field.clone(),
            }
        })?;
        let option_id = field
            .options
            .get(status)
            .ok_or_else(|| BoardError::OptionNotFound {
                field: config.ready_field.clone(),
                option: status.to_string(),
            })?;
        let item_id = self.find_item(config, &ids, issue_number).await?;
        self.execute(
            UPDATE_STATUS_MUTATION,
            serde_json::json!({
                "projectId": ids.project_id,
                "itemId": item_id,
                "fieldId": field.field_id,
                "optionId": option_id,
            }),
        )
        .await?;
        let read_back = self
            .execute(
                READ_BACK_QUERY,
                serde_json::json!({ "id": item_id, "name": config.ready_field }),
            )
            .await?;
        let response: ReadBackResponse = decode(read_back, "status read-back")?;
        let observed = response
            .node
            .and_then(|node| node.field_value_by_name)
            .and_then(|value| value.name);
        if observed.as_deref() != Some(status) {
            return Err(BoardError::StatusVerification {
                field: config.ready_field.clone(),
                expected: status.to_string(),
            });
        }
        Ok(MoveOutcome {
            number: issue_number,
            item_id,
            status: status.to_string(),
        })
    }

    /// Attaches `issue_number` to the board with the §9.52(a) mechanical
    /// field set (epic #624, leaf #664): adds the issue to the project
    /// (idempotent — an already-attached issue is detected and NOT
    /// duplicated), sets the Status, Priority, and native issue type, and
    /// applies the daemon-owned author-class provenance label.
    ///
    /// Every argument is a LAUNCH FACT or a lane constant (§9.52(a)):
    /// issue content never flows through this path. Returns `true` when
    /// the item was newly added, `false` when it was already on the board
    /// (the idempotent update path — the callers treat both as success).
    ///
    /// # Errors
    ///
    /// Typed transport/GraphQL failures — the caller retries on a later
    /// tick, never fails open into scheduling.
    pub async fn attach_issue(
        &self,
        config: &BoardProjectConfig,
        repo: &str,
        issue_number: u64,
        fields: IntakeFields<'_>,
    ) -> Result<bool, BoardError> {
        let ids = self.resolved_ids(config).await?;
        // The exact-repo issue lookup (the same shape `issue_body` uses):
        // never a configured-repository walk, never a guessed owner.
        let Some((owner, name)) = split_repo(repo) else {
            return Err(BoardError::IssueNotFound {
                number: issue_number,
            });
        };
        // Node ids are cached per project; the issue node id is fetched
        // through the same FIND_ITEM_QUERY the move path uses (which also
        // answers the §9.26.3 idempotency question: is the issue already
        // a project item?).
        let data = self
            .execute(
                FIND_ITEM_QUERY,
                serde_json::json!({
                    "owner": owner,
                    "name": name,
                    "number": issue_number,
                }),
            )
            .await?;
        let response: IssueWithContentResponse = decode(data, "issue lookup")?;
        let issue = response
            .repository
            .and_then(|repository| repository.issue)
            .ok_or(BoardError::IssueNotFound {
                number: issue_number,
            })?;
        let issue_id = issue.id;
        // Fail closed on a truncated projectItems window (the same
        // discipline as `find_item`): an issue on more projects than the
        // window holds may already be attached OUTSIDE the window —
        // treating it as unattached would duplicate the item and break
        // the §9.26.3 idempotency signal.
        if crate::item::truncated(
            issue.project_items.nodes.len(),
            issue.project_items.total_count,
        ) {
            return Err(BoardError::TruncatedConnection {
                window: "projectItems",
            });
        }
        let existing_item = issue
            .project_items
            .nodes
            .iter()
            .find(|project_item| project_item.project.id == ids.project_id)
            .map(|project_item| project_item.id.clone());
        let item_id = if let Some(item_id) = existing_item.clone() {
            item_id
        } else {
            let data = self
                .execute(
                    ADD_PROJECT_ITEM_MUTATION,
                    serde_json::json!({
                        "projectId": ids.project_id,
                        "contentId": issue_id,
                    }),
                )
                .await?;
            let response: AddItemResponse = decode(data, "project item add")?;
            response
                .add_project_v2_item_by_id
                .and_then(|added| added.item)
                .map(|item| item.id)
                .ok_or(BoardError::ResponseShape {
                    context: "project item add",
                    source: Box::new(decode_error("mutation returned no item id")),
                })?
        };
        // The mechanical field writes apply ONLY to a NEWLY added item
        // (§9.52: promotion authority is the triage pass's alone). An
        // ALREADY-attached item may have been promoted by the triage
        // pass or moved by a human since attach — overwriting Status or
        // Priority here would reverse a promotion the attach lane has
        // no authority over. The label stays additive either way.
        if existing_item.is_none() {
            // Status: the verified write (same read-back discipline as
            // `move_item` — the write round-trips before success).
            self.set_single_select(&ids, &item_id, &config.ready_field, fields.status)
                .await?;
            // Priority: the §9.52(d) provisional value (never the
            // scheduling value; the triage pass owns any change).
            self.set_single_select(&ids, &item_id, &config.priority_field, fields.priority)
                .await?;
            // Native issue type: resolved by NAME against the org (then
            // the repository); an unresolvable type name is a typed
            // error, never a silently skipped write (the ready queue's
            // leaf predicate reads this exact field).
            self.set_issue_type(repo, &issue_id, fields.item_type)
                .await?;
        }
        // The daemon-owned provenance label (§9.50(c) discipline): label
        // ids resolve by exact name; a missing label is created — the
        // name is a lane constant, never content. Additive on both
        // paths.
        self.apply_label(repo, &issue_id, fields.label).await?;
        Ok(existing_item.is_none())
    }

    /// Writes one single-select field and verifies by read-back (the
    /// `move_item` discipline, generalized over the field name).
    async fn set_single_select(
        &self,
        ids: &ProjectNodeIds,
        item_id: &str,
        field_name: &str,
        value: &str,
    ) -> Result<(), BoardError> {
        let field = ids
            .fields
            .get(field_name)
            .ok_or(BoardError::FieldNotFound {
                name: field_name.to_string(),
            })?;
        let option_id = field.options.get(value).ok_or(BoardError::OptionNotFound {
            field: field_name.to_string(),
            option: value.to_string(),
        })?;
        self.execute(
            UPDATE_STATUS_MUTATION,
            serde_json::json!({
                "projectId": ids.project_id,
                "itemId": item_id,
                "fieldId": field.field_id,
                "optionId": option_id,
            }),
        )
        .await?;
        let read_back = self
            .execute(
                READ_BACK_QUERY,
                serde_json::json!({ "id": item_id, "name": field_name }),
            )
            .await?;
        let response: ReadBackResponse = decode(read_back, "single-select read-back")?;
        let observed = response
            .node
            .and_then(|node| node.field_value_by_name)
            .and_then(|value| value.name);
        if observed.as_deref() != Some(value) {
            return Err(BoardError::StatusVerification {
                field: field_name.to_string(),
                expected: value.to_string(),
            });
        }
        Ok(())
    }

    /// Resolves the native issue-type node id by name (org types first,
    /// then repository types) and writes it onto the issue.
    async fn set_issue_type(
        &self,
        repo: &str,
        issue_id: &str,
        type_name: &str,
    ) -> Result<(), BoardError> {
        let Some((owner, name)) = split_repo(repo) else {
            return Err(BoardError::IssueNotFound { number: 0 });
        };
        let data = self
            .execute(
                ISSUE_TYPE_ID_QUERY,
                serde_json::json!({ "org": owner, "name": name }),
            )
            .await?;
        let response: IssueTypesResponse = decode(data, "issue type resolution")?;
        let type_id = response
            .organization
            .and_then(|org| org.issue_types)
            .and_then(|connection| connection.nodes)
            .unwrap_or_default()
            .iter()
            .chain(
                response
                    .repository
                    .and_then(|repository| repository.issue_types)
                    .and_then(|connection| connection.nodes)
                    .unwrap_or_default()
                    .iter(),
            )
            .find(|node| node.name.as_deref() == Some(type_name))
            .and_then(|node| node.id.clone());
        let Some(type_id) = type_id else {
            return Err(BoardError::IssueTypeNotFound {
                name: type_name.to_string(),
            });
        };
        self.execute(
            UPDATE_ISSUE_TYPE_MUTATION,
            serde_json::json!({
                "issueId": issue_id,
                "issueTypeId": type_id,
            }),
        )
        .await?;
        Ok(())
    }

    /// Applies the daemon-owned provenance label by exact name,
    /// creating it (a fixed color; the name is configuration, never
    /// content) when the repository does not have it yet.
    async fn apply_label(
        &self,
        repo: &str,
        issue_id: &str,
        label_name: &str,
    ) -> Result<(), BoardError> {
        let Some((owner, name)) = split_repo(repo) else {
            return Err(BoardError::IssueNotFound { number: 0 });
        };
        let data = self
            .execute(
                LABEL_ID_QUERY,
                serde_json::json!({
                    "owner": owner,
                    "name": name,
                    "labelName": label_name,
                }),
            )
            .await?;
        let response: LabelIdResponse = decode(data, "label resolution")?;
        let label_id = response
            .repository
            .and_then(|repository| repository.label)
            .and_then(|label| label.id);
        let label_id = if let Some(label_id) = label_id {
            label_id
        } else {
            // The label does not exist: create it (daemon-owned; a
            // fixed color — the name is the only carrier of meaning).
            let repo_data = self
                .execute(
                    REPOSITORY_ID_QUERY,
                    serde_json::json!({ "owner": owner, "name": name }),
                )
                .await?;
            let repo_response: RepositoryIdResponse = decode(repo_data, "repository id")?;
            let repository_id = repo_response
                .repository
                .and_then(|repository| repository.id)
                .ok_or(BoardError::IssueNotFound { number: 0 })?;
            let data = self
                .execute(
                    CREATE_LABEL_MUTATION,
                    serde_json::json!({
                        "repositoryId": repository_id,
                        "labelName": label_name,
                    }),
                )
                .await?;
            let response: CreateLabelResponse = decode(data, "label create")?;
            response
                .create_label
                .and_then(|created| created.label)
                .and_then(|label| label.id)
                .ok_or(BoardError::ResponseShape {
                    context: "label create",
                    source: Box::new(decode_error("mutation returned no label id")),
                })?
        };
        self.execute(
            ADD_LABEL_MUTATION,
            serde_json::json!({
                "labelableId": issue_id,
                "labelIds": [label_id],
            }),
        )
        .await?;
        Ok(())
    }

    /// Fetches the verbatim title and body of one board issue through the
    /// service-identity auth route (issue #495): the loop materializes the
    /// worker task fixture WITH the issue content so the worker never has to
    /// infer acceptance criteria from the title alone.
    ///
    /// Unlike [`Self::find_item`], this fetches from the EXACT `repo` given
    /// (no configured-repository walk, no ambiguity resolution): callers
    /// pass the `repo` from the board selection, which the poller derived
    /// from the configured repositories. A malformed `repo` (empty side,
    /// extra separator) is answered with [`BoardError::IssueNotFound`] —
    /// the query never leaves with a guessed owner or name.
    ///
    /// The body is UNTRUSTED PROVIDER CONTENT (spec `40-arbitraitor-integration.md`
    /// §6.1): the caller carries it as data — fenced and labelled — and never
    /// executes any command it contains. No size cap is applied here: the
    /// body is fetched verbatim and the CALLER owns the bounded-inline-payload
    /// posture (cap + truncation notice) for what it embeds downstream.
    ///
    /// # Errors
    ///
    /// Returns a typed [`BoardError`] for auth, transport, GraphQL, shape,
    /// or not-found failures.
    pub async fn issue_body(
        &self,
        repo: &str,
        issue_number: u64,
    ) -> Result<IssueContent, BoardError> {
        let Some((owner, name)) = repo.split_once('/') else {
            return Err(BoardError::IssueNotFound {
                number: issue_number,
            });
        };
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return Err(BoardError::IssueNotFound {
                number: issue_number,
            });
        }
        let data = self
            .execute(
                ISSUE_BODY_QUERY,
                serde_json::json!({
                    "owner": owner,
                    "name": name,
                    "number": issue_number,
                }),
            )
            .await?;
        let response: IssueBodyResponse = decode(data, "issue body fetch")?;
        let issue = response
            .repository
            .and_then(|repository| repository.issue)
            .ok_or(BoardError::IssueNotFound {
                number: issue_number,
            })?;
        Ok(IssueContent {
            title: issue.title.unwrap_or_default(),
            body: issue.body,
        })
    }

    /// Resolves the project and single-select field node IDs, consulting the
    /// on-disk cache first (spec §9.43: cache outside the repository).
    async fn resolved_ids(
        &self,
        config: &BoardProjectConfig,
    ) -> Result<ProjectNodeIds, BoardError> {
        if let Some(path) = &self.cache_path {
            let cache = NodeIdCacheFile::load(path)?;
            if let Some(ids) = cache.project(&config.organization, config.project_number)
                && ids.fields.contains_key(config.ready_field.as_str())
                && ids.fields.contains_key(config.target_field.as_str())
            {
                return Ok(ids.clone());
            }
        }
        let ids = self.resolve_project(config).await?;
        if let Some(path) = &self.cache_path {
            let mut cache = NodeIdCacheFile::load(path)?;
            cache.upsert(&config.organization, config.project_number, ids.clone());
            cache.store(path)?;
        }
        Ok(ids)
    }

    async fn resolve_project(
        &self,
        config: &BoardProjectConfig,
    ) -> Result<ProjectNodeIds, BoardError> {
        let data = self
            .execute(
                RESOLVE_PROJECT_QUERY,
                serde_json::json!({
                    "org": config.organization,
                    "num": config.project_number,
                }),
            )
            .await?;
        let response: ProjectResponse = decode(data, "project id resolution")?;
        let project = response
            .organization
            .and_then(|org| org.project_v2)
            .ok_or_else(|| BoardError::ProjectNotFound {
                org: config.organization.clone(),
                number: config.project_number,
            })?;
        let data = self
            .execute(
                RESOLVE_FIELDS_QUERY,
                serde_json::json!({ "id": project.id }),
            )
            .await?;
        let response: FieldsResponse = decode(data, "field id resolution")?;
        let mut fields = BTreeMap::new();
        let nodes = response
            .node
            .ok_or_else(|| BoardError::ProjectNotFound {
                org: config.organization.clone(),
                number: config.project_number,
            })?
            .fields
            .nodes;
        for node in nodes {
            let (Some(field_id), Some(name)) = (node.id, node.name) else {
                continue;
            };
            let options = node
                .options
                .unwrap_or_default()
                .into_iter()
                .map(|option| (option.name, option.id))
                .collect();
            fields.insert(name, FieldNodeIds { field_id, options });
        }
        Ok(ProjectNodeIds {
            project_id: project.id,
            fields,
        })
    }

    /// Resolves the board item for `issue_number` across ALL configured
    /// repositories: a same-numbered issue in an earlier repo without a board
    /// item never shadows later repos, and matches under multiple repos are
    /// rejected instead of guessed.
    async fn find_item(
        &self,
        config: &BoardProjectConfig,
        ids: &ProjectNodeIds,
        issue_number: u64,
    ) -> Result<String, BoardError> {
        let mut matches: Vec<(String, String)> = Vec::new();
        let mut issue_seen = false;
        for repo in &config.repos {
            let Some((owner, name)) = repo.split_once('/') else {
                continue;
            };
            let data = self
                .execute(
                    FIND_ITEM_QUERY,
                    serde_json::json!({
                        "owner": owner,
                        "name": name,
                        "number": issue_number,
                    }),
                )
                .await?;
            let response: IssueResponse = decode(data, "issue lookup")?;
            let Some(issue) = response.repository.and_then(|repo| repo.issue) else {
                continue;
            };
            issue_seen = true;
            if crate::item::truncated(
                issue.project_items.nodes.len(),
                issue.project_items.total_count,
            ) {
                return Err(BoardError::TruncatedConnection {
                    window: "projectItems",
                });
            }
            for project_item in issue.project_items.nodes {
                if project_item.project.id == ids.project_id {
                    matches.push((repo.clone(), project_item.id));
                }
            }
        }
        if matches.len() > 1 {
            return Err(BoardError::AmbiguousItemReference {
                number: issue_number,
                repos: matches.iter().map(|(repo, _id)| repo.clone()).collect(),
            });
        }
        if let Some((_repo, item_id)) = matches.pop() {
            return Ok(item_id);
        }
        if issue_seen {
            return Err(BoardError::ItemNotOnBoard {
                number: issue_number,
            });
        }
        Err(BoardError::IssueNotFound {
            number: issue_number,
        })
    }

    /// Executes one GraphQL operation and returns the `data` payload.
    ///
    /// The bearer token is injected into the request headers only; it never
    /// enters a serialized body, error, or trace (spec §9.23.4).
    async fn execute(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, BoardError> {
        let token = self.auth.bearer_token().await?;
        let response = self
            .http
            .post(&self.endpoint)
            .header(AUTHORIZATION, bearer_header(&token)?)
            .header(USER_AGENT, HeaderValue::from_static("orchestraitor-board"))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header("X-GitHub-Api-Version", API_VERSION)
            .json(&serde_json::json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|source| BoardError::Transport(Box::new(source)))?;
        let status = response.status();
        if !status.is_success() {
            return Err(BoardError::HttpStatus {
                status: status.as_u16(),
            });
        }
        if let Some(length) = response.content_length()
            && length > MAX_RESPONSE_BYTES
        {
            return Err(BoardError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|source| BoardError::Transport(Box::new(source)))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RESPONSE_BYTES {
            return Err(BoardError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }
        let envelope: GraphQlEnvelope =
            serde_json::from_slice(&bytes).map_err(|source| BoardError::ResponseShape {
                context: "envelope decoding",
                source: Box::new(source),
            })?;
        if let Some(errors) = envelope.errors
            && !errors.is_empty()
        {
            return Err(BoardError::GraphQl {
                messages: errors.into_iter().map(|error| error.message).collect(),
            });
        }
        envelope.data.ok_or_else(|| BoardError::GraphQl {
            messages: vec!["response carried errors: no data payload".to_string()],
        })
    }
}

/// Builds the bearer Authorization header value from the in-memory secret.
///
/// # Errors
///
/// Returns a typed error when the token value is not a valid header value; a
/// malformed token must surface here, never become a silently wrong header.
fn bearer_header(token: &secrecy::SecretString) -> Result<HeaderValue, BoardError> {
    let mut value = format!("Bearer {}", token.expose_secret());
    let header = HeaderValue::from_str(&value);
    value.zeroize();
    header.map_err(|_error| BoardError::AuthTokenInvalid)
}

/// Decodes a `data` payload into the expected response shape.
fn decode<T: for<'de> Deserialize<'de>>(
    data: serde_json::Value,
    context: &'static str,
) -> Result<T, BoardError> {
    serde_json::from_value(data).map_err(|source| BoardError::ResponseShape {
        context,
        source: Box::new(source),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectResponse {
    organization: Option<OrgProject>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgProject {
    project_v2: Option<ProjectIdTitle>,
}

#[derive(Debug, Deserialize)]
struct ProjectIdTitle {
    id: String,
}

#[derive(Debug, Deserialize)]
struct FieldsResponse {
    node: Option<ProjectFields>,
}

#[derive(Debug, Deserialize)]
struct ProjectFields {
    fields: FieldConnection,
}

#[derive(Debug, Deserialize)]
struct FieldConnection {
    nodes: Vec<FieldNode>,
}

#[derive(Debug, Deserialize)]
struct FieldNode {
    id: Option<String>,
    name: Option<String>,
    options: Option<Vec<OptionNameId>>,
}

#[derive(Debug, Deserialize)]
struct OptionNameId {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct ItemsResponse {
    node: Option<ProjectItems>,
}

#[derive(Debug, Deserialize)]
struct ProjectItems {
    items: ItemPage,
}

#[derive(Debug, Deserialize)]
struct IssueResponse {
    repository: Option<IssueRepo>,
}

#[derive(Debug, Deserialize)]
struct IssueRepo {
    issue: Option<IssueRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueRef {
    project_items: ProjectItemConnection,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectItemConnection {
    nodes: Vec<ProjectItemRef>,
    total_count: u64,
}

#[derive(Debug, Deserialize)]
struct ProjectItemRef {
    id: String,
    project: ProjectIdTitle,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadBackResponse {
    node: Option<ReadBackNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadBackNode {
    field_value_by_name: Option<ReadBackValue>,
}

#[derive(Debug, Deserialize)]
struct ReadBackValue {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IssueBodyResponse {
    repository: Option<IssueBodyRepo>,
}

#[derive(Debug, Deserialize)]
struct IssueBodyRepo {
    issue: Option<IssueBody>,
}

#[derive(Debug, Deserialize)]
struct IssueBody {
    title: Option<String>,
    // REQUIRED: an omitted or null `body` is a shape failure (the typed
    // error path), never a silently-empty body — an issue with a genuinely
    // empty body still serializes `""` from GitHub.
    body: String,
}

/// The `FIND_ITEM_QUERY` response extended with the issue node id (the
/// intake attach needs the content id the add-item mutation takes).
#[derive(Debug, Deserialize)]
struct IssueWithContentResponse {
    repository: Option<IssueWithContentRepo>,
}

#[derive(Debug, Deserialize)]
struct IssueWithContentRepo {
    issue: Option<IssueWithContent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueWithContent {
    id: String,
    project_items: ProjectItemConnection,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddItemResponse {
    add_project_v2_item_by_id: Option<AddedItem>,
}

#[derive(Debug, Deserialize)]
struct AddedItem {
    item: Option<AddedItemRef>,
}

#[derive(Debug, Deserialize)]
struct AddedItemRef {
    id: String,
}

#[derive(Debug, Deserialize)]
struct IssueTypesResponse {
    organization: Option<OrgIssueTypes>,
    repository: Option<RepoIssueTypes>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgIssueTypes {
    issue_types: Option<IssueTypeConnection>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoIssueTypes {
    issue_types: Option<IssueTypeConnection>,
}

#[derive(Debug, Deserialize)]
struct IssueTypeConnection {
    nodes: Option<Vec<IssueTypeNode>>,
}

#[derive(Debug, Deserialize)]
struct IssueTypeNode {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LabelIdResponse {
    repository: Option<LabelRepo>,
}

#[derive(Debug, Deserialize)]
struct LabelRepo {
    label: Option<LabelNode>,
}

#[derive(Debug, Deserialize)]
struct LabelNode {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepositoryIdResponse {
    repository: Option<RepositoryNode>,
}

#[derive(Debug, Deserialize)]
struct RepositoryNode {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateLabelResponse {
    create_label: Option<CreatedLabel>,
}

#[derive(Debug, Deserialize)]
struct CreatedLabel {
    label: Option<LabelNode>,
}

/// Splits one `owner/name` repository slug; a malformed slug is a typed
/// not-found, never a guessed owner or name.
fn split_repo(repo: &str) -> Option<(&str, &str)> {
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((owner, name))
}

/// Builds a serde error carrying a static message (the typed
/// `ResponseShape` path for shapeless GraphQL answers).
fn decode_error(message: &'static str) -> serde_json::Error {
    serde::de::Error::custom(message)
}

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
use crate::item::{GraphQlEnvelope, ItemPage};
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

/// GitHub Projects v2 board client.
pub struct BoardClient {
    http: reqwest::Client,
    endpoint: String,
    auth: Arc<dyn BoardAuth>,
    cache_path: Option<PathBuf>,
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
            after = Some(cursor);
        }
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

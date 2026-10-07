//! [`DecisionProvider`] implementation for the **System One** decision
//! protocol (spec `30-model-routing.md` §9.45).
//!
//! System One is an open protocol, not a vendor: a decision endpoint accepts
//! a state plus typed questions in one `POST <base_url>/systemone` pass and
//! answers every question with calibrated probabilities and zero generated
//! text. Any endpoint serving the shape works — the Neuralwatt cloud
//! (`https://api.neuralwatt.com/v1`, model id `clef-flash`) and self-hosted
//! Clef inference engines are both example deployments; this type is
//! endpoint-configured and names neither.
//!
//! Every method maps exactly one decision surface onto one System One
//! request:
//!
//! - `propose_role_resolution` — `choice` over the candidate `(provider,
//!   model)` pairs for the role.
//! - `propose_task_selection` — `choice` over the ready-queue task ids.
//! - `propose_task_split` / `propose_tool_selection` — typed surfaces with
//!   `noul`/`choice` probes; code adopting their outputs lands in a
//!   follow-up slice, the provider side ships complete.
//!
//! Any transport, status, or parse failure is a typed
//! [`DecisionProviderError`]; callers fall back to the heuristic table and
//! never error the loop. No secret material enters error or log output.

use async_trait::async_trait;
use orchestraitor_model::ProviderId;
use secrecy::ExposeSecret;

use crate::decision::{
    DecisionAlternative, DecisionProposal, DecisionProvider, DecisionResult, TaskSelection,
    TaskSplitProposal, TaskSplitSubtask, TaskSummary, ToolQueryContext, ToolSelection,
};
use crate::error::DecisionProviderError;
use crate::systemone::{
    SystemOneQuestion, SystemOneQuestionBody, SystemOneRequest, SystemOneResponse,
    parse_choice_answer, parse_noul_answer, question_body,
};

/// Decision-provider id surfaced in decision records (the protocol name —
/// records also carry the endpoint host through the decision path, never a
/// vendor name).
pub const SYSTEMONE_DECISION_PROVIDER_ID: &str = "systemone";

/// Default decision model id (the Neuralwatt cloud's Clef Flash id;
/// `routing.model` overrides it per project for any other endpoint).
pub const DEFAULT_DECISION_MODEL: &str = "clef-flash";

/// HTTP connect timeout.
pub(crate) const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// HTTP total request timeout.
pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

/// Endpoint configuration for [`SystemOneDecisionProvider`] — pure data,
/// resolved from the project-scoped `[routing]` config block by the caller.
/// No default URL lives here: the endpoint is operator configuration.
#[derive(Debug, Clone)]
pub struct SystemOneEndpointConfig {
    /// Decision endpoint base URL (for example `https://api.neuralwatt.com/v1`
    /// or a self-hosted engine over tailscale). REQUIRED — there is no
    /// protocol-level default. Plain-`http` endpoints are accepted for
    /// no-auth local/tailnet deployments (no credential crosses the wire);
    /// construction fails closed when a credential is configured against a
    /// non-`https`, non-loopback endpoint.
    pub base_url: String,
    /// Decision model id the endpoint serves (for example `clef-flash`).
    pub model: String,
    /// Resolved endpoint credential, when the endpoint takes auth. `None`
    /// (or an empty secret) sends no `Authorization` header. The value is
    /// never logged, serialized, or carried in an error.
    pub api_key: Option<secrecy::SecretString>,
}

/// Maps the layered `[routing]` config onto the endpoint configuration:
/// `routing.api_key = "none"` (or absence) means no auth; a `secret://…`
/// URI is resolved through the standard secret chain. The resolved value
/// never enters an error or log line.
///
/// # Errors
///
/// Returns [`SystemOneDecisionProviderError::Auth`] when a configured
/// `secret://` URI cannot be resolved.
pub fn resolve_endpoint(
    base_url: String,
    model: Option<String>,
    api_key_uri: Option<&str>,
) -> Result<SystemOneEndpointConfig, SystemOneDecisionProviderError> {
    let api_key = match api_key_uri {
        None => None,
        Some(uri) if uri.eq_ignore_ascii_case("none") => None,
        Some(uri) => {
            let parsed = orchestraitor_core::SecretUri::parse(uri)
                .map_err(|error| SystemOneDecisionProviderError::Auth(error.to_string()))?;
            let secret = parsed
                .resolve(orchestraitor_core::DEFAULT_KEYRING_SERVICE)
                .map_err(|error| SystemOneDecisionProviderError::Auth(error.to_string()))?;
            Some(secret)
        }
    };
    Ok(SystemOneEndpointConfig {
        base_url,
        model: model.unwrap_or_else(|| DEFAULT_DECISION_MODEL.to_string()),
        api_key,
    })
}

/// System One decision provider: any endpoint serving the protocol.
pub struct SystemOneDecisionProvider {
    provider_id: ProviderId,
    endpoint_host: String,
    http: reqwest::Client,
    endpoint: SystemOneEndpointConfig,
}

impl SystemOneDecisionProvider {
    /// Creates a provider from the project-scoped endpoint configuration.
    ///
    /// # Errors
    ///
    /// Returns [`SystemOneDecisionProviderError`] when the base URL is
    /// invalid or the HTTP client cannot be built.
    pub fn new(endpoint: SystemOneEndpointConfig) -> Result<Self, SystemOneDecisionProviderError> {
        // URL validity is validated at configuration time (not per request)
        // so a malformed base URL fails before it can enter the campaign
        // loop. The host is carried for decision-record attribution (the
        // endpoint identity, not a vendor name).
        let base = endpoint
            .base_url
            .parse::<url::Url>()
            .map_err(|source| SystemOneDecisionProviderError::InvalidBaseUrl { source })?;
        // Normalize before path joins: a configured trailing slash would
        // otherwise build `<base>//systemone`, which many servers answer
        // with 404 — silently degrading every consultation to the heuristic
        // fallback. Keep the non-default port (tailscale endpoints use one).
        let base_url = base.as_str().trim_end_matches('/').to_string();
        let endpoint_host = base.host_str().unwrap_or_default().to_string();
        // Fail closed on cleartext credentials (CWE-319): a configured
        // bearer token is never sent over plain `http` unless the endpoint
        // is loopback — the one case where no network observer exists
        // between this process and the engine. A no-auth tailnet endpoint
        // (the documented self-hosted deployment) sends no credential and
        // is unaffected; operator data classification is a separate,
        // config-level choice (`data_classification` rules).
        let has_credential = endpoint
            .api_key
            .as_ref()
            .is_some_and(|key| !key.expose_secret().is_empty());
        if has_credential && base.scheme() != "https" {
            let loopback = base.host_str().is_some_and(|host| {
                host == "localhost"
                    || host == "::1"
                    || host.parse::<std::net::Ipv4Addr>() == Ok(std::net::Ipv4Addr::LOCALHOST)
            });
            if !loopback {
                return Err(SystemOneDecisionProviderError::CleartextCredential {
                    scheme: base.scheme().to_string(),
                });
            }
        }
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|source| SystemOneDecisionProviderError::HttpRequest { source })?;
        Ok(Self {
            provider_id: ProviderId::from_string(SYSTEMONE_DECISION_PROVIDER_ID.to_string()),
            endpoint_host,
            http,
            endpoint: SystemOneEndpointConfig {
                base_url,
                ..endpoint
            },
        })
    }

    /// Returns the decision model id requests are sent to.
    #[must_use]
    pub fn decision_model(&self) -> &str {
        &self.endpoint.model
    }

    /// Returns the endpoint host recorded for decision attribution.
    #[must_use]
    pub fn endpoint_host(&self) -> &str {
        &self.endpoint_host
    }

    /// Sends one System One request and returns the parsed response.
    async fn ask(
        &self,
        state: String,
        questions: Vec<SystemOneQuestion>,
    ) -> Result<SystemOneResponse, SystemOneDecisionProviderError> {
        if questions.is_empty() {
            return Err(SystemOneDecisionProviderError::NoQuestions);
        }
        let mut map = serde_json::Map::new();
        for question in questions {
            map.insert(question.name.clone(), question_body(&question.question));
        }
        let request = SystemOneRequest {
            model: self.endpoint.model.clone(),
            state,
            questions: map,
        };
        let url = format!("{}/systemone", self.endpoint.base_url);
        let mut request = self.http.post(&url).json(&request);
        // A no-auth endpoint (a self-hosted decision engine) sends no
        // `Authorization` header; the header is omitted entirely — never
        // sent empty.
        if let Some(api_key) = &self.endpoint.api_key
            && !api_key.expose_secret().is_empty()
        {
            request = request.header(
                "Authorization",
                format!("Bearer {}", api_key.expose_secret()),
            );
        }
        let response = request
            .send()
            .await
            .map_err(|source| SystemOneDecisionProviderError::HttpRequest { source })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = response.text().await.unwrap_or_default();
            return Err(SystemOneDecisionProviderError::ProviderStatus {
                status: status.as_u16(),
                // Log-safe excerpt only; the body never contains secrets.
                body_excerpt: body_excerpt.chars().take(200).collect(),
            });
        }
        response
            .json::<SystemOneResponse>()
            .await
            .map_err(|source| SystemOneDecisionProviderError::ResponseParse { source })
    }

    /// Runs one `choice` question and returns the typed answer.
    async fn ask_choice(
        &self,
        state: String,
        name: &str,
        criteria: Vec<(String, String)>,
        instructions: Option<String>,
    ) -> Result<crate::systemone::SystemOneChoiceAnswer, SystemOneDecisionProviderError> {
        let response = self
            .ask(
                state,
                vec![SystemOneQuestion {
                    name: name.to_string(),
                    question: SystemOneQuestionBody::Choice {
                        criteria,
                        instructions,
                    },
                }],
            )
            .await?;
        let raw = response.answers.get(name).ok_or_else(|| {
            SystemOneDecisionProviderError::MissingAnswer {
                question: name.to_string(),
            }
        })?;
        parse_choice_answer(name, raw)
            .map_err(|reason| SystemOneDecisionProviderError::MalformedAnswer { reason })
    }

    /// Runs one `noul` question and returns the probability.
    async fn ask_noul(
        &self,
        state: String,
        name: &str,
        instructions: String,
    ) -> Result<f64, SystemOneDecisionProviderError> {
        let response = self
            .ask(
                state,
                vec![SystemOneQuestion {
                    name: name.to_string(),
                    question: SystemOneQuestionBody::Noul { instructions },
                }],
            )
            .await?;
        let raw = response.answers.get(name).ok_or_else(|| {
            SystemOneDecisionProviderError::MissingAnswer {
                question: name.to_string(),
            }
        })?;
        let answer = parse_noul_answer(name, raw)
            .map_err(|reason| SystemOneDecisionProviderError::MalformedAnswer { reason })?;
        Ok(answer.noul)
    }

    /// Builds the error raised for unparseable/failed answers: everything
    /// becomes a typed [`DecisionProviderError::Unavailable`] so callers
    /// fall back to the heuristic table and never error the loop.
    fn unavailable(&self, reason: String) -> DecisionProviderError {
        DecisionProviderError::Unavailable {
            provider_id: self.provider_id.clone(),
            reason,
        }
    }

    /// Maps a provider-level failure onto the caller-facing typed error.
    fn map_failure(&self, error: SystemOneDecisionProviderError) -> DecisionProviderError {
        match error {
            SystemOneDecisionProviderError::MalformedAnswer { reason }
            | SystemOneDecisionProviderError::MissingAnswer { question: reason } => {
                self.unavailable(reason)
            }
            other => self.unavailable(other.to_string()),
        }
    }
}

/// Failures building or sending a [`SystemOneDecisionProvider`] request.
/// The caller-facing [`DecisionProviderError`] never carries secret material;
/// this type carries only log-safe excerpts.
#[derive(Debug, thiserror::Error)]
pub enum SystemOneDecisionProviderError {
    /// The API key could not be resolved from the configured auth URI.
    #[error("systemone decision auth resolution failed: {0}")]
    Auth(String),
    /// The configured base URL could not be parsed.
    #[error("invalid systemone base URL: {source}")]
    InvalidBaseUrl {
        /// Underlying URL parse error.
        #[source]
        source: url::ParseError,
    },
    /// A bearer credential was configured against a non-`https` endpoint
    /// that is not loopback — the credential would cross the network
    /// unencrypted (CWE-319).
    #[error(
        "systemone decision endpoint uses `{scheme}` (not `https`) while a \
         `routing.api_key` credential is configured: the bearer token would \
         cross the network in cleartext. Point `routing.base_url` at an \
         `https` endpoint, or set `routing.api_key = \"none\"` for a no-auth \
         local/tailnet deployment."
    )]
    CleartextCredential {
        /// The URL scheme that would have carried the credential.
        scheme: String,
    },
    /// The HTTP client could not be built or the request failed.
    #[error("systemone decision HTTP request failed: {source}")]
    HttpRequest {
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },
    /// The System One endpoint returned a non-success status.
    #[error("systemone decision endpoint returned HTTP {status}")]
    ProviderStatus {
        /// HTTP status code.
        status: u16,
        /// Log-safe response-body excerpt.
        body_excerpt: String,
    },
    /// The response body could not be parsed.
    #[error("systemone decision response parse failed: {source}")]
    ResponseParse {
        /// Underlying JSON error.
        #[source]
        source: reqwest::Error,
    },
    /// The response omitted the answer for a posed question.
    #[error("systemone decision response omitted answer for `{question}`")]
    MissingAnswer {
        /// Question name whose answer was missing.
        question: String,
    },
    /// An answer had the wrong shape for its question type.
    #[error("systemone decision answer malformed: {reason}")]
    MalformedAnswer {
        /// Log-safe reason.
        reason: String,
    },
    /// A request was attempted without any question (programmer error).
    #[error("systemone decision request posed no questions")]
    NoQuestions,
}

#[async_trait]
impl DecisionProvider for SystemOneDecisionProvider {
    fn id(&self) -> &ProviderId {
        &self.provider_id
    }

    async fn propose_role_resolution(&self, role: &str) -> DecisionResult<DecisionProposal> {
        // Candidate set mirrors the heuristic bootstrap table's shape: the
        // single-provider default plus its flash sibling as the alternative
        // the decision model must explicitly consider and skip.
        let candidates = [("neuralwatt", "glm-5.3-flash"), ("neuralwatt", "glm-5.3")];
        let state = format!(
            "Role routing decision. Orchestration role: {role}. Choose the model \
             that best serves this role from the labeled candidates."
        );
        let criteria: Vec<(String, String)> = candidates
            .iter()
            .map(|(provider, model)| {
                (
                    format!("{provider}/{model}"),
                    format!("{provider} model {model}"),
                )
            })
            .collect();
        let answer = self
            .ask_choice(
                state,
                "pick",
                criteria,
                Some("Which candidate first?".to_string()),
            )
            .await
            .map_err(|error| self.map_failure(error))?;
        let chosen = candidates
            .iter()
            .find(|(provider, model)| format!("{provider}/{model}") == answer.choice)
            .ok_or_else(|| {
                self.unavailable(format!(
                    "proposed candidate '{}' is not in the candidate set",
                    answer.choice
                ))
            })?;
        let alternatives = candidates
            .iter()
            .filter(|candidate| *candidate != chosen)
            .map(|(provider, model)| DecisionAlternative {
                provider: ProviderId::from_string((*provider).to_string()),
                model: (*model).to_string(),
                skip_reason: "skipped-because-decision-model-preferred-otherwise".to_string(),
            })
            .collect();
        DecisionProposal::new(
            role,
            ProviderId::from_string(chosen.0.to_string()),
            chosen.1,
            answer.confidence,
            alternatives,
        )
        .map_err(|error| self.unavailable(format!("proposal rejected at the boundary: {error}")))
    }

    async fn propose_task_selection(
        &self,
        ready_task_ids: &[String],
    ) -> DecisionResult<TaskSelection> {
        if ready_task_ids.is_empty() {
            return Err(self.unavailable("empty ready queue".to_string()));
        }
        // Board content is untrusted input (spec §6.1): the state carries
        // only deterministic task ids, never titles or bodies.
        let state = format!(
            "Task selection decision. Ready queue (deterministic task ids): {}. \
             Choose the task id to work on next.",
            ready_task_ids.join(", ")
        );
        let criteria: Vec<(String, String)> = ready_task_ids
            .iter()
            .map(|task_id| (task_id.clone(), "ready task".to_string()))
            .collect();
        let answer = self
            .ask_choice(state, "pick", criteria, None)
            .await
            .map_err(|error| self.map_failure(error))?;
        if !ready_task_ids.contains(&answer.choice) {
            return Err(self.unavailable(format!(
                "selected task id '{}' is not in the ready queue",
                answer.choice
            )));
        }
        TaskSelection::new(answer.choice, answer.confidence)
            .map_err(|error| self.unavailable(format!("selection rejected: {error}")))
    }

    async fn propose_task_split(&self, task: &TaskSummary) -> DecisionResult<TaskSplitProposal> {
        let state = format!(
            "Task splitting decision. Task id: {}. Title: {}. Description: {}.",
            task.task_id, task.title, task.description
        );
        let should_split = self
            .ask_noul(
                state.clone(),
                "split",
                "Does this task need to be split into multiple independent subtasks?".to_string(),
            )
            .await
            .map_err(|error| self.map_failure(error))?;
        if should_split < 0.5 {
            return TaskSplitProposal::new(
                task.task_id.clone(),
                false,
                Vec::new(),
                (1.0 - should_split).min(1.0),
                Some("decision model scored the task as a single unit".to_string()),
            )
            .map_err(|error| self.unavailable(format!("split proposal rejected: {error}")));
        }
        // Split requested: name the split through a typed choice over a
        // fixed small vocabulary so the answer is validated, never free text.
        let answer = self
            .ask_choice(
                state,
                "shape",
                vec![
                    ("pair".to_string(), "two subtasks".to_string()),
                    ("triple".to_string(), "three subtasks".to_string()),
                ],
                Some("How many subtasks should the task be split into?".to_string()),
            )
            .await
            .map_err(|error| self.map_failure(error))?;
        let count = match answer.choice.as_str() {
            "pair" => 2,
            "triple" => 3,
            other => {
                return Err(self.unavailable(format!(
                    "split shape '{other}' is not in the shape vocabulary"
                )));
            }
        };
        let subtasks: Vec<TaskSplitSubtask> = (1..=count)
            .map(|index| TaskSplitSubtask {
                title: format!("subtask {index} of {}", task.task_id),
                rationale: "named by the decision model's split shape".to_string(),
            })
            .collect();
        TaskSplitProposal::new(
            task.task_id.clone(),
            true,
            subtasks,
            should_split.max(answer.confidence).min(1.0),
            Some(format!(
                "decision model scored split probability {should_split:.4}"
            )),
        )
        .map_err(|error| self.unavailable(format!("split proposal rejected: {error}")))
    }

    async fn propose_tool_selection(
        &self,
        query: &ToolQueryContext,
    ) -> DecisionResult<ToolSelection> {
        if query.available_tools.is_empty() {
            return Err(self.unavailable("no tools in the query context".to_string()));
        }
        let state = format!(
            "Tool discovery decision. Query: {}. Available tools: {}.",
            query.query,
            query
                .available_tools
                .iter()
                .map(|tool| format!("{} ({})", tool.name, tool.description))
                .collect::<Vec<_>>()
                .join("; ")
        );
        let criteria: Vec<(String, String)> = query
            .available_tools
            .iter()
            .map(|tool| (tool.name.clone(), tool.description.clone()))
            .collect();
        let answer = self
            .ask_choice(
                state,
                "pick",
                criteria,
                Some("Which tool applies?".to_string()),
            )
            .await
            .map_err(|error| self.map_failure(error))?;
        if !query
            .available_tools
            .iter()
            .any(|tool| tool.name == answer.choice)
        {
            return Err(self.unavailable(format!(
                "selected tool '{}' is not in the query context",
                answer.choice
            )));
        }
        ToolSelection::new(vec![answer.choice], answer.confidence)
            .map_err(|error| self.unavailable(format!("selection rejected: {error}")))
    }
}

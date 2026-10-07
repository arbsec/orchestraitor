//! [`DecisionProvider`] implementation for the Neuralwatt-hosted Clef Flash
//! decision model (spec `30-model-routing.md` §9.45).
//!
//! Clef Flash (Cloudflare, Apache-2.0, model id `clef-flash`) answers typed
//! questions about a state in a single `POST /v1/systemone` pass with
//! calibrated probabilities and zero generated text. The model id is
//! confirmed live on the authenticated `GET /v1/models` catalog
//! (`metadata.capabilities.task = "decision"`) and via a `200` from
//! `POST /v1/systemone` (2026-10-07).
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

use std::sync::Arc;

use async_trait::async_trait;
use orchestraitor_model::ProviderId;
use orchestraitor_provider_api::{
    DecisionAlternative, DecisionProposal, DecisionProvider, DecisionProviderError, DecisionResult,
    TaskSelection, TaskSplitProposal, TaskSplitSubtask, TaskSummary, ToolQueryContext,
    ToolSelection,
};
use secrecy::ExposeSecret;

use crate::config::NeuralwattConfig;
use crate::systemone::{
    SystemOneQuestion, SystemOneQuestionBody, SystemOneRequest, SystemOneResponse,
    parse_choice_answer, parse_noul_answer, question_body,
};
use crate::transport::{CONNECT_TIMEOUT, REQUEST_TIMEOUT};

/// Decision-provider id surfaced in decision records.
pub const NEURALWATT_DECISION_PROVIDER_ID: &str = "neuralwatt-clef-flash";

/// Default decision model id (confirmed live on the Neuralwatt catalog).
pub const DEFAULT_DECISION_MODEL: &str = "clef-flash";

/// Auth-URI sentinel for a no-auth decision endpoint (a self-hosted
/// deployment, spec §10.3): the config value `routing.api_key = "none"`
/// maps here, key resolution short-circuits to an empty secret, and the
/// request omits the `Authorization` header entirely. The sentinel never
/// appears in an error or log line.
pub(crate) const NO_AUTH_URI: &str = "secret://none";

/// Resolves the endpoint credential from the config's auth URI. The
/// no-auth sentinel short-circuits to an empty secret; a real URI follows
/// the standard `secret://` resolution chain. The resolved value never
/// enters an error or log line.
fn resolve_key(
    config: &NeuralwattConfig,
) -> Result<secrecy::SecretString, NeuralwattDecisionProviderError> {
    if config.auth_uri() == NO_AUTH_URI {
        return Ok(secrecy::SecretString::from(String::new()));
    }
    config
        .resolve_api_key()
        .map_err(|error| NeuralwattDecisionProviderError::Auth(error.to_string()))
}

/// Neuralwatt decision provider backed by the Clef Flash System One endpoint.
pub struct NeuralwattDecisionProvider {
    provider_id: ProviderId,
    http: reqwest::Client,
    config: NeuralwattConfig,
    api_key: secrecy::SecretString,
    decision_model: String,
}

impl NeuralwattDecisionProvider {
    /// Creates a decision provider with default configuration (default base
    /// URL and the `secret://keyring/neuralwatt` auth resolution chain) and
    /// the default decision model (`clef-flash`).
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattDecisionProviderError`] when the API key cannot
    /// be resolved or the HTTP client cannot be built.
    pub fn from_env() -> Result<Self, NeuralwattDecisionProviderError> {
        Self::from_config(NeuralwattConfig::new())
    }

    /// Creates a decision provider from explicit configuration with the
    /// default decision model (`clef-flash`).
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattDecisionProviderError`] when the API key cannot
    /// be resolved or the HTTP client cannot be built.
    pub fn from_config(config: NeuralwattConfig) -> Result<Self, NeuralwattDecisionProviderError> {
        let api_key = config
            .resolve_api_key()
            .map_err(|error| NeuralwattDecisionProviderError::Auth(error.to_string()))?;
        Self::with_key(config, api_key)
    }

    /// Creates a decision provider from explicit configuration with an
    /// explicit decision model id and an optional endpoint/credential
    /// override (spec `30-model-routing.md` §9.45 + §10.3):
    ///
    /// - `endpoint_override` — `Some` replaces the configuration's base URL
    ///   (a self-hosted decision engine, for example over tailscale); `None`
    ///   keeps `config`'s base URL.
    /// - `api_key_override` — `Some` replaces the configuration's auth URI
    ///   before resolution; `None` keeps `config`'s. When the override is
    ///   `"none"` the endpoint takes no auth and requests go out without an
    ///   `Authorization` header.
    /// - `decision_model` — the model id requests name.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattDecisionProviderError`] when the URL is invalid,
    /// the credential cannot be resolved, or the HTTP client cannot be
    /// built. Errors never carry the credential value.
    pub fn with_endpoint(
        config: NeuralwattConfig,
        endpoint_override: Option<String>,
        api_key_override: Option<&str>,
        decision_model: String,
    ) -> Result<Self, NeuralwattDecisionProviderError> {
        let config = match endpoint_override {
            Some(base_url) => config
                .with_base_url(base_url)
                .map_err(|error| NeuralwattDecisionProviderError::Auth(error.to_string()))?,
            None => config,
        };
        let config = match api_key_override {
            Some(uri) if uri.eq_ignore_ascii_case("none") => {
                config.with_auth_uri(NO_AUTH_URI.to_string())
            }
            Some(uri) => config.with_auth_uri(uri.to_string()),
            None => config,
        };
        let api_key = resolve_key(&config)?;
        Self::with_decision_model(config, api_key, decision_model)
    }

    /// Creates a decision provider from configuration with an explicit API
    /// key and the default decision model (`clef-flash`).
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattDecisionProviderError`] when the HTTP client
    /// cannot be built or the base URL is invalid.
    pub fn with_key(
        config: NeuralwattConfig,
        api_key: secrecy::SecretString,
    ) -> Result<Self, NeuralwattDecisionProviderError> {
        Self::with_decision_model(config, api_key, DEFAULT_DECISION_MODEL.to_string())
    }

    /// Creates a decision provider with an explicit decision model id.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattDecisionProviderError`] when the HTTP client
    /// cannot be built or the base URL is invalid.
    pub fn with_decision_model(
        config: NeuralwattConfig,
        api_key: secrecy::SecretString,
        decision_model: String,
    ) -> Result<Self, NeuralwattDecisionProviderError> {
        // URL validity is validated at configuration time (not per request)
        // so a malformed base URL fails before it can enter the campaign
        // loop; `NeuralwattConfig::with_endpoint` rejects unparseable and
        // forbidden hosts before this constructor runs.
        let _base = config
            .base_url()
            .parse::<url::Url>()
            .map_err(|source| NeuralwattDecisionProviderError::InvalidBaseUrl { source })?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|source| NeuralwattDecisionProviderError::HttpRequest { source })?;
        Ok(Self {
            provider_id: ProviderId::from_string(NEURALWATT_DECISION_PROVIDER_ID.to_string()),
            http,
            config,
            api_key,
            decision_model,
        })
    }

    /// Returns the decision model id requests are sent to.
    #[must_use]
    pub fn decision_model(&self) -> &str {
        &self.decision_model
    }

    /// Sends one System One request and returns the parsed response.
    async fn ask(
        &self,
        state: String,
        questions: Vec<SystemOneQuestion>,
    ) -> Result<SystemOneResponse, NeuralwattDecisionProviderError> {
        if questions.is_empty() {
            return Err(NeuralwattDecisionProviderError::NoQuestions);
        }
        let mut map = serde_json::Map::new();
        for question in questions {
            map.insert(question.name.clone(), question_body(&question.question));
        }
        let request = SystemOneRequest {
            model: self.decision_model.clone(),
            state,
            questions: map,
        };
        let url = format!("{}/systemone", self.config.base_url());
        let mut request = self.http.post(&url).json(&request);
        // A no-auth endpoint (self-hosted decision engine) sends no
        // `Authorization` header; the sentinel auth URI yields an empty
        // secret and the header is omitted entirely — never sent empty.
        if !self.api_key.expose_secret().is_empty() {
            request = request.header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            );
        }
        let response = request
            .send()
            .await
            .map_err(|source| NeuralwattDecisionProviderError::HttpRequest { source })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = response.text().await.unwrap_or_default();
            return Err(NeuralwattDecisionProviderError::ProviderStatus {
                status: status.as_u16(),
                // Log-safe excerpt only; the body never contains secrets.
                body_excerpt: body_excerpt.chars().take(200).collect(),
            });
        }
        response
            .json::<SystemOneResponse>()
            .await
            .map_err(|source| NeuralwattDecisionProviderError::ResponseParse { source })
    }

    /// Runs one `choice` question and returns the typed answer.
    async fn ask_choice(
        &self,
        state: String,
        name: &str,
        criteria: Vec<(String, String)>,
        instructions: Option<String>,
    ) -> Result<crate::systemone::SystemOneChoiceAnswer, NeuralwattDecisionProviderError> {
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
            NeuralwattDecisionProviderError::MissingAnswer {
                question: name.to_string(),
            }
        })?;
        parse_choice_answer(name, raw)
            .map_err(|reason| NeuralwattDecisionProviderError::MalformedAnswer { reason })
    }

    /// Runs one `noul` question and returns the probability.
    async fn ask_noul(
        &self,
        state: String,
        name: &str,
        instructions: String,
    ) -> Result<f64, NeuralwattDecisionProviderError> {
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
            NeuralwattDecisionProviderError::MissingAnswer {
                question: name.to_string(),
            }
        })?;
        let answer = parse_noul_answer(name, raw)
            .map_err(|reason| NeuralwattDecisionProviderError::MalformedAnswer { reason })?;
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
    fn map_failure(&self, error: NeuralwattDecisionProviderError) -> DecisionProviderError {
        match error {
            NeuralwattDecisionProviderError::MalformedAnswer { reason }
            | NeuralwattDecisionProviderError::MissingAnswer { question: reason } => {
                self.unavailable(reason)
            }
            other => self.unavailable(other.to_string()),
        }
    }
}

/// Failures building or sending a [`NeuralwattDecisionProvider`] request.
/// The caller-facing [`DecisionProviderError`] never carries secret material;
/// this type carries only log-safe excerpts.
#[derive(Debug, thiserror::Error)]
pub enum NeuralwattDecisionProviderError {
    /// The API key could not be resolved from the configured auth URI.
    #[error("neuralwatt decision auth resolution failed: {0}")]
    Auth(String),
    /// The configured base URL could not be parsed.
    #[error("invalid neuralwatt base URL: {source}")]
    InvalidBaseUrl {
        /// Underlying URL parse error.
        #[source]
        source: url::ParseError,
    },
    /// The HTTP client could not be built or the request failed.
    #[error("neuralwatt decision HTTP request failed: {source}")]
    HttpRequest {
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },
    /// The System One endpoint returned a non-success status.
    #[error("neuralwatt decision endpoint returned HTTP {status}")]
    ProviderStatus {
        /// HTTP status code.
        status: u16,
        /// Log-safe response-body excerpt.
        body_excerpt: String,
    },
    /// The response body could not be parsed.
    #[error("neuralwatt decision response parse failed: {source}")]
    ResponseParse {
        /// Underlying JSON error.
        #[source]
        source: reqwest::Error,
    },
    /// The response omitted the answer for a posed question.
    #[error("neuralwatt decision response omitted answer for `{question}`")]
    MissingAnswer {
        /// Question name whose answer was missing.
        question: String,
    },
    /// An answer had the wrong shape for its question type.
    #[error("neuralwatt decision answer malformed: {reason}")]
    MalformedAnswer {
        /// Log-safe reason.
        reason: String,
    },
    /// A request was attempted without any question (programmer error).
    #[error("neuralwatt decision request posed no questions")]
    NoQuestions,
}

#[async_trait]
impl DecisionProvider for NeuralwattDecisionProvider {
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

/// Convenience alias so `with_cost_sink`-style builders can stay on the
/// shared transport crate without re-importing.
pub type SharedDecisionProvider = Arc<NeuralwattDecisionProvider>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
}

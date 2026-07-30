//! Neuralwatt GLM-5.2 BYOK provider adapter for Orchestraitor.
//!
//! This crate implements [`ProviderTransport`] against the Neuralwatt
//! `OpenAI` Chat Completions-compatible API (spec §10.3). Neuralwatt with
//! GLM-5.2 is the initial real-world BYOK compatibility target.
//!
//! # Endpoints
//!
//! | Endpoint | Operator | Notes |
//! |---|---|---|
//! | `https://api.neuralwatt.com/v1` | Neuralwatt | **Default MVP target.** `OpenAI` Chat Completions shape. |
//! | `https://api.z.ai/api/paas/v4/` | Z.ai | Alternate endpoint; same underlying model. |
//!
//! The legacy Zhipu endpoint `https://open.bigmodel.cn/api/paas/v4/` MUST NOT
//! be used as a default (spec §10.3). This crate rejects that host at
//! configuration time.
//!
//! # Authentication
//!
//! API keys are resolved from `secret://keyring/neuralwatt` (preferred) or
//! `secret://env/NEURALWATT_API_KEY` (fallback), following the models.dev
//! `env` convention (tech-stack §3.2). The key never enters a serialized
//! serde stream and is never logged.
//!
//! # Streaming
//!
//! Streaming responses are consumed via [`reqwest::Response::bytes_stream`]
//! and parsed as Server-Sent Events into [`ModelEvent`] values. Non-streaming
//! responses are parsed from a single JSON body.
//!
//! # Cost accounting
//!
//! Per-call cost entries are emitted per spec §9.19.4 through a [`CostSink`].
//! The ledger keeps metered API spend separate from subscription
//! utilization.

#![forbid(unsafe_code)]

pub mod config;
pub mod cost;
pub mod error;
pub mod stream;
pub mod transport;
pub mod wire;

pub use config::{DEFAULT_NEURALWATT_BASE_URL, NEURALWATT_ENV_VAR, NeuralwattConfig};
pub use cost::{CostSink, InMemoryCostSink, LedgerCostSink};
pub use error::NeuralwattError;
pub use transport::NeuralwattTransport;
pub use wire::ChatCompletionRequest;

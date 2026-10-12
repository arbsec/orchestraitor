//! Deterministic provider simulator and fixtures for Orchestraitor tests
//! (spec §21.3). CI MUST never depend on a live model provider: every client
//! behavior is verified against this simulator.
//!
//! This crate currently implements the `OpenAI` Chat Completions surface
//! (non-streaming, streaming, structured output). Other surfaces (Responses
//! API, Anthropic Messages, latency/cancellation/adversarial scripts) are
//! tracked by follow-up issues.

pub mod openai;

pub use openai::{CapturedRequest, OpenAiMockServer, PlannedResponse};

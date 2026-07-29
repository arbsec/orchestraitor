//! Serializable domain types for Orchestraitor.
//!
//! This crate defines the type vocabulary used across the Orchestraitor
//! workspace. It contains NO I/O, NO tokio, and NO security logic — just
//! types that round-trip through `serde_json`.
//!
//! See `docs/spec/spec.md` §18 for the authoritative data model.

pub mod context;
pub mod digest;
pub mod enums;
pub mod error_codes;
pub mod ids;
pub mod promotion;
pub mod repository;
pub mod session;
pub mod workspace;

pub use context::ContextReceipt;
pub use digest::Digest;
pub use enums::*;
pub use ids::*;
pub use promotion::PromotionReceipt;
pub use repository::Repository;
pub use session::Session;
pub use workspace::Workspace;

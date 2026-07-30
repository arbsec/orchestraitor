//! Command-line entry points for Orchestraitor.

#![forbid(unsafe_code)]

pub mod cli;
mod detection;
pub mod init;
mod render;
mod scanner;

pub use cli::{Cli, Commands};

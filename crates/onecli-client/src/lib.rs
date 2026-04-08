//! OneCLI Client Library

pub mod client;
pub mod config;
pub mod error;

pub use client::OneCliClient;
pub use config::{OneCliConfig, OneCliServiceConfig};
pub use error::{OneCliError, Result};

//! Integration tests for ZeroClawed + OneCLI system
//!
//! These tests verify the full credential proxy chain:
//! Agent → OneCLI → LLM API (with credential injection)

pub mod adapter_edge_cases;
pub mod config_sanity;
pub mod onecli_proxy;
pub mod property_tests;
pub mod security_tests;

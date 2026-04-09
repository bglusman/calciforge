//! Authentication and identity resolution from mTLS client certificates

mod adapter;
mod identity;

pub use adapter::AgentRegistry;
pub use identity::{build_identity, is_cert_revoked, ClientIdentity};

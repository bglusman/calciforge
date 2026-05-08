//! Authentication and identity resolution from mTLS client certificates

mod adapter;
mod identity;

pub use adapter::AgentRegistry;
pub use identity::{ClientIdentity, build_identity, is_cert_revoked};

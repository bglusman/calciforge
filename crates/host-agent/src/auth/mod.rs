//! Authentication and identity resolution from mTLS client certificates

mod adapter;
mod identity;

pub use adapter::AgentRegistry;
pub use identity::{build_identity, cert_fingerprint, is_cert_revoked, ClientIdentity};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use tracing::{debug, warn};

/// mTLS authentication middleware
///
/// This middleware expects ClientIdentity to be injected by the TLS acceptor.
/// In axum-server with rustls, we use a custom acceptor to extract certs
/// and inject them into the request extensions.
pub async fn auth_middleware(
    State(_state): State<Arc<crate::AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // Check if identity was injected by TLS layer
    if let Some(identity) = request.extensions().get::<ClientIdentity>() {
        debug!(cn = %identity.cn, uid = %identity.uid, "Authenticated request");
    } else {
        // This should not happen if mTLS is properly enforced
        warn!("Request without ClientIdentity - mTLS may be misconfigured");
        return Response::builder()
            .status(401)
            .body(Body::from("Authentication required"))
            .unwrap();
    }

    next.run(request).await
}

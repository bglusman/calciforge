//! Error types for Host-Agent

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Invalid dataset name: {0}")]
    InvalidDataset(String),

    #[error("Invalid snapshot name: {0}")]
    InvalidSnapshotName(String),

    #[error("ZFS operation failed: {0}")]
    Zfs(#[from] ZfsError),

    #[error("Invalid approval token")]
    InvalidToken,

    #[error("Approval error: {0}")]
    Approval(#[from] ApprovalError),

    #[error("Audit log error: {0}")]
    Audit(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Identity resolution failed: {0}")]
    Identity(String),

    #[error("Policy denied: {0}")]
    PolicyDenied(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

// Implement From<anyhow::Error> for AppError to handle audit logging errors
impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Audit(e.to_string())
    }
}

#[derive(Error, Debug)]
pub enum ZfsError {
    #[error("Command execution failed: {0}")]
    Execution(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Dataset not found: {0}")]
    DatasetNotFound(String),

    #[error("Dataset is busy: {0}")]
    DatasetBusy(String),

    #[error("Invalid operation: {0}")]
    InvalidOperation(String),
}

#[derive(Error, Debug)]
pub enum ApprovalError {
    #[error("Approval request not found: {0}")]
    NotFound(String),

    #[error("Token expired: {0}")]
    Expired(String),

    #[error("Invalid token for approval")]
    InvalidToken,

    #[error("Approval already used")]
    AlreadyUsed,

    #[error("Approval not yet granted via Signal")]
    NotYetApproved,

    #[error("Unauthorized approver: {0}")]
    UnauthorizedApprover(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_message) = match &self {
            AppError::InvalidDataset(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::InvalidSnapshotName(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Zfs(e) => match e {
                ZfsError::PermissionDenied(_) => (StatusCode::FORBIDDEN, self.to_string()),
                ZfsError::DatasetNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
                ZfsError::DatasetBusy(_) => (StatusCode::CONFLICT, self.to_string()),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            },
            AppError::InvalidToken => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Approval(e) => match e {
                ApprovalError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
                ApprovalError::Expired(_) => (StatusCode::GONE, self.to_string()),
                ApprovalError::AlreadyUsed => (StatusCode::CONFLICT, self.to_string()),
                ApprovalError::NotYetApproved => (StatusCode::ACCEPTED, self.to_string()),
                ApprovalError::UnauthorizedApprover(_) => (StatusCode::FORBIDDEN, self.to_string()),
                _ => (StatusCode::BAD_REQUEST, self.to_string()),
            },
            AppError::Audit(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::Tls(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::Auth(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Identity(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::PolicyDenied(_) => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        let body = json!({
            "error": error_message,
            "success": false,
        });

        (status, axum::Json(body)).into_response()
    }
}

/// Standard error response format
#[derive(Debug, serde::Serialize)]
pub struct ErrorResponse {
    pub success: bool,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zfs_error_status_mapping() {
        let err = AppError::Zfs(ZfsError::PermissionDenied("test".to_string()));
        // Just verify it compiles and has IntoResponse
        let _ = err.into_response();
    }

    #[test]
    fn test_approval_error_status_mapping() {
        let err = AppError::Approval(ApprovalError::Expired("token".to_string()));
        let _ = err.into_response();
    }
}

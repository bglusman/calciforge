use axum::http::{HeaderMap, StatusCode};

use crate::config::ProxyConfig;

pub(crate) struct AuthError {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

pub(crate) fn require_secret_discovery_api_key(
    config: &ProxyConfig,
    headers: &HeaderMap,
) -> Result<(), AuthError> {
    let Some(expected_key) = config.secret_discovery_api_key.as_deref().map(str::trim) else {
        return Err(AuthError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "secret_discovery_auth_not_configured",
            message: "Secret discovery API requires proxy.secret_discovery_api_key or proxy.secret_discovery_api_key_file",
        });
    };
    require_bearer(
        expected_key,
        headers,
        "secret_discovery_auth_not_configured",
        "Invalid secret discovery API key",
    )
}

pub(crate) fn require_control_api_key(
    config: &ProxyConfig,
    headers: &HeaderMap,
) -> Result<(), AuthError> {
    let Some(expected_key) = config.secret_control_api_key.as_deref().map(str::trim) else {
        return Err(AuthError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "control_api_auth_not_configured",
            message: "Secret control API requires proxy.secret_control_api_key or proxy.secret_control_api_key_file",
        });
    };
    require_bearer(
        expected_key,
        headers,
        "control_api_auth_not_configured",
        "Invalid secret control API key",
    )
}

fn require_bearer(
    expected_key: &str,
    headers: &HeaderMap,
    empty_key_code: &'static str,
    unauthorized_message: &'static str,
) -> Result<(), AuthError> {
    if expected_key.is_empty() {
        return Err(AuthError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: empty_key_code,
            message: "Secret API requires a non-empty configured key",
        });
    }
    if bearer_token(headers) == Some(expected_key) {
        Ok(())
    } else {
        Err(AuthError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: unauthorized_message,
        })
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| {
            let mut parts = s.trim().splitn(2, char::is_whitespace);
            let scheme = parts.next()?;
            let token = parts.next()?.trim();
            if scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty() {
                Some(token)
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn config_with_secret_discovery_key(key: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            secret_discovery_api_key: key.map(str::to_string),
            ..Default::default()
        }
    }

    fn config_with_secret_control_key(key: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            secret_control_api_key: key.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn discovery_api_fails_closed_without_configured_key() {
        assert_eq!(
            require_secret_discovery_api_key(
                &config_with_secret_discovery_key(None),
                &HeaderMap::new()
            )
            .unwrap_err()
            .status,
            StatusCode::SERVICE_UNAVAILABLE
        );

        let err = require_secret_discovery_api_key(
            &config_with_secret_discovery_key(Some("  ")),
            &HeaderMap::new(),
        )
        .unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code, "secret_discovery_auth_not_configured");
    }

    #[test]
    fn discovery_api_rejects_control_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer control-key"),
        );

        assert_eq!(
            require_secret_discovery_api_key(
                &config_with_secret_discovery_key(Some("discovery-key")),
                &headers,
            )
            .unwrap_err()
            .status,
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn discovery_api_accepts_valid_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer discovery-key"),
        );
        assert!(
            require_secret_discovery_api_key(
                &config_with_secret_discovery_key(Some("discovery-key")),
                &headers,
            )
            .is_ok()
        );
    }

    #[test]
    fn control_api_fails_closed_without_configured_key() {
        assert_eq!(
            require_control_api_key(&config_with_secret_control_key(None), &HeaderMap::new())
                .unwrap_err()
                .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn control_api_accepts_valid_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer test-key"));
        assert!(
            require_control_api_key(&config_with_secret_control_key(Some("test-key")), &headers)
                .is_ok()
        );
    }
}

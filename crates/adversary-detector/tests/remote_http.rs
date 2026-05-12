use adversary_detector::{
    AdversaryScanner, ScanContext, ScanVerdict, ScannerCheckConfig, ScannerConfig,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn remote_http_check_can_fail_closed() {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("reserve an unused local test port");
    let unavailable_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let scanner = AdversaryScanner::new(ScannerConfig {
        checks: vec![ScannerCheckConfig::RemoteHttp {
            url: unavailable_url,
            fail_closed: true,
        }],
        ..Default::default()
    });

    let verdict = scanner
        .scan(
            "https://example.com",
            "ordinary content",
            ScanContext::WebFetch,
        )
        .await;

    assert!(
        verdict.is_unsafe(),
        "fail_closed remote check should block when service is unavailable"
    );
}

#[tokio::test]
async fn remote_http_check_can_block_clean_local_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/scan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "verdict": "unsafe",
            "reason": "custom classifier blocked this content",
        })))
        .mount(&server)
        .await;

    let scanner = AdversaryScanner::new(ScannerConfig {
        checks: vec![ScannerCheckConfig::RemoteHttp {
            url: server.uri(),
            fail_closed: true,
        }],
        ..Default::default()
    });

    let verdict = scanner
        .scan("https://example.com", "ordinary content", ScanContext::Api)
        .await;

    assert!(matches!(
        verdict,
        ScanVerdict::Unsafe { reason } if reason == "custom classifier blocked this content"
    ));
}

#[tokio::test]
async fn remote_http_check_uses_sanitized_payload() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/scan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "verdict": "clean",
            "reason": null,
        })))
        .mount(&server)
        .await;

    let scanner = AdversaryScanner::new(ScannerConfig {
        checks: vec![ScannerCheckConfig::RemoteHttp {
            url: server.uri(),
            fail_closed: true,
        }],
        ..Default::default()
    });

    let secret_value = "REMOTE-SCANNER-MUST-NOT-SEE-THIS";
    let verdict = scanner
        .scan_with_remote_payload(
            "https://example.com/_redacted_",
            &format!(r#"{{"token":"{secret_value}"}}"#),
            "https://example.com/_redacted_",
            r#"{"token":"{{secret:API_TOKEN}}"}"#,
            ScanContext::Api,
        )
        .await;

    assert_eq!(verdict, ScanVerdict::Clean);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["content"], r#"{"token":"{{secret:API_TOKEN}}"}"#);
    assert!(
        !body.to_string().contains(secret_value),
        "remote scanner request must not contain substituted secret values"
    );
}

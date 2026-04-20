//! Scanner unit tests — error cases and edge conditions

#[cfg(test)]
mod tests {
    use super::*;

    /// Test scanner with empty/whitespace content
    #[tokio::test]
    async fn test_scan_empty_body() {
        let scanner = ExfilScanner::new();
        let report = scanner.scan("https://api.openai.com/v1/chat/completions", "").await;
        assert!(matches!(report.verdict, Verdict::Allow));
        assert!(report.findings.is_empty());
    }

    /// Test scanner with very large payload
    #[tokio::test]
    async fn test_scan_large_payload() {
        let scanner = ExfilScanner::new();
        let large_body = "x".repeat(1024 * 1024); // 1MB
        let report = scanner.scan("https://api.openai.com/v1/chat/completions", &large_body).await;
        // Should not panic or hang
        assert!(matches!(report.verdict, Verdict::Allow));
    }

    /// Test scanner with malformed URL
    #[tokio::test]
    async fn test_scan_malformed_url() {
        let scanner = InjectionScanner::new();
        let report = scanner.scan("not-a-valid-url", "clean content").await;
        // Should handle gracefully
        assert!(matches!(report.verdict, Verdict::Allow | Verdict::Log { .. }));
    }

    /// Test scanner with unicode-heavy content
    #[tokio::test]
    async fn test_scan_unicode_content() {
        let scanner = InjectionScanner::new();
        let body = "こんにちは世界 🌍 مرحبا بالعالم 👋";
        let report = scanner.scan("https://example.com", body).await;
        assert!(matches!(report.verdict, Verdict::Allow));
    }

    /// Test scan timing is reasonable
    #[tokio::test]
    async fn test_scan_performance_sanity() {
        let scanner = ExfilScanner::new();
        let body = "Normal API request body".repeat(100);
        
        let start = std::time::Instant::now();
        let _report = scanner.scan("https://api.openai.com/v1/chat/completions", &body).await;
        let elapsed = start.elapsed();
        
        // Scan should complete in reasonable time (<1s for simple content)
        assert!(elapsed < std::time::Duration::from_secs(1), 
            "Scan took too long: {:?}", elapsed);
    }

    /// Test concurrent scanning doesn't deadlock
    #[tokio::test]
    async fn test_concurrent_scans() {
        use tokio::task::JoinSet;
        
        let scanner = std::sync::Arc::new(ExfilScanner::new());
        let mut set = JoinSet::new();
        
        for i in 0..10 {
            let scanner_clone = scanner.clone();
            set.spawn(async move {
                let body = format!("Request body {}", i);
                scanner_clone.scan("https://api.openai.com/v1/chat/completions", &body).await
            });
        }
        
        let mut count = 0;
        while let Some(result) = set.join_next().await {
            let _report = result.expect("Task should not panic");
            count += 1;
        }
        
        assert_eq!(count, 10);
    }
}
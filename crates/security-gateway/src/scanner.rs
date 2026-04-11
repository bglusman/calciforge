use std::time::Instant;

use adversary_detector::{AdversaryScanner, ScanContext, ScanVerdict, ScannerConfig};

use crate::config::{ExfilReport, InjectionReport, Verdict};

/// Outbound traffic scanner — checks for data exfiltration, PII leakage, secrets.
pub struct ExfilScanner {
    scanner: AdversaryScanner,
}

impl Default for ExfilScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ExfilScanner {
    pub fn new() -> Self {
        let config = ScannerConfig::default();
        Self {
            scanner: AdversaryScanner::new(config),
        }
    }

    /// Scan outbound request body for exfiltration signals.
    pub async fn scan(&self, url: &str, body: &str) -> ExfilReport {
        let start = Instant::now();
        let ctx = ScanContext::Api;

        let verdict = self.scanner.scan(url, body, ctx).await;
        let findings = Self::extract_findings(&verdict);

        let (final_verdict, findings_str) = match verdict {
            ScanVerdict::Clean => (Verdict::Allow, findings),
            ScanVerdict::Review { reason } => (
                Verdict::Log {
                    finding: reason.clone(),
                },
                vec![reason],
            ),
            ScanVerdict::Unsafe { reason } => (
                Verdict::Block {
                    reason: reason.clone(),
                },
                vec![reason],
            ),
        };

        ExfilReport {
            verdict: final_verdict,
            findings: findings_str,
            scan_time_ms: start.elapsed().as_millis() as u64,
        }
    }

    fn extract_findings(verdict: &ScanVerdict) -> Vec<String> {
        match verdict {
            ScanVerdict::Clean => vec![],
            ScanVerdict::Review { reason } => vec![reason.clone()],
            ScanVerdict::Unsafe { reason } => vec![reason.clone()],
        }
    }
}

/// Inbound traffic scanner — checks response content for prompt injection.
pub struct InjectionScanner {
    scanner: AdversaryScanner,
}

impl Default for InjectionScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl InjectionScanner {
    pub fn new() -> Self {
        let config = ScannerConfig::default();
        Self {
            scanner: AdversaryScanner::new(config),
        }
    }

    /// Scan inbound response for prompt injection attempts.
    pub async fn scan(&self, url: &str, body: &str) -> InjectionReport {
        let start = Instant::now();
        let ctx = ScanContext::WebFetch;

        let verdict = self.scanner.scan(url, body, ctx).await;

        let (final_verdict, findings_str) = match verdict {
            ScanVerdict::Clean => (Verdict::Allow, vec![]),
            ScanVerdict::Review { reason } => (
                Verdict::Log {
                    finding: reason.clone(),
                },
                vec![reason],
            ),
            ScanVerdict::Unsafe { reason } => (
                Verdict::Block {
                    reason: format!("Response contains adversarial content: {}", reason),
                },
                vec![reason],
            ),
        };

        InjectionReport {
            verdict: final_verdict,
            findings: findings_str,
            scan_time_ms: start.elapsed().as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test scanner with empty/whitespace content
    #[tokio::test]
    async fn test_scan_empty_body() {
        let scanner = ExfilScanner::new();
        let report = scanner
            .scan("https://api.openai.com/v1/chat/completions", "")
            .await;
        assert!(matches!(report.verdict, Verdict::Allow));
        assert!(report.findings.is_empty());
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
        let _report = scanner
            .scan("https://api.openai.com/v1/chat/completions", &body)
            .await;
        let elapsed = start.elapsed();

        // Scan should complete in reasonable time (<1s for simple content)
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "Scan took too long: {:?}",
            elapsed
        );
    }
}

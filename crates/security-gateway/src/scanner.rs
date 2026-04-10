use std::time::Instant;

use adversary_detector::{OutpostScanner, OutpostVerdict, ScanContext, ScannerConfig};

use crate::config::{ExfilReport, InjectionReport, Verdict};

/// Outbound traffic scanner — checks for data exfiltration, PII leakage, secrets.
pub struct ExfilScanner {
    scanner: OutpostScanner,
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
            scanner: OutpostScanner::new(config),
        }
    }

    /// Scan outbound request body for exfiltration signals.
    pub async fn scan(&self, url: &str, body: &str) -> ExfilReport {
        let start = Instant::now();
        let ctx = ScanContext::Api;

        let verdict = self.scanner.scan(url, body, ctx).await;
        let findings = Self::extract_findings(&verdict);

        let (final_verdict, findings_str) = match verdict {
            OutpostVerdict::Clean => (Verdict::Allow, findings),
            OutpostVerdict::Review { reason } => (
                Verdict::Log {
                    finding: reason.clone(),
                },
                vec![reason],
            ),
            OutpostVerdict::Unsafe { reason } => (
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

    fn extract_findings(verdict: &OutpostVerdict) -> Vec<String> {
        match verdict {
            OutpostVerdict::Clean => vec![],
            OutpostVerdict::Review { reason } => vec![reason.clone()],
            OutpostVerdict::Unsafe { reason } => vec![reason.clone()],
        }
    }
}

/// Inbound traffic scanner — checks response content for prompt injection.
pub struct InjectionScanner {
    scanner: OutpostScanner,
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
            scanner: OutpostScanner::new(config),
        }
    }

    /// Scan inbound response for prompt injection attempts.
    pub async fn scan(&self, url: &str, body: &str) -> InjectionReport {
        let start = Instant::now();
        let ctx = ScanContext::WebFetch;

        let verdict = self.scanner.scan(url, body, ctx).await;

        let (final_verdict, findings_str) = match verdict {
            OutpostVerdict::Clean => (Verdict::Allow, vec![]),
            OutpostVerdict::Review { reason } => (
                Verdict::Log {
                    finding: reason.clone(),
                },
                vec![reason],
            ),
            OutpostVerdict::Unsafe { reason } => (
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

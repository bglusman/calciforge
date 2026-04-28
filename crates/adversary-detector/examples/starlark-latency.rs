use adversary_detector::{AdversaryScanner, ScanContext, ScannerCheckConfig, ScannerConfig};
use std::time::Instant;

#[tokio::main]
async fn main() {
    let policy = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "builtin:calciforge/default-scanner.star".to_string());
    let iterations: u32 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000);

    let scanner = AdversaryScanner::new(ScannerConfig {
        checks: vec![ScannerCheckConfig::Starlark {
            path: policy,
            fail_closed: true,
            max_callstack: 64,
        }],
        ..Default::default()
    });

    let content = "normal agent traffic with no custom policy finding";
    let _ = scanner
        .scan("https://api.example.com/v1/chat", content, ScanContext::Api)
        .await;

    let started = Instant::now();
    for _ in 0..iterations {
        let _ = scanner
            .scan("https://api.example.com/v1/chat", content, ScanContext::Api)
            .await;
    }
    let elapsed = started.elapsed();
    let average = elapsed / iterations;

    println!(
        "starlark scans: {iterations}, total: {:?}, average: {:?}",
        elapsed, average
    );
}

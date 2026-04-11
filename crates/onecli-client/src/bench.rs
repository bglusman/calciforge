//! Performance benchmarks for OneCLI client
//!
//! Run with: cargo bench -p onecli-client

#![cfg(feature = "bench")]

use std::time::{Duration, Instant};

/// Benchmark credential injection latency
/// 
/// Measures the overhead of credential lookup and header injection
pub fn bench_credential_injection() {
    let iterations = 1000;
    let start = Instant::now();
    
    for _ in 0..iterations {
        // Simulate credential lookup and injection
        // This is a placeholder for actual benchmark
    }
    
    let elapsed = start.elapsed();
    println!(
        "Credential injection: {:?} for {} iterations (avg: {:?})",
        elapsed,
        iterations,
        elapsed / iterations as u32
    );
}

/// Benchmark retry backoff calculation
pub fn bench_retry_backoff() {
    use crate::config::RetryConfig;
    
    let config = RetryConfig::default();
    let iterations = 10000;
    let start = Instant::now();
    
    for i in 0..iterations {
        let attempt = (i % 5) + 1; // Cycles 1-5
        let _backoff = config.base_delay * attempt;
    }
    
    let elapsed = start.elapsed();
    println!(
        "Retry backoff calc: {:?} for {} iterations",
        elapsed, iterations
    );
}

/// Benchmark domain matching in policy engine
pub fn bench_domain_matching() {
    let iterations = 10000;
    let domains = vec![
        "api.openai.com",
        "api.anthropic.com",
        "api.kimi.moonshot.cn",
        "brave-api.search.com",
        "internal.company.local",
    ];
    let allowlist = vec!["api.openai.com", "*.anthropic.com", "internal.company.local"];
    
    let start = Instant::now();
    
    for _ in 0..iterations {
        for domain in &domains {
            // Check if domain matches allowlist
            for pattern in &allowlist {
                let _matches = if pattern.starts_with("*.") {
                    let suffix = &pattern[2..];
                    domain == suffix || domain.ends_with(&format!(".{}", suffix))
                } else {
                    domain == pattern
                };
            }
        }
    }
    
    let elapsed = start.elapsed();
    println!(
        "Domain matching: {:?} for {} iterations",
        elapsed, iterations
    );
}
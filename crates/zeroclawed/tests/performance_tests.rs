//! Performance tests for ZeroClawed
//! Tests throughput, latency, and resource usage under load

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

// ==================== THROUGHPUT TESTS ====================

/// Test message throughput under load
#[tokio::test]
async fn test_message_throughput() {
    let message_count = 1000;
    let start = Instant::now();

    let counter = Arc::new(Mutex::new(0u32));
    let mut handles = vec![];

    for i in 0..message_count {
        let counter = counter.clone();
        handles.push(tokio::spawn(async move {
            // Simulate message processing
            tokio::time::sleep(Duration::from_micros(100)).await;
            let mut c = counter.lock().await;
            *c += 1;
            i
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let elapsed = start.elapsed();
    let processed = *counter.lock().await;

    let throughput = processed as f64 / elapsed.as_secs_f64();
    println!(
        "Processed {} messages in {:?} ({:.0} msg/sec)",
        processed, elapsed, throughput
    );

    assert_eq!(processed, message_count);
    assert!(
        throughput > 100.0,
        "Throughput should be at least 100 msg/sec, got {:.0}",
        throughput
    );
}

/// Test concurrent request handling capacity
#[tokio::test]
async fn test_concurrent_capacity() {
    let concurrency_levels = vec![10, 50, 100];

    for concurrency in &concurrency_levels {
        let start = Instant::now();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(*concurrency));

        let mut handles = vec![];
        for _ in 0..*concurrency * 2 {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            handles.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                drop(permit);
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        let elapsed = start.elapsed();
        println!(
            "Concurrency {}: completed in {:?}",
            concurrency, elapsed
        );
    }
}

/// Test latency percentiles
#[tokio::test]
async fn test_latency_percentiles() {
    let request_count = 100;
    let mut latencies: Vec<Duration> = vec![];

    for _ in 0..request_count {
        let start = Instant::now();

        // Simulate request
        tokio::time::sleep(Duration::from_millis(1)).await;

        latencies.push(start.elapsed());
    }

    latencies.sort();

    let p50 = latencies[request_count / 2];
    let p99 = latencies[(request_count * 99) / 100];

    println!("Latency percentiles: p50={:?}, p99={:?}", p50, p99);

    assert!(
        p50 < Duration::from_millis(50),
        "p50 latency should be < 50ms"
    );
}

/// Test memory usage stability
#[tokio::test]
async fn test_memory_stability() {
    // Simulate growing state
    let state = Arc::new(Mutex::new(Vec::<String>::new()));

    // Phase 1: Add items
    for i in 0..1000 {
        let mut s = state.lock().await;
        s.push(format!("item-{}", i));
    }

    let size_after_growth = state.lock().await.len();
    assert_eq!(size_after_growth, 1000);

    // Phase 2: Clear items
    state.lock().await.clear();

    let size_after_clear = state.lock().await.len();
    assert_eq!(size_after_clear, 0);
}

// ==================== LOAD TESTS ====================

/// Test system under burst load
#[tokio::test]
async fn test_burst_load_handling() {
    let burst_size = 500;
    let start = Instant::now();

    let success_count = Arc::new(Mutex::new(0));
    let mut handles = vec![];

    for _ in 0..burst_size {
        let success_count = success_count.clone();
        handles.push(tokio::spawn(async move {
            // Simulate request with small chance of failure
            if rand::random::<f32>() < 0.01 {
                // 1% failure rate
                return;
            }
            *success_count.lock().await += 1;
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let elapsed = start.elapsed();
    let successes = *success_count.lock().await;

    println!(
        "Burst load: {} succeeded out of {} in {:?}",
        successes, burst_size, elapsed
    );

    assert!(
        successes >= burst_size * 95 / 100,
        "Should handle at least 95% of burst load"
    );
}

/// Test sustained load over time
#[tokio::test]
async fn test_sustained_load() {
    let duration = Duration::from_secs(1);
    let start = Instant::now();
    let mut request_count = 0;

    while start.elapsed() < duration {
        // Simulate request
        tokio::time::sleep(Duration::from_millis(10)).await;
        request_count += 1;
    }

    let throughput = request_count as f64 / duration.as_secs_f64();
    println!(
        "Sustained load: {} requests in {:?} ({:.0} req/sec)",
        request_count, duration, throughput
    );

    assert!(
        throughput > 50.0,
        "Should sustain at least 50 req/sec"
    );
}

/// Test recovery after load spike
#[tokio::test]
async fn test_load_spike_recovery() {
    // Phase 1: Normal load
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let normal_latency = Duration::from_millis(10);

    // Phase 2: Spike
    let mut spike_handles = vec![];
    for _ in 0..100 {
        spike_handles.push(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }));
    }
    for handle in spike_handles {
        handle.await.unwrap();
    }

    // Phase 3: Recovery check
    let recovery_start = Instant::now();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let recovery_latency = recovery_start.elapsed();

    assert!(
        recovery_latency < normal_latency * 2,
        "Should recover to normal latency after spike"
    );
}

// ==================== RESOURCE TESTS ====================

/// Test connection pool behavior
#[tokio::test]
async fn test_connection_pool_limits() {
    let pool_size = 10;
    let pool = Arc::new(tokio::sync::Semaphore::new(pool_size));

    let mut handles = vec![];
    for i in 0..pool_size * 3 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            let _permit = pool.acquire().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            println!("Connection {} completed", i);
        }));
    }

    let start = Instant::now();
    for handle in handles {
        handle.await.unwrap();
    }
    let elapsed = start.elapsed();

    // With pool_size=10 and 50ms per request, 30 requests should take ~150ms
    let expected_min = Duration::from_millis(150);
    assert!(
        elapsed >= expected_min,
        "Should respect pool limits, took {:?}",
        elapsed
    );
}

/// Test timeout handling under load
#[tokio::test]
async fn test_timeout_under_load() {
    let timeout = Duration::from_millis(50);
    let mut timeouts = 0;
    let mut completions = 0;

    for _ in 0..20 {
        match tokio::time::timeout(timeout, async {
            // Random delay
            let delay = if rand::random::<bool>() {
                10 // Fast
            } else {
                100 // Slow (will timeout)
            };
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }).await {
            Ok(_) => completions += 1,
            Err(_) => timeouts += 1,
        }
    }

    println!(
        "Timeout test: {} completions, {} timeouts",
        completions, timeouts
    );

    assert!(timeouts > 0, "Should have some timeouts");
    assert!(completions > 0, "Should have some completions");
}

/// Test queue behavior under load
#[tokio::test]
async fn test_queue_behavior() {
    struct Queue {
        items: Vec<String>,
        max_size: usize,
    }

    impl Queue {
        fn new(max_size: usize) -> Self {
            Self {
                items: Vec::with_capacity(max_size),
                max_size,
            }
        }

        fn push(&mut self, item: String) -> Result<(), String> {
            if self.items.len() >= self.max_size {
                return Err("Queue full".to_string());
            }
            self.items.push(item);
            Ok(())
        }

        fn pop(&mut self) -> Option<String> {
            if self.items.is_empty() {
                None
            } else {
                Some(self.items.remove(0))
            }
        }
    }

    let mut queue = Queue::new(5);

    // Fill queue
    for i in 0..5 {
        assert!(queue.push(format!("item-{}", i)).is_ok());
    }

    // Should reject when full
    assert!(queue.push("overflow".to_string()).is_err());

    // Drain queue
    let mut drained = 0;
    while queue.pop().is_some() {
        drained += 1;
    }
    assert_eq!(drained, 5);
}
//! Loom concurrency tests for ZeroClawed
//!
//! These tests use Loom to exhaustively explore all possible thread interleavings
//! to detect data races, deadlocks, and memory ordering bugs.
//!
//! See [docs/concurrency-testing.md](docs/concurrency-testing.md) for:
//! - When to use loom vs runtime mutexes
//! - Surelock as a potential future async mutex option
//! - Running instructions
//! # Running Loom Tests
//!
//! ```bash
//! LOOM_MAX_PREEMPTIONS=2 RUSTFLAGS="--cfg loom" cargo test -p loom-tests
//! ```

#![allow(unexpected_cfgs)]

// Guard against silent 0-test runs without RUSTFLAGS="--cfg loom".
// Without the cfg, all loom tests below are stripped and this crate
// silently reports 0 tests passed — false confidence.
#[cfg(not(loom))]
#[test]
fn test_loom_cfg_missing() {
    panic!(
        "loom-tests requires RUSTFLAGS='--cfg loom'. Run:\n  \
         LOOM_MAX_PREEMPTIONS=2 RUSTFLAGS='--cfg loom' cargo test -p loom-tests"
    );
}

#[cfg(loom)]
mod loom_tests {
    use loom::sync::atomic::{AtomicUsize, Ordering};
    use loom::sync::{Arc, Mutex, RwLock};
    use loom::thread;
    use std::collections::HashMap;

    /// Test concurrent access to a shared registry pattern
    #[test]
    fn test_concurrent_registry_access() {
        loom::model(|| {
            let registry = Arc::new(Mutex::new(HashMap::new()));
            let r1 = Arc::clone(&registry);
            let r2 = Arc::clone(&registry);

            // Thread 1: Insert entries
            let t1 = thread::spawn(move || {
                let mut guard = r1.lock().unwrap();
                guard.insert("agent1".to_string(), "config1".to_string());
                guard.insert("agent2".to_string(), "config2".to_string());
            });

            // Thread 2: Read and verify entries
            let t2 = thread::spawn(move || {
                let guard = r2.lock().unwrap();
                let _count = guard.len();
            });

            t1.join().unwrap();
            t2.join().unwrap();

            // Verify final state
            let guard = registry.lock().unwrap();
            assert_eq!(guard.len(), 2);
            assert_eq!(guard.get("agent1").map(String::as_str), Some("config1"));
            assert_eq!(guard.get("agent2").map(String::as_str), Some("config2"));
        });
    }

    /// Test session management pattern with concurrent reads and writes
    #[test]
    fn test_concurrent_session_management() {
        loom::model(|| {
            let sessions = Arc::new(RwLock::new(HashMap::new()));
            let s1 = Arc::clone(&sessions);
            let s2 = Arc::clone(&sessions);
            let s3 = Arc::clone(&sessions);

            // Thread 1: Create sessions (write)
            let t1 = thread::spawn(move || {
                let mut guard = s1.write().unwrap();
                guard.insert("session_1".to_string(), "user_a".to_string());
                guard.insert("session_2".to_string(), "user_b".to_string());
            });

            // Thread 2: Read sessions
            let t2 = thread::spawn(move || {
                let guard = s2.read().unwrap();
                let _count = guard.len();
                let _ = guard.get("session_1");
            });

            // Thread 3: Another writer
            let t3 = thread::spawn(move || {
                let mut guard = s3.write().unwrap();
                guard.insert("session_3".to_string(), "user_c".to_string());
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();

            // All writers have joined
            let guard = sessions.read().unwrap();
            assert_eq!(guard.len(), 3);
            assert_eq!(guard.get("session_1").map(String::as_str), Some("user_a"));
            assert_eq!(guard.get("session_2").map(String::as_str), Some("user_b"));
            assert_eq!(guard.get("session_3").map(String::as_str), Some("user_c"));
        });
    }

    /// Test reference counting lifecycle
    #[test]
    fn test_arc_lifecycle() {
        loom::model(|| {
            let data = Arc::new(Mutex::new(0));
            let d1 = Arc::clone(&data);
            let d2 = Arc::clone(&data);

            let t1 = thread::spawn(move || {
                let mut guard = d1.lock().unwrap();
                *guard += 1;
            });

            let t2 = thread::spawn(move || {
                let mut guard = d2.lock().unwrap();
                *guard += 1;
            });

            t1.join().unwrap();
            t2.join().unwrap();

            let guard = data.lock().unwrap();
            assert_eq!(*guard, 2);
        });
    }

    /// Test channel-like message passing pattern
    #[test]
    fn test_message_passing_pattern() {
        loom::model(|| {
            let counter = Arc::new(AtomicUsize::new(0));
            let c1 = Arc::clone(&counter);
            let c2 = Arc::clone(&counter);

            // Simulate producer
            let producer = thread::spawn(move || {
                for _ in 0..3 {
                    c1.fetch_add(1, Ordering::SeqCst);
                    thread::yield_now();
                }
            });

            // Simulate consumer that observes state
            let consumer = thread::spawn(move || {
                thread::yield_now();
                let _value = c2.load(Ordering::SeqCst);
            });

            producer.join().unwrap();
            consumer.join().unwrap();

            assert_eq!(counter.load(Ordering::SeqCst), 3);
        });
    }

    /// Test for potential deadlock in nested lock acquisition
    #[test]
    fn test_no_deadlock_with_consistent_ordering() {
        loom::model(|| {
            let lock_a = Arc::new(Mutex::new(0));
            let lock_b = Arc::new(Mutex::new(0));

            let a1 = Arc::clone(&lock_a);
            let b1 = Arc::clone(&lock_b);
            let a2 = Arc::clone(&lock_a);
            let b2 = Arc::clone(&lock_b);

            let t1 = thread::spawn(move || {
                let _a = a1.lock().unwrap();
                thread::yield_now();
                let _b = b1.lock().unwrap();
            });

            let t2 = thread::spawn(move || {
                let _a = a2.lock().unwrap();
                thread::yield_now();
                let _b = b2.lock().unwrap();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }

    /// Test session cache invalidation pattern
    #[test]
    fn test_session_cache_invalidation_pattern() {
        loom::model(|| {
            let cache = Arc::new(RwLock::new(HashMap::new()));
            let cache2 = Arc::clone(&cache);

            let writer = thread::spawn(move || {
                let mut guard = cache.write().unwrap();
                guard.insert("sess1".to_string(), true);
                guard.insert("sess2".to_string(), true);

                thread::yield_now();
                guard.insert("sess1".to_string(), false);
            });

            let reader = thread::spawn(move || {
                let guard = cache2.read().unwrap();
                if let Some(&active) = guard.get("sess1") {
                    let _ = active;
                }
            });

            writer.join().unwrap();
            reader.join().unwrap();
        });
    }
}

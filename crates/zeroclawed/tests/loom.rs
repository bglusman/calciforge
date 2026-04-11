//! Loom concurrency tests for ZeroClawed
//!
//! These tests use Loom to exhaustively explore all possible thread interleavings
//! to detect data races, deadlocks, and memory ordering bugs.
//!
//! # Running Loom Tests
//!
//! Standard run (faster, explores fewer interleavings):
//! ```bash
//! RUSTFLAGS="--cfg loom" cargo test --test loom
//! ```
//!
//! Exhaustive run (slower, more thorough):
//! ```bash
//! LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" cargo test --test loom
//! ```
//!
//! With checkpointing for debugging:
//! ```bash
//! LOOM_CHECKPOINT_FILE=loom.json RUSTFLAGS="--cfg loom" cargo test --test loom
//! ```

#![allow(unexpected_cfgs)]
#![cfg(loom)]

use loom::sync::{Arc, Mutex, RwLock};
use loom::thread;
use std::collections::HashMap;

/// Test concurrent access to a shared registry pattern
/// Similar to AdapterRegistry but using std::sync for loom compatibility
#[test]
fn test_concurrent_registry_access() {
    loom::model(|| {
        let registry: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let registry2 = Arc::clone(&registry);

        // Thread 1: Insert entries
        let t1 = thread::spawn(move || {
            let mut guard = registry.lock().unwrap();
            guard.insert("agent1".to_string(), "config1".to_string());
            guard.insert("agent2".to_string(), "config2".to_string());
        });

        // Thread 2: Read and verify entries
        let t2 = thread::spawn(move || {
            // Try to read - may see empty or partial state depending on interleaving
            let guard = registry2.lock().unwrap();
            // Just verify we can access without deadlock
            let _count = guard.len();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Verify final state
        let guard = registry.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard.get("agent1"), Some(&"config1".to_string()));
        assert_eq!(guard.get("agent2"), Some(&"config2".to_string()));
    });
}

/// Test session management pattern with concurrent reads and writes
/// Simulates the ACP session management pattern
#[test]
fn test_concurrent_session_management() {
    loom::model(|| {
        let sessions: Arc<RwLock<HashMap<String, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let sessions2 = Arc::clone(&sessions);
        let sessions3 = Arc::clone(&sessions);

        // Thread 1: Create sessions (write)
        let t1 = thread::spawn(move || {
            let mut guard = sessions.write().unwrap();
            guard.insert("session_1".to_string(), "user_a".to_string());
            guard.insert("session_2".to_string(), "user_b".to_string());
        });

        // Thread 2: Read sessions
        let t2 = thread::spawn(move || {
            let guard = sessions2.read().unwrap();
            let _count = guard.len();
            // Try to find a session
            let _ = guard.get("session_1");
        });

        // Thread 3: Another writer
        let t3 = thread::spawn(move || {
            let mut guard = sessions3.write().unwrap();
            guard.insert("session_3".to_string(), "user_c".to_string());
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        // Verify final state
        let guard = sessions.read().unwrap();
        assert!(guard.len() >= 2); // At least the two from t1
    });
}

/// Test reference counting lifecycle
/// Ensures Arc properly manages shared ownership
#[test]
fn test_arc_lifecycle() {
    loom::model(|| {
        let data = Arc::new(Mutex::new(0));
        let data2 = Arc::clone(&data);
        let data3 = Arc::clone(&data);

        let t1 = thread::spawn(move || {
            let mut guard = data2.lock().unwrap();
            *guard += 1;
        });

        let t2 = thread::spawn(move || {
            let mut guard = data3.lock().unwrap();
            *guard += 1;
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let guard = data.lock().unwrap();
        assert_eq!(*guard, 2);
        assert_eq!(Arc::strong_count(&data), 1);
    });
}

/// Test channel-like message passing pattern
/// Simulates the mpsc pattern used in send_streaming
#[test]
fn test_message_passing_pattern() {
    use loom::sync::atomic::{AtomicUsize, Ordering};

    loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::clone(&counter);

        // Simulate producer
        let producer = thread::spawn(move || {
            for i in 0..3 {
                counter.fetch_add(1, Ordering::SeqCst);
                loom::yield_now();
            }
        });

        // Simulate consumer that observes state
        let consumer = thread::spawn(move || {
            loom::yield_now();
            let _value = counter2.load(Ordering::SeqCst);
            // Value could be 0, 1, 2, or 3 depending on interleaving
        });

        producer.join().unwrap();
        consumer.join().unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 3);
    });
}

/// Test for potential deadlock in nested lock acquisition
/// This pattern should be avoided in production code
#[test]
fn test_no_deadlock_with_consistent_ordering() {
    loom::model(|| {
        let lock_a = Arc::new(Mutex::new(0));
        let lock_b = Arc::new(Mutex::new(0));

        let lock_a2 = Arc::clone(&lock_a);
        let lock_b2 = Arc::clone(&lock_b);

        // Both threads acquire locks in same order to prevent deadlock
        let t1 = thread::spawn(move || {
            let _a = lock_a.lock().unwrap();
            loom::yield_now();
            let _b = lock_b.lock().unwrap();
        });

        let t2 = thread::spawn(move || {
            let _a = lock_a2.lock().unwrap();
            loom::yield_now();
            let _b = lock_b2.lock().unwrap();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

/// Test that demonstrates the pattern for testing ZeroClawed's session cache
/// This is a template for future integration tests
#[test]
fn test_session_cache_invalidation_pattern() {
    loom::model(|| {
        let cache: Arc<RwLock<HashMap<String, bool>>> = Arc::new(RwLock::new(HashMap::new()));
        let cache2 = Arc::clone(&cache);

        // Thread that marks sessions as active
        let writer = thread::spawn(move || {
            let mut guard = cache.write().unwrap();
            guard.insert("sess1".to_string(), true);
            guard.insert("sess2".to_string(), true);

            // Later, invalidate one
            loom::yield_now();
            guard.insert("sess1".to_string(), false);
        });

        // Thread that checks session validity
        let reader = thread::spawn(move || {
            let guard = cache2.read().unwrap();
            // May see various states depending on interleaving
            if let Some(&active) = guard.get("sess1") {
                // Session state observed
                let _ = active;
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    });
}

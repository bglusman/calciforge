//! Loom concurrency tests for ZeroClawed
//!
//! These tests use Loom to exhaustively explore all possible thread interleavings
//! to detect data races, deadlocks, and memory ordering bugs.
//!
//! # Running Loom Tests
//!
//! Standard run (faster, explores fewer interleavings):
//! ```bash
//! RUSTFLAGS="--cfg loom" cargo run -p loom-tests
//! ```
//!
//! Exhaustive run (slower, more thorough):
//! ```bash
//! LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" cargo run -p loom-tests
//! ```

use loom::sync::{Arc, Mutex, RwLock};
use loom::thread;
use std::collections::HashMap;

fn main() {
    println!("Running Loom concurrency tests...");

    test_concurrent_registry_access();
    println!("✓ test_concurrent_registry_access passed");

    test_concurrent_session_management();
    println!("✓ test_concurrent_session_management passed");

    test_arc_lifecycle();
    println!("✓ test_arc_lifecycle passed");

    test_message_passing_pattern();
    println!("✓ test_message_passing_pattern passed");

    test_no_deadlock_with_consistent_ordering();
    println!("✓ test_no_deadlock_with_consistent_ordering passed");

    test_session_cache_invalidation_pattern();
    println!("✓ test_session_cache_invalidation_pattern passed");

    println!("\nAll Loom tests passed!");
}

fn test_concurrent_registry_access() {
    loom::model(|| {
        let registry: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let registry2 = Arc::clone(&registry);
        let registry3 = Arc::clone(&registry);

        let t1 = thread::spawn(move || {
            let mut guard = registry2.lock().unwrap();
            guard.insert("agent1".to_string(), "config1".to_string());
            guard.insert("agent2".to_string(), "config2".to_string());
        });

        let t2 = thread::spawn(move || {
            let guard = registry3.lock().unwrap();
            let _count = guard.len();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let guard = registry.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard.get("agent1"), Some(&"config1".to_string()));
        assert_eq!(guard.get("agent2"), Some(&"config2".to_string()));
    });
}

fn test_concurrent_session_management() {
    loom::model(|| {
        let sessions: Arc<RwLock<HashMap<String, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let sessions2 = Arc::clone(&sessions);
        let sessions3 = Arc::clone(&sessions);
        let sessions4 = Arc::clone(&sessions);

        let t1 = thread::spawn(move || {
            let mut guard = sessions2.write().unwrap();
            guard.insert("session_1".to_string(), "user_a".to_string());
            guard.insert("session_2".to_string(), "user_b".to_string());
        });

        let t2 = thread::spawn(move || {
            let guard = sessions3.read().unwrap();
            let _count = guard.len();
            let _ = guard.get("session_1");
        });

        let t3 = thread::spawn(move || {
            let mut guard = sessions4.write().unwrap();
            guard.insert("session_3".to_string(), "user_c".to_string());
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let guard = sessions.read().unwrap();
        assert!(guard.len() >= 2);
    });
}

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

fn test_message_passing_pattern() {
    use loom::sync::atomic::{AtomicUsize, Ordering};

    loom::model(|| {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::clone(&counter);
        let counter3 = Arc::clone(&counter);

        let producer = thread::spawn(move || {
            for _ in 0..3 {
                counter2.fetch_add(1, Ordering::SeqCst);
                thread::yield_now();
            }
        });

        let consumer = thread::spawn(move || {
            thread::yield_now();
            let _value = counter3.load(Ordering::SeqCst);
        });

        producer.join().unwrap();
        consumer.join().unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 3);
    });
}

fn test_no_deadlock_with_consistent_ordering() {
    loom::model(|| {
        let lock_a = Arc::new(Mutex::new(0));
        let lock_b = Arc::new(Mutex::new(0));

        let lock_a2 = Arc::clone(&lock_a);
        let lock_b2 = Arc::clone(&lock_b);

        let t1 = thread::spawn(move || {
            let _a = lock_a.lock().unwrap();
            thread::yield_now();
            let _b = lock_b.lock().unwrap();
        });

        let t2 = thread::spawn(move || {
            let _a = lock_a2.lock().unwrap();
            thread::yield_now();
            let _b = lock_b2.lock().unwrap();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

fn test_session_cache_invalidation_pattern() {
    loom::model(|| {
        let cache: Arc<RwLock<HashMap<String, bool>>> = Arc::new(RwLock::new(HashMap::new()));
        let cache2 = Arc::clone(&cache);
        let cache3 = Arc::clone(&cache);

        let writer = thread::spawn(move || {
            let mut guard = cache2.write().unwrap();
            guard.insert("sess1".to_string(), true);
            guard.insert("sess2".to_string(), true);
            thread::yield_now();
            guard.insert("sess1".to_string(), false);
        });

        let reader = thread::spawn(move || {
            let guard = cache3.read().unwrap();
            if let Some(&active) = guard.get("sess1") {
                let _ = active;
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    });
}

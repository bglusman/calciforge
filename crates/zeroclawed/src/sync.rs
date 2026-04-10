//! Concurrency primitives with Loom support for testing
//!
//! This module provides wrappers around standard synchronization primitives
//! that automatically use Loom's versions when running under `cfg(loom)`.
#![allow(unused_imports)]
//!
//! # Usage
//!
//! Instead of using `std::sync::Arc` or `tokio::sync::RwLock` directly,
//! use the types from this module:
//!
//! ```rust
//! use zeroclawed::sync::{Arc, Mutex};
//!
//! let data = Arc::new(Mutex::new(Vec::new()));
//! ```
//!
//! When running with `RUSTFLAGS="--cfg loom"`, these types use Loom's
//! implementations for exhaustive concurrency testing.

#[cfg(loom)]
pub use loom::sync::{Arc, Mutex, RwLock, Weak};

#[cfg(not(loom))]
pub use std::sync::{Arc, Mutex, RwLock, Weak};

/// Condvar with loom support
#[cfg(loom)]
pub use loom::sync::Condvar;

#[cfg(not(loom))]
pub use std::sync::Condvar;

/// Barrier with loom support
#[cfg(loom)]
pub use loom::sync::Barrier;

#[cfg(not(loom))]
pub use std::sync::Barrier;

/// Atomic types with loom support
#[cfg(loom)]
pub mod atomic {
    pub use loom::sync::atomic::{AtomicBool, AtomicI16, AtomicI32, AtomicI64, AtomicI8};
    pub use loom::sync::atomic::{AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8};
    pub use loom::sync::atomic::{AtomicUsize, Ordering};
}

#[cfg(not(loom))]
pub mod atomic {
    pub use std::sync::atomic::{AtomicBool, AtomicI16, AtomicI32, AtomicI64, AtomicI8};
    pub use std::sync::atomic::{AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8};
    pub use std::sync::atomic::{AtomicUsize, Ordering};
}

/// Thread spawning with loom support
#[cfg(loom)]
pub use loom::thread;

#[cfg(not(loom))]
pub use std::thread;

/// Channel types for loom
#[cfg(loom)]
pub mod channel {
    pub use loom::sync::mpsc::{channel, Receiver, Sender};
}

#[cfg(not(loom))]
pub mod channel {
    pub use std::sync::mpsc::{channel, Receiver, Sender};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_arc_mutex_basic() {
        let data = Arc::new(Mutex::new(42));
        let data2 = Arc::clone(&data);

        let handle = thread::spawn(move || {
            let mut guard = data2.lock().unwrap();
            *guard += 1;
        });

        handle.join().unwrap();

        let guard = data.lock().unwrap();
        assert_eq!(*guard, 43);
    }

    #[test]
    fn test_rwlock_basic() {
        let data = Arc::new(RwLock::new(HashMap::new()));
        let data2 = Arc::clone(&data);

        // Writer thread
        let writer = thread::spawn(move || {
            let mut guard = data.write().unwrap();
            guard.insert("key", "value");
        });

        // Reader thread
        let reader = thread::spawn(move || {
            let guard = data2.read().unwrap();
            // May or may not see the write depending on timing
            let _ = guard.get("key");
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}

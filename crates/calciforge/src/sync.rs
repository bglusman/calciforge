//! Loom-aware synchronization primitives for Calciforge.
//!
//! This module provides synchronization primitives that work with both
//! regular Rust and Loom's model checker. When compiled with `--cfg loom`,
//! these types delegate to `loom::sync`; otherwise they use the standard
//! library versions.
//!
//! # Usage
//!
//! ```rust
//! use crate::sync::{Mutex, AtomicU64};
//!
//! let counter = AtomicU64::new(0);
//! let data = Mutex::new(vec![1, 2, 3]);
//! ```

#![allow(unused_imports)]
#![allow(dead_code)]

// Re-export synchronization primitives with conditional compilation
#[cfg(loom)]
pub use loom::sync::atomic;
#[cfg(not(loom))]
pub use std::sync::atomic;

#[cfg(loom)]
pub use loom::sync::{Arc, Mutex, RwLock};
#[cfg(not(loom))]
pub use std::sync::{Arc, Mutex, RwLock};

// OnceLock - std only (loom doesn't support it yet)
// For loom tests, use lazy initialization patterns or const constructors
pub use std::sync::OnceLock;

// Atomic types with conditional compilation
#[cfg(not(loom))]
pub type AtomicU64 = std::sync::atomic::AtomicU64;
#[cfg(loom)]
pub type AtomicU64 = loom::sync::atomic::AtomicU64;

#[cfg(not(loom))]
pub type AtomicUsize = std::sync::atomic::AtomicUsize;
#[cfg(loom)]
pub type AtomicUsize = loom::sync::atomic::AtomicUsize;

#[cfg(not(loom))]
pub type AtomicBool = std::sync::atomic::AtomicBool;
#[cfg(loom)]
pub type AtomicBool = loom::sync::atomic::AtomicBool;

// Ordering constants
#[cfg(loom)]
pub use loom::sync::atomic::Ordering;
#[cfg(not(loom))]
pub use std::sync::atomic::Ordering;

/// A thread-safe, loom-aware wrapper for shared mutable state.
///
/// This is a convenience type that combines `Arc<Mutex<T>>` with
/// conditional compilation for Loom testing.
pub struct Shared<T> {
    inner: Arc<Mutex<T>>,
}

impl<T> Shared<T> {
    /// Creates a new `Shared` wrapper around `value`.
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(value)),
        }
    }

    /// Returns a reference to the inner `Arc<Mutex<T>>`.
    pub fn inner(&self) -> &Arc<Mutex<T>> {
        &self.inner
    }

    /// Creates a new reference to the shared data.
    pub fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self::clone(self)
    }
}

/// A thread-safe, loom-aware read-write lock wrapper.
///
/// This is a convenience type that combines `Arc<RwLock<T>>` with
/// conditional compilation for Loom testing.
pub struct SharedRw<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> SharedRw<T> {
    /// Creates a new `SharedRw` wrapper around `value`.
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(value)),
        }
    }

    /// Returns a reference to the inner `Arc<RwLock<T>>`.
    pub fn inner(&self) -> &Arc<RwLock<T>> {
        &self.inner
    }

    /// Creates a new reference to the shared data.
    pub fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> Clone for SharedRw<T> {
    fn clone(&self) -> Self {
        Self::clone(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_mutex() {
        let shared = Shared::new(42);
        let shared2 = shared.clone();

        // In a real test with loom, we'd spawn threads here
        // For now, just verify it compiles and works
        let lock_result = shared.inner().lock();
        assert!(lock_result.is_ok());
        if let Ok(mut guard) = lock_result {
            *guard = 100;
        }

        let lock_result2 = shared2.inner().lock();
        assert!(lock_result2.is_ok());
        if let Ok(guard) = lock_result2 {
            assert_eq!(*guard, 100);
        }
    }

    #[test]
    fn test_shared_rwlock() {
        let shared = SharedRw::new(vec![1, 2, 3]);
        let shared2 = shared.clone();

        // Test write
        {
            let mut guard = shared.inner().write().unwrap();
            guard.push(4);
        }

        // Test read
        {
            let guard = shared2.inner().read().unwrap();
            assert_eq!(*guard, vec![1, 2, 3, 4]);
        }
    }

    #[test]
    fn test_atomic_types() {
        let atomic_u64 = AtomicU64::new(0);
        atomic_u64.store(42, Ordering::SeqCst);
        assert_eq!(atomic_u64.load(Ordering::SeqCst), 42);

        let atomic_usize = AtomicUsize::new(0);
        atomic_usize.store(100, Ordering::SeqCst);
        assert_eq!(atomic_usize.load(Ordering::SeqCst), 100);

        let atomic_bool = AtomicBool::new(false);
        atomic_bool.store(true, Ordering::SeqCst);
        assert!(atomic_bool.load(Ordering::SeqCst));
    }
}

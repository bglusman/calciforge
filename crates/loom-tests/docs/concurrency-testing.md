# Concurrency Testing in ZeroClawed

## Approach

ZeroClawed uses **loom** for exhaustive concurrency model checking in this
isolated crate. Loom explores all possible thread interleavings to prove
absence of data races and deadlocks.

## When to Use Loom

- Testing `Arc<RwLock<...>>` or `Arc<Mutex<...>>` shared-state patterns
- Verifying message-passing ordering guarantees
- Proving absence of deadlocks with consistent lock ordering
- Testing cache invalidation / observer patterns under concurrency

## Surelock (seanmonstar/surelock) — Future Consideration

**Surelock** is a simple async-aware mutex that provides:

```rust
let lock = Surelock::new();
let guard = lock.lock().await;
// critical section
```

Key properties:
- **No deadlock detection** — simple blocking, no sophisticated algorithms
- **No poisoning** — no `PoisonError` like `std::sync::Mutex`
- **Fair scheduling** — FIFO lock acquisition order
- **Minimal API surface** — just `new()`, `lock().await`, `try_lock()`

### When to Consider Surelock

- Replace `Arc<RwLock<...>>` if contention benchmarks show unfair scheduling
- Shared state that needs fair access ordering (context store, session registry)
- When `std::sync::Mutex` poisoning adds complexity without value
- NOT a replacement for loom — loom is for testing, surelock is for runtime

### Decision Criteria

| Signal | Action |
|--------|--------|
| RwLock write starvation under load | Switch to surelock |
| Lock contention benchmarks show unfairness | Switch to surelock |
| Context store grows to high contention | Evaluate surelock |
| Current `Arc<RwLock<...>>` is fine | No action needed |

### Status

- Not yet needed — current `Arc<RwLock<...>>` patterns pass all loom tests
- No contention benchmarks have been run
- Revisit if we add concurrent session management or high-throughput dispatch

## Running Loom Tests

```bash
# Requires RUSTFLAGS="--cfg loom" (tokio::net is disabled under --cfg loom)
RUSTFLAGS='--cfg loom' LOOM_MAX_PREEMPTIONS=2 cargo test -p loom-tests

# CI runs with LOOM_MAX_PREEMPTIONS=2 for speed (default is 1000)
# Tests also check LOOM_MAX_PREEMPTIONS is >= 2 to enforce the limit
```

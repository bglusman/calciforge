# Concurrency Testing for ZeroClawed

This document describes the concurrency testing infrastructure for zeroclawed, including Loom for model checking and QEMU for cross-architecture testing.

## Overview

ZeroClawed uses a multi-layered approach to concurrency testing:

1. **Loom** - Exhaustive model checking for concurrent Rust code (in `crates/loom-tests/`)
2. **QEMU** - Cross-architecture testing (x86_64 → ARM64)
3. **Standard tests** - Regular cargo test for unit and integration tests

## Loom Testing

[Loom](https://github.com/tokio-rs/loom) is a model checker for concurrent Rust code. It exhaustively explores all possible thread interleavings to detect data races, deadlocks, and memory ordering issues.

Loom tests are in a separate crate (`crates/loom-tests/`) because tokio (used by zeroclawed) conflicts with loom's `cfg(loom)` flag.

### Running Loom Tests

```bash
cd crates/loom-tests

# Run with default settings
cargo run

# Run with loom model checking enabled
RUSTFLAGS="--cfg loom" cargo run

# Run with reduced preemptions (faster, good for CI)
RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=2 cargo run

# Run with maximum exploration (slower, more thorough)
RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=5 cargo run

# Run with checkpointing for debugging
RUSTFLAGS="--cfg loom" LOOM_CHECKPOINT_FILE=loom.json cargo run
```

### Loom Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `LOOM_MAX_PREEMPTIONS` | Max preemptions per thread | 3 |
| `LOOM_MAX_BRANCHES` | Max branches to explore | 10,000 |
| `LOOM_CHECKPOINT_INTERVAL` | Save checkpoint every N branches | Disabled |
| `LOOM_CHECKPOINT_FILE` | File to save/load checkpoint | Disabled |

### Writing Loom Tests

Loom tests are located in `crates/loom-tests/src/main.rs`. When writing new tests:

1. Use loom's sync primitives (`loom::sync::Arc`, `loom::sync::Mutex`, etc.)
2. Wrap test bodies in `loom::model()`
3. Keep test scope focused - Loom's state space grows exponentially

Example:
```rust
fn test_my_concurrent_pattern() {
    loom::model(|| {
        let data = Arc::new(Mutex::new(0));
        let data2 = Arc::clone(&data);
        
        let t1 = thread::spawn(move || {
            let mut guard = data2.lock().unwrap();
            *guard += 1;
        });
        
        let t2 = thread::spawn(move || {
            let mut guard = data.lock().unwrap();
            *guard += 1;
        });
        
        t1.join().unwrap();
        t2.join().unwrap();
        
        assert_eq!(*data.lock().unwrap(), 2);
    });
}
```

### Using sync.rs in Production Code

The `src/sync.rs` module in zeroclawed provides loom-aware sync primitives:

```rust
use zeroclawed::sync::{Arc, Mutex};

// When running normally, uses std::sync
// When running with RUSTFLAGS="--cfg loom", uses loom::sync
```

To use in production code:
1. Import from `crate::sync` instead of `std::sync`
2. The same code works with and without loom testing

## QEMU Cross-Architecture Testing

We use [cross](https://github.com/cross-rs/cross) and QEMU to run tests on ARM64 from x86_64 CI runners.

### Cross.toml Configuration

The `Cross.toml` file in `crates/zeroclawed/` configures cross-compilation:

```toml
[build]
pre-build = [
    "apt-get update && apt-get install -y pkg-config libssl-dev"
]

[target.aarch64-unknown-linux-gnu]
image = "ghcr.io/cross-rs/aarch64-unknown-linux-gnu:main"
passthrough = ["RUSTFLAGS", "LOOM_MAX_PREEMPTIONS", "LOOM_MAX_BRANCHES"]
runner = "qemu-aarch64"
```

### Running Cross-Architecture Tests Locally

```bash
cd crates/zeroclawed

# Install cross tool
cargo install cross --git https://github.com/cross-rs/cross

# Verify ARM64 compilation
cross build --target aarch64-unknown-linux-gnu

# Run all tests on ARM64 via QEMU
cross test --target aarch64-unknown-linux-gnu

# Run specific test
cross test --target aarch64-unknown-linux-gnu test_name
```

### How It Works

1. **cross** uses Docker containers with pre-installed cross-compilation toolchains
2. For test targets, cross automatically uses QEMU user-mode emulation
3. The `Cross.toml` file configures the build environment and passes through environment variables
4. Tests run in an isolated container with the target architecture

### Known Limitations

1. **QEMU Performance** - Tests run slower under emulation (5-10x typical)
2. **Loom State Space** - Complex tests may timeout; use `LOOM_MAX_PREEMPTIONS` to limit
3. **Platform Differences** - Some tests may behave differently on ARM64 vs x86_64 due to memory ordering differences

## Debugging Failures

### Loom Failures

Loom will report the exact interleaving that caused a failure:

```
thread panicked at 'assertion failed: ...'

--- STDERR:
 loom iteration: 42
 switches: 5
 ...
```

To reproduce:
```bash
cd crates/loom-tests
RUSTFLAGS="--cfg loom" LOOM_CHECKPOINT_FILE=loom_checkpoint.json cargo run
# Then re-run with the checkpoint to reproduce the failure
```

### QEMU Failures

Check the CI logs for architecture-specific issues. Common problems:

1. **Missing syscalls** - Some syscalls may not be fully emulated
2. **Endianness bugs** - ARM64 is little-endian (same as x86_64), but beware on big-endian targets
3. **Alignment issues** - ARM64 is stricter about memory alignment than x86_64

## Future Work

- [ ] Add Miri testing for undefined behavior detection
- [ ] Expand Loom tests to more concurrent code paths
- [ ] Add RISC-V cross-compilation target
- [ ] Integrate Loom tests into CI workflow
- [ ] Investigate using [proptest](https://github.com/proptest-rs/proptest) for property-based testing
- [ ] Migrate production code to use `crate::sync` for loom-testability

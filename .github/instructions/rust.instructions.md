---
applyTo: "**/*.rs"
description: Rust review specifics for Calciforge — applied automatically to every .rs file in a PR
---

# Rust review specifics

These extend `.github/copilot-instructions.md`. Same review philosophy: **if uncertain, do not comment**. Verify against the diff and surrounding code before flagging.

## Errors

- `.unwrap()` / `.expect()` in non-test code is a finding only when the invariant isn't obvious from local context. `unwrap()` on a freshly-constructed `Mutex` lock or on a `Regex::new` of a literal is fine. `unwrap()` on a `parse()` of arbitrary input is not.
- Library code uses `thiserror`. Binary / glue code uses `anyhow` with `.context("…")`. Don't suggest swapping the convention; that's intentional.
- Don't suggest replacing `?` with `match` "for clarity" — `?` is the convention.

## Unsafe

- Every `unsafe { … }` block needs a `// SAFETY:` comment immediately above explaining *why* the invariants hold. Flag missing SAFETY comments. Don't flag the existence of `unsafe` itself unless the block is plausibly avoidable.
- Edition 2024 makes `std::env::set_var` / `remove_var` `unsafe` (process-wide, not thread-safe). In test code, prefer `serial_test` + scoped restoration over leaving the env mutated. **Do not** repeat the env-mutex / `serial_test` comment if it's already raised on a sibling test in the same PR — link to it once.

## Async / concurrency

- `std::sync::Mutex` (or `RwLock`) held across `.await` is a deadlock risk under the multi-threaded runtime. Use `tokio::sync::Mutex` or restructure to drop the guard before awaiting. Worth flagging.
- Blocking I/O (`std::fs`, `std::process::Command`, `reqwest::blocking`) inside an async fn is worth flagging — wrap in `tokio::task::spawn_blocking` or use the async equivalent. Exception: startup-only one-shot reads are fine.
- `tokio::select!` branches must be cancellation-safe. If a branch holds partial state across `.await` (e.g., a half-written buffer), losing the race silently corrupts state. Worth flagging if non-obvious.
- `tokio::process::Command` without `.kill_on_drop(true)` leaks the child if the parent task is dropped mid-await. Flag for long-running children; skip for one-shot commands that are awaited to completion.
- Spawned `JoinHandle`s that are dropped silently swallow panics. Worth flagging for long-running tasks; not for fire-and-forget helpers.

## Lints / attributes

- Prefer `#[expect(lint, reason = "…")]` over `#[allow(lint)]` (Rust 1.81+). `expect` warns if the lint stops triggering, so dead suppressions get cleaned up. Worth suggesting on new `#[allow]` additions.
- `#[must_use]` on functions that return a `Result`-like wrapper or a builder is worth suggesting once per type.

## Public API hygiene

- Public enums/structs that may grow: `#[non_exhaustive]` to keep additions non-breaking.
- Function args: prefer `&str` over `&String`, `&[T]` over `&Vec<T>`, `impl AsRef<Path>` over `&PathBuf`. Worth flagging on new public APIs; skip on internal call-sites where it doesn't matter.
- Don't flag missing rustdoc on `pub(crate)` items.

## Allocations / regex

- `Regex::new(...)` inside a hot loop or per-request handler should be hoisted to a `static` via `LazyLock<Regex>`. Worth flagging if it's actually on a hot path; skip for one-shot CLI use.
- `format!("{}", x).into()` when `x.to_string()` works, or `vec.clone()` where `&vec` would do — only flag if the path is hot or the allocation is large.
- `Cow<'_, str>` over owned `String` is worth suggesting only when callers actually have borrowed inputs; speculative `Cow` is overkill.

## Cargo / deps

- New dependency in a `Cargo.toml` is worth a sanity check: is it in `workspace.dependencies` already? Does it duplicate something we have (e.g., adding `serde_yaml` when `serde_yml` is the workspace pick)? Don't flag version mismatches that match the workspace `*` resolver behavior.
- Feature flags: optional deps should be `optional = true` and gated behind a named feature, not pulled unconditionally.

## What NOT to flag in Rust files

- Module organization preferences (`mod.rs` vs `foo.rs` + `foo/`) — both valid, no consensus.
- `Box<dyn Error>` vs concrete error types in binaries — fine for binaries.
- Lifetime elision style — if it compiles and reads naturally, leave it.
- `clone()` in tests — readability wins over micro-optimization in test code.

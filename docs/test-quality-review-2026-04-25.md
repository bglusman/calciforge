# Test Quality Review - 2026-04-25

This review focuses on test reliability, signal quality, CI coverage, and the
places where the suite can pass while missing an important behavior.

## Summary

The project has a strong baseline:

- Roughly 660 Rust `#[test]` / `#[tokio::test]` sites by text scan.
- Workspace pre-push checks run format, clippy with `-D warnings`, workspace
  tests, and isolated Loom tests.
- CI splits crate tests, integration tests, clippy, release builds, Loom, and
  Gitleaks secret scanning.
- Several security-sensitive areas already have property tests, semantic shell
  quoting tests, mock HTTP tests, and fail-closed policy tests.

The main quality risk is not lack of tests. It is false confidence from tests
that are isolated imperfectly, skipped by default, duplicated in stale entry
points, or named "integration" while only testing pure logic.

## Findings

### 1. Digest store tests can race on temp filenames

**Evidence:** `crates/adversary-detector/src/digest.rs:164`

`tmp_path()` uses process ID plus `SystemTime::now().as_nanos()` to create test
paths. Tokio tests run concurrently inside one process, so two tests can produce
the same path if the timestamp collides. While publishing the adversarial review,
the full pre-push hook failed because `test_set_and_get_roundtrip` read the
digest written by `test_ttl_zero_means_no_expiration`.

**Impact:** flaky workspace test failures and, worse, occasional cross-test
state pollution that can hide real failures.

**Recommendation:** use `tempfile` or an atomic monotonic suffix for test paths.
PR #30 includes a minimal atomic-counter fix.

### 2. Ignored security-proxy integration tests can pass without a gateway

**Evidence:** `crates/security-proxy/tests/integration.rs:7`,
`crates/security-proxy/tests/integration.rs:39`

The two ignored gateway tests are manually runnable with `--ignored`, but if the
gateway is not running the request error is printed and the test completes
without failing.

**Impact:** a developer can explicitly run the ignored gateway tests and get a
green result even though the gateway behavior was never exercised.

**Recommendation:**

- Convert these to self-contained tests that spawn the service on
  `127.0.0.1:0`, or
- keep them manual but fail fast when the gateway is unavailable, using a clear
  message such as "start security-proxy before running --ignored tests".

### 3. Loom tests have two entry points, one of which is effectively stale

**Evidence:** `crates/loom-tests/src/lib.rs:18`,
`crates/zeroclawed/tests/loom.rs:24`, `scripts/pre-push.sh:79`

The maintained path is the isolated `loom-tests` crate, and the pre-push script
correctly runs it with `RUSTFLAGS="--cfg loom"`. There is also a
`zeroclawed` test target guarded by `#![cfg(loom)]`, plus `cfg(loom)` code in
`crates/zeroclawed/src`, which the pre-push script warns about as inert.

**Impact:** duplicate guidance and stale Loom targets make it easier for future
contributors to run the wrong command or add tests to the path that CI does not
exercise.

**Recommendation:**

- Move any still-useful scenarios from `crates/zeroclawed/tests/loom.rs` into
  `crates/loom-tests`.
- Remove the stale `zeroclawed` Loom test target after migration.
- Turn the current pre-push warning about `cfg(loom)` in `zeroclawed/src` into a
  failing check once migration is complete.

### 4. CI has overlapping test jobs with slightly different semantics

**Evidence:** `.github/workflows/ci.yml`, `.github/workflows/integration-tests.yml`,
`scripts/pre-push.sh`

The main CI workflow runs `cargo test -p <crate>` in a matrix. The integration
workflow runs workspace lib/bin tests and workspace integration tests with
`--skip test_gateway_`. The pre-push hook runs `cargo test --workspace --exclude
loom-tests`.

The overlap is useful, but the differences are not documented as intentional.
For example, `integration-tests.yml` skips `test_gateway_`, while the pre-push
hook runs ignored tests only if they are not marked ignored.

**Impact:** when a test is added, it is not obvious which gate should own it or
whether it should be skipped, ignored, mocked, or run under a service harness.

**Recommendation:**

- Add a short `docs/testing.md` that defines test tiers:
  - unit: no network/process dependencies
  - integration-self-contained: may bind `127.0.0.1:0` or spawn child processes
  - live/manual: requires an external service and must fail if prerequisites are
    missing
  - loom: only in `crates/loom-tests`
- Reference that document from CI and `scripts/pre-push.sh`.

### 5. OneCLI live proxy CI should use a random port

**Evidence:** `.github/workflows/integration-tests.yml:52`

The OneCLI proxy CI starts the service on its default `127.0.0.1:8081` and waits
for `/health`. This is probably fine on hosted runners, but it is less robust
than a random port and can conflict on self-hosted or reused runners.

**Impact:** avoidable port-collision flakes.

**Recommendation:** set `ONECLI_BIND=127.0.0.1:0` if the service can report the
chosen port, or assign a high random port in the workflow and pass the same port
through `ONECLI_URL`.

## Positive Signals

- The pre-push hook catches the same class of failures as CI: formatting,
  clippy warnings, workspace tests, and Loom.
- The dedicated `loom-tests` crate is the right design because global
  `RUSTFLAGS="--cfg loom"` can break normal async networking crates.
- `zeroclawed` e2e tests are mostly self-contained and use mock binaries or
  loopback listeners instead of requiring live external services.
- The installer and SSH layers have useful semantic tests around shell quoting,
  TOML/JSON patching, health-check rollback, and remote config writes.
- Property testing is already present in `zeroclawed` e2e tests and host-agent
  validation work.

## Fix Order

1. Land the digest temp-path race fix from PR #30.
2. Make ignored security-proxy gateway tests fail when prerequisites are absent,
   or convert them to self-contained spawned-service tests.
3. Consolidate Loom tests into `crates/loom-tests` and remove stale `zeroclawed`
   Loom entry points.
4. Add `docs/testing.md` to make test tiers and CI ownership explicit.
5. Randomize OneCLI live proxy test ports where practical.

## Suggested Test Policy For The Fnox Secret Input UI

If the input-only fnox UI moves forward, it should have tests before it is
treated as safe UX:

- Create-only is the default; update attempts fail unless an explicit
  `allow_update` setting is present.
- Form submission passes secret values over stdin to the fnox wrapper, never
  argv.
- Server responses never include plaintext secret values.
- Prefix/suffix confirmation is derived only from the currently submitted form
  value, not by reading the stored secret back out.
- Audit logs include secret names and operations, never secret values.

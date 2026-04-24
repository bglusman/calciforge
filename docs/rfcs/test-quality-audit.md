# Test Quality Audit

Triage of existing tests against a quality bar. Each finding is `KEEP | REWRITE | DELETE` with a one-line reason. `REWRITE` entries include a suggested assertion.

## Quality bar

A test is rejected if any of these apply:

1. Can't fail (asserts against test-local constant, uses expected in the act step).
2. Tests implementation details, not observable behavior.
3. Meaningless assertion (`.is_ok()` without checking value).
4. Tautological (mock returns X, test asserts X back).
5. Missing negative/edge case for behavior that has interesting failure modes.
6. Name doesn't describe what should happen.
7. Duplicates another test with no additional coverage.
8. Flaky by design.

## Round 1: onecli-client + security-proxy

### crates/onecli-client/src/client.rs

- REWRITE · `client.rs:84 test_client_creation_with_valid_config` · `.is_ok()` without value inspection; creation of a reqwest client with default timeout is essentially infallible here, so this only guards a panic.
  - should assert: the builder uses the configured timeout and agent_id (exercise via a spawned mock server and observe outgoing request headers/timeout behavior).
- DELETE · `client.rs:91 test_client_creation_with_custom_config` · Duplicates `test_client_creation_with_valid_config` with only trivially different values; `.is_ok()` alone, no additional coverage.
- REWRITE · `client.rs:101 test_client_get_routes_through_proxy` · The comment admits "we can't inspect RequestBuilder internals" — test asserts nothing (implicit non-panic only). Cannot meaningfully fail.
  - should assert: spawn a local mock server as the OneCLI proxy, issue a `.get("https://api.example.com/test").send()`, and verify the server received method=GET, `X-Target-URL: https://api.example.com/test`, `X-OneCLI-Agent-ID: <id>`.
- REWRITE · `client.rs:116 test_client_post_routes_through_proxy` · Same as above: no assertion, only non-panic.
  - should assert: same mock-server approach, verify method=POST reaches proxy and target URL is preserved in header.
- KEEP · `client.rs:124 test_client_debug_format` · Weakly useful (guards against accidentally printing credentials), though ideally it should also assert no secret-like fields leak. Acceptable as-is.
- REWRITE · `client.rs:138 test_request_builder_method_mapping` · Pure non-panic check across 4 methods — no assertions.
  - should assert: via mock server, each method arrives as the correct HTTP verb at the proxy.
- REWRITE · `client.rs:156 test_client_url_trailing_slash_stripped` · Comment says what *should* happen, body does not check it. No assertion.
  - should assert: after configuring `http://proxy:8081/` (trailing slash), the request sent to the mock server hits the root path exactly once (no `//`), and the `X-Target-URL` header is unmodified.

### crates/onecli-client/src/config.rs

- REWRITE · `config.rs:99 test_retry_config_defaults` · Tautological: asserts the same constants the `Default` impl hard-codes. Any change to defaults updates the test in lockstep; it cannot catch regressions in behavior, only accidental typos.
  - should assert: defaults produce expected retry *behavior* (e.g., max total wait bounded by `max_delay * max_retries`), tested through `execute_with_retry`. At minimum document the defaults as policy-bound in a comment so the test has a reason to exist.
- REWRITE · `config.rs:107 test_onecli_config_defaults` · Same tautology as above; plus the default `url` is `http://localhost:8081` — if that default changes (likely, see CLAUDE.md re: no hard-coded URLs), this test breaks without exercising any behavior.
  - should assert: the default config produces a working `OneCliClient::new(...)` and the default URL is *some* localhost URL (regex), so tests don't pin a specific port.
- KEEP · `config.rs:115 test_onecli_config_toml_roundtrip` · Real roundtrip with non-default values; would catch serde attribute drift (e.g. `humantime_serde` removal). Useful.
- KEEP · `config.rs:129 test_retry_config_toml_roundtrip` · Same rationale as above.
- Missing coverage: `OneCliServiceConfig::from_env_or_file` is entirely untested despite having non-trivial env-var fallback logic and TOML parsing. Add tests using `ONECLI_CONFIG=<tempfile>` and env-var paths (but beware: env tests need serial execution / `#[serial_test]`).

### crates/onecli-client/src/error.rs

- REWRITE · `error.rs:66 test_unreachable_is_retryable` · Name is wrong: body asserts `RateLimited`, not `Unreachable`. Comment acknowledges reqwest::Error is hard to construct. Test mislabels what it checks.
  - should assert: rename to `test_rate_limited_is_retryable_v2` or use a feature-gated helper that constructs a reqwest error (e.g., via `reqwest::get("http://127.0.0.1:1")` in tokio test) to actually cover `Unreachable`. Otherwise delete — it duplicates `test_rate_limited_is_retryable`.
- DELETE · `error.rs:75 test_rate_limited_is_retryable` · Redundant with the (misnamed) test above once that is fixed.
- KEEP · `error.rs:81 test_policy_denied_not_retryable` · Behavioral; guards the non-retryable policy.
- KEEP · `error.rs:87 test_credential_not_found_not_retryable` · Behavioral.
- KEEP · `error.rs:93 test_approval_required_not_retryable` · Behavioral.
- KEEP · `error.rs:99 test_config_error_not_retryable` · Behavioral.
- KEEP · `error.rs:105 test_rate_limited_retry_delay` · Checks the numeric retry_after round-trips into Duration — useful.
- KEEP · `error.rs:111 test_other_error_no_retry_delay` · Negative case for retry_delay. Useful.
- REWRITE · `error.rs:117 test_error_display` · Pins message text to exact string — this is a near-tautology of the `#[error(...)]` annotation. Low-value but not harmful; if kept, add one test per variant to make it a real contract.
  - should assert: all variants produce a Display string containing the variant-specific *fact* (e.g., the policy reason, the URL), without asserting the surrounding boilerplate.
- KEEP · `error.rs:123 test_rate_limited_display` · Same caveat as above; OK.
- Missing coverage: `OneCliError::Http(_)` retryability is claimed by `is_retryable` but never tested; consider building a reqwest error via a real failing request in a tokio test.

### crates/onecli-client/src/retry.rs

- KEEP · `retry.rs:80 test_default_retry_strategy` · Covers three distinct variant classifications in one test. Clear.
- KEEP · `retry.rs:89 test_execute_with_retry_success_first_attempt` · Verifies both result and call count. Good.
- KEEP · `retry.rs:106 test_execute_with_retry_max_retries_exceeded` · Verifies total attempts = retries + 1 and final err. Good.
- Missing coverage (important):
  - Success after N transient failures (e.g., fail-fail-succeed with attempts == 3).
  - Non-retryable error aborts immediately (attempts == 1 despite max_retries > 0).
  - Exponential backoff actually grows (inject a fake clock or expose backoff_ms) — currently untested.
  - `max_delay` clamp — also untested.
  These are the failure modes the retry module exists for; the current tests miss the interesting ones.

### crates/security-proxy/src/credentials.rs

- KEEP · `credentials.rs:141 test_detect_provider` · Covers 5 hosts including an unknown case. Note: doesn't cover `kimi`, `github`, `cloudflare`, or the substring-vs-exact-match semantics (e.g., `api.openai.com.evil.com` would match).
- REWRITE · `credentials.rs:163 test_format_auth_header` · Only checks 2 of 7 branches; misses `openrouter`, `kimi`, `github`, `google`, `cloudflare`, and the default fallback `_` arm.
  - should assert: one assertion per variant, so adding a new provider without updating the match arm is caught.
- KEEP · `credentials.rs:176 test_inject_no_credential` · Clean negative case.
- KEEP · `credentials.rs:184 test_inject_with_credential` · Positive case.
- KEEP · `credentials.rs:196 test_get_credential` · Positive + negative.
- KEEP · `credentials.rs:205 test_add_overwrites` · Good — pins the documented cache policy (first-write-wins is *not* what this proves; this proves last-write-wins via `add`, which contradicts the doc-comment on `ensure_cached`). Flag for investigation: spec says first-write-wins, but `add` overwrites unconditionally.
- Missing coverage (important):
  - `load_from_env` — reads `ZEROGATE_KEY_*`. Needs a serial test with temporarily-set env vars.
  - `ensure_cached` — non-trivial resolver path (env → fnox → vaultwarden). Currently zero coverage.
  - Substring-match foot-gun: `detect_provider("api.openai.com.evil.example")` currently returns `Some("openai")`. Add a test asserting this is `None` once fixed, OR add a test pinning current (insecure) behavior with a `TODO: tighten` comment.
  - Provider precedence: host containing multiple provider substrings — which wins? (deterministic order matters for security).

### crates/security-proxy/src/config.rs

- REWRITE · `config.rs:79 test_default_config` · Tautological: re-asserts the exact constants of the `Default` impl. Any change to defaults requires updating this test with no behavioral signal.
  - should assert: defaults pass a self-consistency invariant — e.g., if `mitm_enabled` is true, `ca_cert_path`/`ca_key_path` must be either both set or both None. Or: `bypass_domains` contains at least one loopback pattern.
- REWRITE · `config.rs:91 test_config_serialization` · Only checks `port`. If any other field silently drops on roundtrip (e.g., a skipped serde field), this test passes.
  - should assert: full structural equality after roundtrip (derive or manual compare of every field). Also include a non-default config to catch `skip_serializing_if` bugs.
- KEEP · `config.rs:99 test_verdict_equality` · Trivial but cheap; guards PartialEq derive.
- REWRITE · `config.rs:110 test_verdict_serialization` · `.contains("Block")` is brittle (passes on any `Block`-containing string) and doesn't verify the `reason` field is preserved structurally.
  - should assert: roundtrip and compare via `PartialEq`; plus `matches!(parsed, Verdict::Block { reason }) if reason == "exfiltration detected"`.
- Missing coverage: `Verdict::Log` has zero tests; `ExfilReport` and `InjectionReport` are entirely untested structurally.

### crates/security-proxy/src/scanner.rs

- KEEP · `scanner.rs:122 test_scan_empty_body` · Real behavioral assertion (verdict + findings empty).
- KEEP · `scanner.rs:133 test_scan_unicode_content` · Mildly useful as a smoke test that unicode doesn't panic the scanner.
- DELETE · `scanner.rs:142 test_scan_performance_sanity` · Flaky-by-design: a <1s threshold on a non-isolated CI runner is a coin flip on slow machines, and it tests nothing about correctness. Perf gates belong in a benchmark suite (criterion), not `#[test]`.
- Missing coverage (critical):
  - The interesting part of `ExfilScanner::scan` / `InjectionScanner::scan` is the *mapping* from `ScanVerdict::{Review,Unsafe}` → `Verdict::{Log,Block}`. Zero tests cover `Review` or `Unsafe` branches. Without a mockable scanner or a known-triggering payload, the Verdict mapping could regress silently.
  - `InjectionScanner::scan` on Unsafe prepends `"Response contains adversarial content: "` — entirely untested. This is the only place that text appears; a regression would be invisible.

### crates/security-proxy/src/scanner_test.rs

- DELETE (entire file) · `scanner_test.rs:1..82` · File is not wired into the crate: `lib.rs` declares only `pub mod scanner;`, no `scanner_test` module. File contains `use super::*;` but has no parent module that imports the scanner types, so this compiles only because it is *never compiled* (no `mod scanner_test` anywhere). Dead code masquerading as tests — if anyone thinks these run, they're wrong.
- If someone wires this file in, the tests largely duplicate `scanner.rs` inline tests with the addition of:
  - `test_scan_large_payload` — OK smoke test (1 MB of `x`), could be kept as one new test in `scanner.rs`.
  - `test_scan_malformed_url` — asserts `Allow | Log { .. }` which is too permissive to fail usefully. REWRITE to pin exact behavior.
  - `test_concurrent_scans` — useful deadlock/Send+Sync check. Could be kept, but move into `scanner.rs` inline.

### crates/security-proxy/src/proxy.rs

- KEEP · `proxy.rs:397 test_fetch_clean_content` · Real end-to-end via wiremock. `is_ok()` on an `AdversaryFetchResult` (not `Result`) — check it's not the reqwest-style meaningless `.is_ok()`; this is a domain-specific method. Acceptable.
- KEEP · `proxy.rs:412 test_fetch_blocks_injection` · Real behavioral: fetches a payload containing "IGNORE PREVIOUS INSTRUCTIONS" and asserts `is_blocked()`. Good.
- KEEP · `proxy.rs:430 test_fetch_blocked_content_not_in_result` · Strong security assertion: blocked reason must not re-echo the adversarial payload (avoids second-order injection via error propagation). Exactly the kind of test you want.
- KEEP · `proxy.rs:454 test_intercept_blocks_response_injection` · End-to-end, checks both status and body doesn't leak blocked content. Good.
- KEEP · `proxy.rs:496 test_intercept_passes_clean_response` · Matches the positive case.
- KEEP (but note the ignore) · `proxy.rs:535 test_intercept_injects_credentials` · Gated `#[ignore]` with a clear reason. Acceptable as documentation, but currently provides no coverage. If credential injection is security-critical (it is), this gap needs a different test: e.g., intercept-with-custom-`http_client`-resolver, or verify header construction without a network call.
- KEEP · `proxy.rs:577 test_intercept_scan_outbound` · Good behavioral test.
- KEEP · `proxy.rs:609 test_intercept_passes_safe_outbound` · Positive case.
- KEEP · `proxy.rs:640 test_intercept_bypasses_configured_domains` · Confirms bypass short-circuits scanning.
- REWRITE · `proxy.rs:666 test_check_bypassed` · The wildcard logic (`192.168.1.*`) is the interesting behavior; test covers one positive and one negative for it but misses: `192.168.2.1` (not matching), `192.168.1` without trailing char, URLs with `192.168.1.X` appearing in the *path* (substring-match bug — current implementation would match `https://evil.com/?redirect=192.168.1.1`). Also creates a fresh runtime inside a test that is already runnable sync — awkward.
  - should assert: (a) URL-path-embedded IPs do NOT bypass (this is likely a real bug given `url.contains(pattern)` semantics); (b) edge cases like `localhost.evil.com` — does it bypass? Should probably not.
- Missing coverage:
  - Hop-by-hop header stripping (`connection`, `te`, `trailers`, `transfer-encoding`, `upgrade`, etc.) — the list in `intercept` is load-bearing for HTTP correctness. No test verifies those are stripped.
  - `content-type` non-text responses (image/png) skip inbound scan — untested; a regression here would scan binary bytes as UTF-8.
  - Non-UTF-8 response body — the code uses `from_utf8(...).ok()`; should be tested to not crash.
  - Upstream-error pathway (`Err(e)` on `send()`) returns a `blocked_response` — untested.

### crates/security-proxy/tests/integration.rs

- DELETE (or DOWNGRADE) · `integration.rs:9 test_gateway_blocks_adversarial_content` · `#[ignore]`, and on network failure it swallows the error with `println!` and passes. That makes it silently-green. If kept, the `Err` arm must panic.
  - should assert: on `Err`, the test should fail (`panic!("gateway not running: {e}")`). Otherwise an outage or mis-config looks like a passing test.
- DELETE (or DOWNGRADE) · `integration.rs:41 test_gateway_allows_clean_content` · Same silent-green pattern on `Err`.
- REWRITE · `integration.rs:70 test_credential_injection_logic` · This is labeled "unit test" but duplicates `credentials.rs::test_inject_with_credential` and `test_detect_provider`. Same assertions, just run from a different crate boundary.
  - should assert: keep only if it adds integration coverage (e.g., verifies the public re-export path works, or uses only public types). Otherwise delete — it's a duplicate.
- KEEP · `integration.rs:99 test_agent_config_parsing` · Covers JSON deserialization + `all_providers()` aggregator. Real coverage, no duplicate elsewhere.

## Scope notes

- Audited: `crates/onecli-client/src/{client,config,error,retry}.rs` and `crates/security-proxy/src/{credentials,config,scanner,scanner_test,proxy}.rs` + `crates/security-proxy/tests/integration.rs`.
- Not audited (out of scope): `crates/security-proxy/src/{agent_config,audit,main}.rs`, `crates/security-proxy/src/lib.rs`, `crates/onecli-client/src/{lib,main,vault}.rs`. None of these contain `#[cfg(test)]` modules based on the grep above, except possibly `agent_config` and `audit` which did not appear in the test-grep — confirmed no inline tests to review.
- `crates/onecli-client/tests/` does not exist.

## Top priorities for follow-up

1. **False-positive safety nets.** Many `.is_ok()` / non-panic tests in `onecli-client/client.rs` can't fail. Either wire up mock-server integration tests or delete them outright — they give a misleading green.
2. **`scanner_test.rs` is dead code.** Either delete the file or wire it into `lib.rs`. Right now it lies about the coverage.
3. **Silent-green integration tests.** `integration.rs` tests swallow network errors and print them. Failing tests should fail.
4. **Missing negative tests on security-critical logic.** `check_bypassed` has a likely substring-match bug where URL-embedded IPs bypass scanning; `detect_provider` similarly. These need explicit negative tests.
5. **Perf-threshold test (`test_scan_performance_sanity`).** Delete or move to a benchmark — flaky-by-design.
6. **Duplicate default-config tautologies.** `test_default_config`, `test_retry_config_defaults`, `test_onecli_config_defaults` all re-assert the `Default` impl. Replace with behavioral invariants.


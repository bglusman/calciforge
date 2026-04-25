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


## Round 2: zeroclawed + adversary-detector + clashd

Incremental audit; findings appended as each file is evaluated.

### crates/clashd/src/domain_lists.rs

- KEEP · `domain_lists.rs:286 test_exact_match` · Real behavior: exact + case-insensitive + negative. Cheap and clear.
- KEEP · `domain_lists.rs:298 test_subdomain_match` · Positive + nested + negative for sibling-suffix (`example.net`). Good.
- KEEP · `domain_lists.rs:309 test_regex_pattern` · Covers two distinct patterns + negative. Good.
- KEEP · `domain_lists.rs:320 test_parse_hosts_format` · End-to-end of `parse`: comments, HOSTS entries, regex prefix, plain domain — useful regression fence.
- REWRITE · `domain_lists.rs:339 test_malware_urlhaus_format` · Asserts that a URL line `http://1.2.3.4/path/malware.exe` matches `1.2.3.4`. This is *host-only* matching, which silently discards the path. For a blocklist that is supposed to surface specific malicious URLs, this means `http://1.2.3.4/safe` also matches — likely a real false-positive bug. Name implies format compatibility but asserts lossy behavior.
  - should assert: pin the current host-only behavior AND add a TODO/negative test confirming path-specific URLs are NOT distinguished (so the lossy semantics are documented, not accidental).
- Missing coverage (important):
  - `matches` substring-match foot-gun: entry `example.com` also matches `fooexample.com`? (Actually no — the code uses `ends_with(".{}")`, so `fooexample.com` should not match. Worth pinning with a negative test: `fooexample.com` NOT in list containing `example.com`.)
  - Adversarial subdomain: entry `evil.com` — does `notevil.com` match? (Should NOT.) Add a test.
  - Empty list: `matches("anything")` should return false (guards against accidental default-allow inversion).
  - `parse` with invalid regex in a `~` line returns an error (tests the error path; currently no test verifies bad input is rejected).
  - HOSTS-format edge cases: `0.0.0.0\tfoo.com` (tab-separated, common in real HOSTS files) — current code only handles exact `"0.0.0.0 "`, single-space. Likely a real bug; add a test.
  - `DomainListManager` has zero tests. Dynamic refresh / multi-list aggregation is entirely unverified.

### crates/clashd/src/policy/eval.rs

- REWRITE · `eval.rs:201 test_load_valid_policy` · `.is_ok()` without inspecting anything. `PolicyEvaluator::new` only parses; any syntactically valid Starlark with or without an `evaluate` function passes. Doesn't prove the loaded policy is usable.
  - should assert: follow with an `evaluate(...)` call that returns `Verdict::Allow`; loading-then-invoking is the real contract.
- KEEP · `eval.rs:210 test_load_missing_policy` · Negative path is real: verifies `new()` returns Err for non-existent file. Acceptable (could tighten to match on a specific error message).
- KEEP · `eval.rs:217 test_evaluate_allow` · End-to-end: load + evaluate + check verdict + check reason is None. Good.
- KEEP · `eval.rs:229 test_evaluate_deny` · Same pattern.
- KEEP · `eval.rs:240 test_evaluate_review` · Covers the dict-return path AND reason propagation. Highest-value test in this file.
- KEEP · `eval.rs:253 test_evaluate_with_tool_arg` · Verifies the `tool` argument actually reaches the Starlark function (branching on it). Good behavioral assertion.
- Missing coverage (important):
  - Invalid verdict string (e.g., `return "maybe"`) — `verdict_from_string` error path is unreached.
  - Missing `evaluate` function — "Policy must define an 'evaluate' function" error unreached.
  - Syntactically valid Starlark but `evaluate()` returns a non-string/non-dict (e.g., integer) — unreached error path.
  - `args` and `context` propagation: currently only `tool` is tested. A policy that branches on `args["path"] == "/etc"` would prove `json_to_starlark` actually works for non-primitive JSON — the tree-conversion of Array/Object is entirely untested.
  - Runtime exception in Starlark (e.g., `fail("boom")`) — the error-mapping in `eval_function` is untested.

### crates/clashd/src/policy/engine/tests.rs

- KEEP · `tests.rs:15 test_engine_allows_by_default` · End-to-end allow path. Cheap positive case.
- KEEP · `tests.rs:33 test_engine_denies_when_policy_returns_deny` · Verdict + reason propagation. Good.
- KEEP · `tests.rs:52 test_engine_fail_closed_on_invalid_policy` · Security-critical: missing `evaluate` fn → deny, not allow. Strong test.
- KEEP · `tests.rs:74 test_engine_fail_closed_on_runtime_error` · Same fail-closed invariant on runtime error. Strong.
- KEEP · `tests.rs:95 test_domain_extraction_from_url` · Exercises url-parsing branch of `_extract_domain`.
- KEEP · `tests.rs:102 test_domain_extraction_from_domain_field` · Exercises the plain-string + alternate-field branch.
- KEEP · `tests.rs:109 test_domain_extraction_no_domain` · Negative case.
- REWRITE · `tests.rs:116 test_agent_config_loading` · Name promises "loading" but the assertion is that a policy that ignores agent config returns Allow. Doesn't actually verify agent config reached the policy — a policy with `return "allow"` would pass regardless of whether `set_agent_configs` worked at all.
  - should assert: use a Starlark policy that inspects `context["agent_allowed_domains"]` or `context["agent_denied_domains"]` and returns `deny` iff the config was propagated. Alternately assert via `context["agent_id"]` round-trip.
- Missing coverage (important):
  - Domain list integration: `domain_manager.matches(...)` result is injected into `context["domain_lists"]` — never tested end-to-end. A policy could assert on that field and prove the wiring. Right now a regression that silently drops domain-list injection would not be caught.
  - `parse_domain` edge cases: `http://` (no host), `example.com:8080` port stripping, uppercase domain lowered — none tested.
  - `extract_domain` field priority: if args have both `url` and `domain`, which wins? (Currently `url` due to iteration order — undocumented but testable.)
  - Empty `args` object / non-object `args` (array, scalar) — `extract_domain` should return None; untested.

### crates/adversary-detector/src/scanner.rs

- KEEP · `scanner.rs:247 test_clean_content` · Real positive case, tight assertion on `Clean`.
- KEEP · `scanner.rs:260 test_zero_width_chars` · Real layer-1 hit, behavioral.
- KEEP · `scanner.rs:270 test_unicode_tag_chars` · Same, for the U+E0000 tag range — distinct regex branch.
- REWRITE · `scanner.rs:280 test_css_hiding` · Comment admits layer1 catches injection first, yet the assertion is only `!v.is_clean()` — far too loose: it passes on Unsafe, Review, or anything non-Clean. Name promises testing CSS-hiding behavior but can't fail for the right reason.
  - should assert: give a content with pure CSS hiding (no "ignore previous instructions" phrase), then assert `matches!(v, ScanVerdict::Review { .. })`. Tighten to the specific branch.
- KEEP · `scanner.rs:291 test_injection_phrase` · Real behavioral.
- KEEP · `scanner.rs:301 test_pii_harvest` · Real behavioral.
- KEEP · `scanner.rs:311 test_exfiltration_signal` · Real behavioral.
- KEEP · `scanner.rs:321 test_discussion_context_suppression` · Strong: exercises the ratio heuristic's downgrade from Unsafe→Review. Important.
- KEEP · `scanner.rs:340 test_base64_blob_review` · Real: large-blob Review branch.
- KEEP · `scanner.rs:354 test_fallback_when_service_unreachable` · Important invariant — scanning never skipped on layer-3 outage. Strong test (uses port 19999 which may be bindable; OK in practice).
- KEEP · `scanner.rs:368 test_borderline_unicode_mixed_content` · Two sub-scenarios; real behavioral assertions for each. Slightly over-broad (could be two tests) but acceptable.
- KEEP · `scanner.rs:405 test_borderline_base64_with_legitimate_use` · Three scenarios, all tight assertions. Good.
- KEEP · `scanner.rs:439 test_discussion_context_edge_cases` · Tests the "weak injection, weak discussion" path stays clean AND "strong injection" stays unsafe. Good.
- KEEP · `scanner.rs:463 test_merge_verdict_stricter_wins` · Covers all ordering pairs of `merge`. Unit-test of the private helper, but justified given the public `scan` calls it.
- KEEP · `scanner.rs:507 test_extract_host` · Covers positive, port, subdomain, query, AND the critical "no-scheme → empty" negative (prevents bare-string matching). Good.
- KEEP · `scanner.rs:521 test_skip_protection_exact_match` · Positive + two negatives (sibling-subdomain foot-gun). Good.
- KEEP · `scanner.rs:532 test_skip_protection_wildcard` · Covers `*.example.com` matching root, sub, and deep-sub. Good.
- KEEP · `scanner.rs:544 test_skip_protection_empty_list` · Default negative — guards fail-open regression.
- Missing coverage (important):
  - `layer3_http` happy path: HTTP service returns `"review"` or `"unsafe"` and merge chooses the stricter. Zero tests verify the remote verdict is honored. Use wiremock.
  - `layer3_http` returns `"clean"` but layer2 said `Unsafe` — merge must keep `Unsafe`. Currently untested.
  - `digest_cache_ttl_secs` behavior — config field exists, no scanner-level test.
  - `override_on_review` — documented in comments but unreachable in current `scan` logic (field is consumed elsewhere). If used by callers, at least a unit test of config deserialization should pin the field name.
  - Non-UTF-8 byte sequences in `content` — `scan` takes `&str` so this is caller-filtered, but worth noting that a scanner that only handles valid UTF-8 can miss adversarial binary payloads.

### crates/adversary-detector/src/proxy.rs

- REWRITE · `proxy.rs:440 test_digest_cache_hit_skips_rescan` · Name promises verifying cache-hit skips the server (and `.expect(1)` is set on the mock), but the test only calls `fetch` ONCE. It then introspects `detector.store` to confirm the entry was written. The `.expect(1)` is enforced by wiremock on drop — but since there's only one call, a regression that re-fetched on the second call wouldn't be caught because there IS no second call.
  - should assert: make the second `detector.fetch(&url).await`, keep the `.expect(1)` on the mock, and assert both results have equal digests. That would actually test the named property.
- KEEP · `proxy.rs:471 test_digest_change_triggers_rescan` · Real behavioral via wiremock `up_to_n_times(1)` pair. First call Ok, second call Blocked — proves both rescan AND that cache-miss-on-changed-digest works. Strong test.
- KEEP · `proxy.rs:510 test_override_bypasses_block` · Full loop: block → mark_override → same URL re-fetched is Ok. Security-critical path, tightly asserted.
- KEEP · `proxy.rs:542 test_blocked_content_not_in_result` · Very strong: ensures injection payload strings don't leak through `Blocked.reason`. Exactly the kind of "second-order injection" guard that matters.
- REWRITE · `proxy.rs:575 test_review_verdict_prepends_warning` · The `AdversaryFetchResult::Ok { .. } => {}` arm treats Clean as acceptable — test can't fail for the wrong reason. Given that CSS hiding is documented to return Review, the Clean arm is a silent-pass escape hatch. Name promises testing the Review annotation.
  - should assert: use a payload that reliably triggers Review (e.g., CSS hiding pattern that doesn't overlap with Clean), and remove the `Ok => {}` fallthrough. Fail the test if Review isn't produced.
- KEEP · `proxy.rs:606 test_rate_limiter_burst_allowance` · Tight: exactly `burst_size` allowed then one rejected. Good.
- KEEP · `proxy.rs:633 test_rate_limiter_per_source_isolation` · Covers a real invariant (per-source buckets don't share tokens).
- REWRITE · `proxy.rs:659 test_rate_limiter_cooldown_calculation` · `cooldown_remaining` currently returns `Some(Duration::from_secs(self.config.cooldown_seconds))` unconditionally (ignores `_source`). Test passes regardless of rate-limit state because the impl is just a config echo. Tautological given impl.
  - should assert: either (a) rework `cooldown_remaining` to compute time-until-next-token from bucket state and test that, OR (b) drop the test as meaningless until the impl is behavioral. Currently it verifies only that `config.cooldown_seconds > 0`.
- Missing coverage (important):
  - Rate-limited request returns `Blocked { reason.contains("Rate limit") }` via the public `fetch` API — the integration between limiter and fetch is untested end-to-end.
  - `skip_protection_domains` end-to-end: a skip-protected URL serving an injection payload should come back as `Ok` (bypass). Currently untested — regression could accidentally enable scanning for bypass domains.
  - LRU eviction: `RATE_LIMITER_MAX_SOURCES = 10000` eviction path (`evict_if_needed`) has zero tests.
  - Token refill: after burst exhaustion, waiting should restore tokens at `max_requests_per_minute / 60` rate. No fake-clock test; cannot regress silently.
  - HTTP fetch error (e.g. DNS failure / 500) produces `Blocked { reason.contains("HTTP fetch failed") }` — untested.
  - `digest_cache_ttl_secs` expiring a cached entry and forcing rescan — config field exists, untested.
  - `override_on_review=true` config bypasses the Review wrapping — untested.

### crates/adversary-detector/src/digest.rs

- KEEP · `digest.rs:179 test_empty_store_returns_none` · Cheap invariant: empty store is actually empty.
- KEEP · `digest.rs:185 test_set_and_get_roundtrip` · Real end-to-end persistence: set, reopen from disk, get. Exactly the test that would catch serde drift. Strong.
- KEEP · `digest.rs:211 test_mark_override_sets_flag` · Covers before/after assert of override flag. Good.
- KEEP · `digest.rs:247 test_mark_override_wrong_digest_noop` · Negative/security-critical: override is scoped to exact digest. Essential guard against replay-with-new-content.
- REWRITE · `digest.rs:278 test_sha256_hex_deterministic` · The test encodes a *known* hash literal, which is good — but also asserts `a == b` (tautology: pure function is pure) and `a != sha256_hex("world")` (another tautology for a cryptographic hash). Only the known-vector assertion is load-bearing.
  - should assert: keep only `assert_eq!(sha256_hex("hello"), "2cf2...")` (the known vector). Drop the other two lines; they can't fail in a universe where sha2 works.
- KEEP · `digest.rs:291 test_ttl_expires_entry` · Three assertions cover None, expired-by-TTL, and within-TTL. Strong.
- KEEP · `digest.rs:320 test_ttl_zero_means_no_expiration` · Documents the `0 = never expires` convention indirectly (actually tests that an entry a year old with TTL-None still returns). Note: test calls `get(url, None)`, NOT `get(url, Some(0))`, so it's testing `None` means "no check," not the `0 → None` conversion that happens in `proxy.rs`. Name is slightly misleading.
  - should assert: keep as-is but rename to `test_none_ttl_disables_expiration_check`. Or add a separate test actually exercising `Some(0)` as an edge case (currently `Some(0)` would treat any entry older than 0s as expired — likely a bug; worth a test to pin behavior).
- Missing coverage (important):
  - Corrupt JSON file: `load` falls back to empty map with a warning. Untested — if serde drift breaks existing stores, this silent-empty behavior could lose override state.
  - Concurrent set/get: `DigestStore` takes `&mut self` for set, so it's behind a Mutex in caller — no concurrency test, but that's a proxy.rs concern.
  - `mark_override` on a URL that doesn't exist (not just wrong digest): no-op, untested.
  - Non-UTF-8 content in sha256_hex: impossible (takes `&str`), but worth noting if bytes-based hashing is desired later.

### crates/adversary-detector/src/middleware.rs

- KEEP · `middleware.rs:213 test_clean_passes_through` · Positive path end-to-end via the trait impl.
- KEEP · `middleware.rs:228 test_unsafe_blocks_content` · Strong: both presence of "ADVERSARY BLOCKED" tag AND absence of the injection payload. Security-critical.
- REWRITE · `middleware.rs:249 test_review_annotates_content` · Same silent-green pattern as `proxy.rs:575`: `PassThrough(_) => {}` arm accepts Clean, so if CSS hiding regresses to Clean the test still passes. Name promises testing Review annotation.
  - should assert: use a payload that reliably triggers Review (or make scanner assertions first to confirm the payload triggers Review before testing the middleware mapping); remove the Clean escape hatch.
- KEEP · `middleware.rs:266 test_non_intercepted_tool_passes_through` · Covers the short-circuit branch when the tool isn't in `intercepted_tools`. Useful.
- Missing coverage (important):
  - `should_intercept`/profile variation: test that a tool included in `all_including_exec` is scanned under Paranoid profile but NOT scanned under Balanced. Currently only Balanced is exercised and only via the hook. Profile-mapping correctness is unverified.
  - `scan_text` (public channel-scanning API) has zero tests.
  - `ToolResult::context_for` mapping is untested — a regression that maps `web_fetch → Api` would change audit-log categorization silently.
  - `InterceptedToolSet::{web_only, web_and_search, all_tools, all_including_exec}` constructors and `intercepts` are untested. The "paranoid includes exec, balanced does not" invariant is the whole point; no test pins it.
  - `audit_logging=false` does NOT emit a log entry — untested (would need a mock AuditLogger).

### crates/adversary-detector/src/profiles.rs

- KEEP · `profiles.rs:320 test_profile_from_str` · Covers all four primary names + two aliases. Parse contract.
- KEEP · `profiles.rs:348 test_profile_from_str_invalid` · Negative case; also checks the error-message mentions valid options. Good.
- REWRITE · `profiles.rs:357 test_all_profiles_build` · Only asserts `config.profile == p` (tautological — `from_profile` constructs with exactly that field) and `description().is_empty()` is false (tautological — description() matches on profile and returns a `&'static str` literal). Cannot fail.
  - should assert: validate a cross-field invariant per profile (e.g., Paranoid has `!enable_digest_cache`, Open has `override_on_review`). That's partially done in the next three tests, but those test only a specific profile — roll up a matrix test here.
- REWRITE · `profiles.rs:371 test_open_is_permissive` · Half-tautology: re-asserts `0.5`, `true`, `false` constants directly from the `open()` constructor. Will update in lockstep with the code. Does guard against a *typo* but not a behavioral regression.
  - should assert: a *behavioral* permissiveness invariant — e.g., `SecurityConfig::open().is_strictly_more_permissive_than(&SecurityConfig::balanced())` via a helper, OR anchor to actual scanner behavior by running a scan and asserting Open passes something Balanced blocks.
- REWRITE · `profiles.rs:380 test_paranoid_is_strict` · Same tautology pattern as above. Pins `15` explicitly — breaks if the rate is ever retuned, without any behavioral signal.
  - should assert: relational invariant (paranoid.rate < hardened.rate < balanced.rate < open.rate). Note: this is what the *next* test does, making this test mostly redundant.
- KEEP · `profiles.rs:390 test_profiles_are_progressively_stricter` · Relational: each profile stricter than the next. Good *and* the kind of test that catches accidentally-broken monotonicity (e.g., a retune that makes Hardened more permissive than Balanced). Strongest test in the file.
- Missing coverage (important):
  - `intercepted_tools` monotonicity: Open ⊂ Balanced ⊂ Hardened ⊂ Paranoid tool sets. Currently untested; a regression where Balanced *loses* `web_search` would pass.
  - `scan_outbound` monotonicity: false for Open/Balanced, true for Hardened/Paranoid. Untested.
  - `log_verbosity` monotonicity across profiles. Untested.
  - `digest_cache_ttl_secs` invariant: strictly non-increasing across profiles (24h → 1h → 5min → 0). Untested.
  - `SecurityProfile` roundtrip through serde (lowercase rename) is untested — a change to the serde rename would silently break YAML/TOML configs.

### crates/zeroclawed/tests/e2e/onecli_proxy.rs

- DELETE · `onecli_proxy.rs:16 test_proxy_openai_models_endpoint` · Silent-green: connection refused returns early with `println!`. Assumes a service listening on 8081. Also passes on `success OR 401` which is far too broad (assertion only catches 404). Even when "running" it doesn't assert credentials were injected — the test title lies.
  - should assert: spawn OneCLI in-process with a wiremock upstream; assert that the upstream received a request with `Authorization: Bearer <real-injected-token>` (not the dummy the test sent).
- DELETE · `onecli_proxy.rs:54 test_proxy_brave_uses_subscription_token_header` · Same silent-green pattern; asserts `status != 404` only — completely misses the stated intent (verifying `X-Subscription-Token` header). The header the test promises to check is never observed.
  - should assert: wiremock upstream, verify `X-Subscription-Token` present and `Authorization` absent.
- DELETE · `onecli_proxy.rs:91 test_proxy_preserves_request_body` · Same silent-green + only asserts `status != 404`. The word "preserves" is aspirational — body contents are never compared.
  - should assert: wiremock echoes request body; assert `resp.body == sent.body`, with particular attention to the `tools` array.
- DELETE · `onecli_proxy.rs:136 test_proxy_path_stripping` · Same silent-green; the `Err(_) => continue` arm on the third iteration makes this silently pass even when every call fails.
  - should assert: wiremock upstream records request path; assert path stripping yielded the expected upstream path for each case.

Summary: every test in this file is either silent-green-on-error or so permissive its assertion couldn't catch the exact bug cited in its own comment ("the bug we caught"). Recommend rewriting the file against an in-process OneCLI + wiremock fixture, or deleting it and trusting `onecli-client` unit tests.

### crates/zeroclawed/tests/e2e/config_sanity.rs

- DELETE · `config_sanity.rs:20 test_agents_after_memory_section_load` · The file comment cites "Agents defined after [memory] section were silently ignored" as the bug — but the test parses via raw `toml::Value` (not the zeroclawed Config struct that had the bug). Generic TOML parsing obviously doesn't care about ordering. Test does not exercise the buggy code path.
  - should assert: parse via `zeroclawed::config::Config` (the struct that had the bug), and check `config.agents.len() == 1`. Otherwise this is a test of the `toml` crate, not of zeroclawed.
- REWRITE · `config_sanity.rs:54 test_unknown_adapter_kind_fails` · The assertion calls a *test-local helper* `is_valid_adapter_kind` that re-implements the list. This is tautological — the test asserts that the helper it also defines returns true/false for the inputs it specifies. It does not test the real adapter-kind validator in the crate.
  - should assert: call the actual config-parsing code (e.g., via `Config::load(&path)` for a config with `kind = "openclaw"`) and assert an error is returned.
- DELETE · `config_sanity.rs:111 test_duplicate_agents_array_works` · Raw `toml::Value` parse, not the zeroclawed Config. Tests the `toml` crate's aggregation of `[[agents]]` tables — already well-covered upstream.
- DELETE · `config_sanity.rs:140 test_nzc_native_without_command` · Same problem: parses via raw `toml::Value` and asserts `get("command").is_none()` — which is true because the test author literally didn't write a `command` field. Tautology. Real question ("does nzc-native adapter config validate with no command field?") is not tested.
- DELETE · `config_sanity.rs:174 test_empty_agents_array_valid` · Raw toml parse; asserts `get("agents").is_none()` after writing a config without an agents section. Tautology.

Summary: this entire file tests the `toml` crate instead of the zeroclawed config loader. All five tests need to be rewritten to exercise `Config::load` (or whatever the real loader is) or deleted. Currently they provide no regression coverage for the bugs described in their own comments.

### crates/zeroclawed/tests/e2e/adapter_edge_cases.rs

- DELETE (whole file) · `adapter_edge_cases.rs:1..222` · File header: "self-contained, no zeroclawed imports." The `run_cmd` helper is entirely test-local — these tests exercise `std::process::Command` and `/bin/echo` / `/bin/sh` / `/bin/false`, not any zeroclawed adapter code. They test the Rust stdlib + the host's coreutils, not the CLI adapter in `crates/zeroclawed/src/adapters/cli.rs`. The "adapter" in the filename is misleading.
  - individual notes (if the file is kept):
    - `test_binary_not_found` (62): tests `Command::spawn`, not the adapter.
    - `test_timeout_produces_clear_error` (74): tests the local `run_cmd` helper's timeout logic. Could be flaky under extreme load.
    - `test_echo_passes_message` (93): tests `/bin/echo`.
    - `test_shell_safety` (108): claim-title is misleading — it tests that `std::process::Command::args` does NOT shell-interpret (which is a Rust stdlib property, not an adapter property). Doesn't test zeroclawed's shell-safety at all.
    - `test_empty_message` (126): `echo ""` works. Low-value.
    - `test_exit_code_propagation` (137): tests the local helper's Err-on-failure behavior.
    - `test_stderr_capture` (149): tests the local helper.
    - `test_env_passthrough` (163): tests `Command::envs`.
    - `test_path_not_injected` (180): test body doesn't actually attempt PATH injection — asserts `echo safe` contains "safe". Name is aspirational; test is trivial.
    - `test_two_instances_isolated` (194): two sequential `echo` calls. Not isolated in any meaningful sense.
    - `test_invalid_utf8_handled` (207): `Ok(s) | Err(s) => { let _ = s.len(); }` — the match arm literally discards the result. Cannot fail (only would fail if `run_cmd` panicked). Pure non-panic smoke test.

Recommendation: replace the whole file with tests against the actual `CliAdapter` in `crates/zeroclawed/src/adapters/cli.rs`. Every test here either tests the stdlib or the test-local helper; zero test zeroclawed behavior.

### crates/zeroclawed/tests/e2e/property_tests.rs

- DELETE · `property_tests.rs:12 test_url_reconstruction_lossless` · The test implements its OWN `strip_prefix` logic inline, then asserts the output equals `path`. It does not call any zeroclawed code. Tautological: tests `str::strip_prefix` (stdlib).
  - should assert: invoke the real OneCLI path-stripping function, not reimplement it.
- REWRITE · `property_tests.rs:36 test_tool_payload_preservation` · Serde `to_string` → `from_str` roundtrip on `serde_json::Value` always preserves structure — this is tested by the serde_json crate itself. Tests the wrong thing; doesn't exercise zeroclawed's tool-payload handling.
  - should assert: roundtrip through whatever zeroclawed type wraps tool definitions, not a bare `serde_json::Value`.
- DELETE · `property_tests.rs:72 test_adapter_kind_exhaustive` · The test defines `valid_kinds` as an array AND the `matches!` block as two copies of the same list. Asserts the two copies agree — pure tautology. Catches only transcription errors between the array and the match block, both of which are in the test.
  - should assert: pass the kind string to the real `Config`/adapter loader in zeroclawed and assert accept/reject.
- DELETE · `property_tests.rs:105 test_phone_normalization_idempotent` · Defines `normalize_phone` inline (not imported from zeroclawed). Tests a test-local helper's idempotence — a property of the helper itself, not of any zeroclawed code.
- DELETE · `property_tests.rs:123 test_phone_normalization_plus_prefix` · Same: tests the test-local helper. If zeroclawed has a real phone-normalization function, this property is worth testing against *that* function.

Summary: every property test in this file tests a test-local helper or the stdlib, not zeroclawed code. Property-based testing is valuable, but only when pointed at the system under test. Recommend rewriting against the real zeroclawed functions (URL-stripper, tool-payload type, phone normalizer, adapter-kind validator) or deleting the file. Currently provides zero regression coverage for the crate.

### crates/zeroclawed/tests/e2e/security_tests.rs

- DELETE (whole file) · `security_tests.rs:1..263` · Same problem as `adapter_edge_cases.rs`: the file header says "no zeroclawed imports" and it means it. Every test invokes `/bin/echo`, `/bin/env`, or `nonexistent_bin_xyz` via `std::process::Command` directly. Zero of these exercise zeroclawed code. The "security properties" tested are properties of the POSIX shell, not of zeroclawed.
  - individual notes:
    - `test_error_no_file_path_leak` (21): tests that `spawn("nonexistent_bin_xyz")` error doesn't contain "/root" or "/etc". It's the kernel's ENOENT message — a property of libc, not zeroclawed.
    - `test_error_no_credential_leak` (39): runs `env NONEXISTENT_VAR` and asserts stderr doesn't contain "password"/"token". env's error message is "NONEXISTENT_VAR: No such file" — of course it doesn't contain "password". Tautology. Security claim is aspirational.
    - `test_injection_payloads_safe` (79): asserts `echo 'ignore previous instructions'` outputs "ignore previous instructions" literally. Tests `echo`, not zeroclawed's prompt handling. Name promises injection safety — test is trivial.
    - `test_env_secret_not_leaked` (120): sets `SECRET_KEY=sk-secret-12345`, runs `echo hello`, asserts output doesn't contain the secret. `echo` does not read environment variables unless you pass `$SECRET_KEY`, which the test does not. Tautology — it can't leak what it doesn't reference.
    - `test_empty_input_handling` (151): `echo ""` exits 0. Trivial.
    - `test_long_input_handling` (172): `let _ = output.status;` — result is discarded. Cannot fail.
    - `test_unicode_input_handling` (203): `Ok(_) => {}` arm — cannot fail on success path. Pure non-panic smoke.
    - `test_concurrent_subprocess_safety` (239): tests `std::thread` + `std::process::Command`. Not a zeroclawed test.

Recommendation: delete or repoint. If there are zeroclawed security properties worth testing (there are — credential-injection sanitization, adversary-detector fail-closed, etc.), write tests that exercise those actual code paths. The current file is a security theater directory.

### crates/zeroclawed/tests/loom.rs

- REWRITE · `loom.rs:33 test_concurrent_registry_access` · Uses `loom::sync` primitives but the types exercised are `loom::sync::Mutex<HashMap<String,String>>` — that's loom validating its OWN Mutex correctness, not validating any zeroclawed code. The comment says "similar to AdapterRegistry" but never imports `AdapterRegistry`. The final assertion (`len() == 2`, values match) is trivially true once both joins succeed, because there's only one writer.
  - should assert: exercise the actual `AdapterRegistry` under loom, or at least a `#[cfg(loom)]` stub that shares the same locking discipline.
- REWRITE · `loom.rs:67 test_concurrent_session_management` · Same pattern: tests loom's RwLock on a HashMap, not the ACP session-management code. The final `len() == 3` and 3 value lookups are trivially deterministic given the writers don't race (each inserts different keys).
- REWRITE · `loom.rs:110 test_arc_lifecycle` · Asserts `*guard == 2` after two increment threads (correct) and `Arc::strong_count == 1` at the end. These are Arc+Mutex properties — loom validating its own primitives. Not zeroclawed code.
- REWRITE · `loom.rs:138 test_message_passing_pattern` · Comment says "simulates the mpsc pattern used in send_streaming" but no mpsc is used. Producer does 3 `fetch_add`s; final assertion is `counter == 3`. No race can make this false because producer runs to completion before join. Can't fail.
- REWRITE · `loom.rs:170 test_no_deadlock_with_consistent_ordering` · Explicitly acquires both locks in the same order, which is the documented way to NOT deadlock. A test that asserts "if I don't do the buggy thing, there is no bug" is not useful. The interesting test would be opposite-order acquisition to prove the *deadlock detector* catches it — but this test intentionally avoids that.
  - should assert: either write a matching `#[should_panic]` test that acquires in inverse order and proves loom detects the deadlock, OR delete this test as vacuous.
- REWRITE · `loom.rs:199 test_session_cache_invalidation_pattern` · Comment admits "This is a template for future integration tests." It's a placeholder. Writer and reader don't share any data dependency worth observing; reader's `if let Some(&active) = ...` block discards the value. Cannot fail.

Summary: every test in this file tests loom's own primitives and has no reference to any type defined in the `zeroclawed` crate. The tests are well-intentioned templates but currently provide no coverage of concurrent code paths in zeroclawed itself. Either wire them up to real types (`AdapterRegistry`, session caches, request queues) or mark them as examples/docs, not as regression tests.

### crates/zeroclawed/src/auth.rs

- SECURITY FLAG (non-test): the test fixture `make_config()` at `auth.rs:97-108` hard-codes Telegram numeric IDs `8465871195` and `15555550002` attached to an `owner`-role identity named "brian". Per CLAUDE.md, "Real chat identifiers (Matrix handles, Discord user-ids, Telegram chat ids) tied to specific users" must not be committed to this public repo. At minimum, verify with the maintainer that these are not real; if they are, rotate/replace with RFC-style placeholders (e.g., `1`/`2`) and rename "brian" to `user_a`/`user_b`. This is a CLAUDE.md violation that the scanner may or may not catch.
- KEEP · `auth.rs:155 test_resolve_known_telegram_sender` · Real behavioral: identity + role propagation through resolution.
- KEEP · `auth.rs:165 test_resolve_unknown_telegram_sender_drops` · Negative / fail-closed — security-critical.
- KEEP · `auth.rs:175 test_resolve_second_identity` · Confirms iteration doesn't short-circuit on first identity. Good.
- KEEP · `auth.rs:183 test_resolve_channel_sender_generic` · Generic resolver positive.
- DUPLICATE of `test_wrong_channel_drops` · `auth.rs:191 test_resolve_wrong_channel_drops` · Near-identical to `test_wrong_channel_drops` (line 283). Keep one, delete the other.
- KEEP · `auth.rs:199 test_default_agent_for_known_identity` · Positive routing lookup.
- KEEP · `auth.rs:206 test_default_agent_for_unknown_identity` · Negative routing lookup.
- KEEP · `auth.rs:213 test_is_agent_allowed_unrestricted` · Critical policy: empty allowed_agents = unrestricted. Good.
- DUPLICATE of `test_is_agent_allowed_empty_means_unrestricted` · `auth.rs:213` and `auth.rs:292` both assert the same empty-means-unrestricted rule on "brian". Keep one.
- KEEP · `auth.rs:220 test_is_agent_allowed_restricted` · Positive + negative of the restricted case. Good.
- KEEP · `auth.rs:228 test_is_agent_allowed_no_routing_rule` · Fail-closed for unknown identity. Security-critical.
- KEEP · `auth.rs:234 test_find_agent_exists` · Real lookup check; asserts an inner field (endpoint). Good.
- KEEP · `auth.rs:242 test_find_agent_missing` · Negative lookup.
- KEEP · `auth.rs:251 test_resolve_with_empty_identities` · Tests fail-closed on an entirely empty config. Good cross-function smoke.
- KEEP · `auth.rs:273 test_resolve_sender_id_as_string_not_integer` · STRONG: "leading zeros should not match" guards a subtle string-vs-int comparison bug.
- DELETE · `auth.rs:282 test_wrong_channel_drops` · Duplicate of `test_resolve_wrong_channel_drops` at line 191.
- DELETE · `auth.rs:291 test_is_agent_allowed_empty_means_unrestricted` · Duplicate of `test_is_agent_allowed_unrestricted` at line 213.
- KEEP · `auth.rs:299 test_unknown_channel_kind_drops` · Unknown channel kind (e.g. `discord`) → None. Important as new channels are added.
- KEEP · `auth.rs:306 test_empty_sender_id_drops` · Empty sender ID → None. Edge case worth pinning.
- Missing coverage:
  - Aliases list with MULTIPLE entries per identity: test that the second entry in `aliases` also resolves (currently each identity has only one alias in the fixture).
  - Identity with an empty aliases vec — behavior untested.
  - Role `None` — does resolution still work? Covered by default case but worth an explicit test.
  - Case sensitivity of `channel_kind` — `Telegram` vs `telegram` — is this case-sensitive? Spec ambiguous; test would pin it.

### crates/zeroclawed/src/proxy/auth.rs

- KEEP · `proxy/auth.rs:127 test_model_matches_exact` · Positive + negative exact match. Good.
- KEEP · `proxy/auth.rs:136 test_model_matches_wildcard` · Prefix wildcard with positive + negative cross-provider. Good.
- KEEP · `proxy/auth.rs:143 test_model_matches_wildcard_star` · Universal `*` match. Important; distinct branch.
- KEEP · `proxy/auth.rs:160 test_check_model_access_allow_all` · Covers AllowAll default policy with a non-existent agent — exactly the fail-open branch you want pinned.
- KEEP · `proxy/auth.rs:191 test_check_model_access_deny_all` · DenyAll fail-closed. Security-critical.
- KEEP · `proxy/auth.rs:217 test_check_model_access_agent_specific` · Strong: tests allowed-list positive (including wildcard branch) AND negative AND the AllowConfigured-means-deny-unknown-agent invariant.
- KEEP · `proxy/auth.rs:268 test_check_model_access_blocked_models` · Strong: blocked-list takes precedence over `"*"` allow. Security-critical; exactly the kind of precedence bug you want tested.
- Missing coverage:
  - `model_matches` edge cases: empty pattern, empty model, pattern `""` vs `"*"`, pattern `"a/b/*"` (nested prefix), pattern ending in `/*` but model exactly equal to prefix-without-slash. Foot-gun zone.
  - Conflict: both allowed and blocked contain overlapping patterns — blocked wins (tested implicitly) but order-of-evaluation is worth pinning.
  - `AllowConfigured` policy + agent IS configured but has empty `allowed_models` — per impl, empty means unrestricted; test should confirm.
- Note: commented-out `validate_api_key` and `constant_time_eq` are dead code. Corresponding commented test for `constant_time_eq` at :312 should be dropped too. If constant-time comparison comes back, restore both.

### crates/zeroclawed/src/config/validator.rs

- No active tests. The module has a commented-out `mod tests` block (`validator.rs:281-289`) with a TODO: "config structs have changed significantly… Tests removed temporarily due to struct changes."
- CRITICAL gap: this is the validator that gates agent/identity/alloy misconfiguration. Zero current coverage for:
  - Duplicate-ID detection (identity, agent, alloy, model-shortcut)
  - Routing-rule references non-existent agent
  - Alloy `weighted` strategy with zero total weight
  - Alloy with no constituents (warning)
  - Proxy bind address invalid (should produce error)
  - Proxy `timeout_seconds == 0` (error) and `> 3600` (warning)
  - Proxy backend_type whitelist (accept valid, reject unknown)
  - Security profile whitelist
  - `validate_toml_syntax` on malformed TOML
- Recommendation: restoring these tests is higher priority than almost any other Round 2 finding — the validator is the safety net for user-authored config.

### crates/zeroclawed/src/sync.rs

- DELETE · `sync.rs:132 test_shared_mutex` · Single-threaded smoke test. Mutates via one clone, reads via another, both in the same thread. `.lock().is_ok()` is infallible for a std Mutex held by this thread. Test can only fail if the locking API changes shape.
- DELETE · `sync.rs:152 test_shared_rwlock` · Same pattern: single-threaded write-then-read on two clones. Tests nothing about RwLock semantics — those are stdlib guarantees.
- DELETE · `sync.rs:170 test_atomic_types` · `store(42); load()` returning 42 — tests the stdlib atomic. Cannot fail in a universe where Rust's atomic types work.

The entire module is a thin conditional-re-export of std/loom primitives. If any test here is worth keeping, it'd be a loom-gated test that proves a specific ZeroClawed concurrent type (not std primitives) is race-free. Currently there are none.

### crates/zeroclawed/src/adapters/mod.rs

- KEEP (group) · `build_openclaw_adapter`, `build_zeroclaw_adapter`, `build_cli_adapter`, `build_acp_adapter`, `build_openclaw_native_adapter`, `build_nzc_native_adapter` · Each just asserts `adapter.kind() == "<same string>"`. Shallow but cheap — they serve as a smoke that every `kind` branch in `build_adapter` compiles and returns something. The per-test assertion is weak (one field, one branch) but collectively they catch the common "rename kind string without updating factory" regression.
- KEEP · `adapters/mod.rs:446 test_build_unknown_kind_returns_error` · Real negative: unknown kind → Err, and err message contains "unknown agent kind". Good.
- KEEP · `adapters/mod.rs:471 test_build_zeroclaw_missing_api_key_returns_error` · Real negative: required-field enforcement.
- KEEP · `adapters/mod.rs:523 test_build_acp_missing_command_returns_error` · Same pattern.
- KEEP · `adapters/mod.rs:548 test_build_cli_missing_command_returns_error` · Same.
- REWRITE · `adapters/mod.rs:573 test_adapter_error_display` · Missing the `ApprovalPending` variant — four-variant enum tested on only three. If a new variant is added, test doesn't force-update.
  - should assert: add a case for `ApprovalPending { request_id, reason, command }` with expected "🔒 Approval pending — request_id=…, command=…" format.
- REWRITE · `adapters/mod.rs:586 test_openclaw_uses_api_key_over_auth_token` · Name claims to test precedence, but the only assertion is `adapter.kind() == "openclaw-http"`. Adapter doesn't expose the selected token, so the test can't actually verify which of `api_key` or `auth_token` was chosen. This is a "tests only non-panic" case.
  - should assert: either make the adapter expose its auth via a debug method (feature-flagged), OR exercise through a wiremock server and observe the `Authorization` header. Otherwise the test name is aspirational.
- REWRITE · `adapters/mod.rs:667 test_openclaw_native_uses_api_key` · Same pattern as above; asserts only `kind()`.
- REWRITE · `adapters/mod.rs:691 test_nzc_native_uses_auth_token_fallback` · Same.
- REWRITE · `adapters/mod.rs:713 test_openclaw_native_builds_without_token` · "Should build even with no token" — but assertion is only `kind() == "openclaw-native"`. Doesn't test that requests are actually sent WITHOUT an Authorization header. If the adapter silently sent `Authorization: Bearer ` (empty Bearer) that could be a security issue, and this test wouldn't catch it.
- Missing coverage:
  - `openclaw-channel` factory branch has no build-test (only kind-smoke absent for it).
  - `nzc-http` factory branch has no build-test.
  - Precedence order when BOTH `api_key` AND env var `ZEROCLAWED_AGENT_TOKEN` are set — untested.
  - `DispatchContext::message_only` is constructed but never asserted-against.
  - `RuntimeStatus` default impl (`None`) untested.

Note: `test-librarian` with `api_key: "REPLACE_WITH_HOOKS_TOKEN"` is a placeholder, which is fine per CLAUDE.md conventions.

### crates/zeroclawed/src/router.rs

- DELETE · `router.rs:198 test_router_creates` · `let _r = Router::new();` — pure non-panic smoke. Cannot fail.
- KEEP · `router.rs:203 test_unknown_kind_returns_error` · Negative path. Duplicates `adapters/mod.rs:446` but through the router surface — integration-level, defensible.
- REWRITE · `router.rs:227 test_dispatch_openclaw_unreachable` · `assert!(result.is_err())` — doesn't inspect the error variant. A `Protocol(...)` error would also pass, which isn't the intended "unreachable" case.
  - should assert: match on `AdapterError::Unavailable(_)` or `Timeout`, not any `Err`.
- REWRITE · `router.rs:236 test_dispatch_zeroclaw_unreachable` · Same issue as above.
- KEEP · `router.rs:245 test_dispatch_cli_echo` · Real end-to-end via `/bin/echo`: asserts the exact output. Good. (Cross-platform caveat: `/bin/echo` path is Linux/macOS only.)
- REWRITE · `router.rs:255 test_dispatch_cli_bad_binary` · Another "asserts .is_err()" without checking the variant. Should confirm `Unavailable` / spawn-failure mapping.
- KEEP · `router.rs:288 test_openclaw_http_adapter_does_not_intercept_slash_commands` · Excellent test. Raw TCP listener captures request, asserts verbatim `/status` forwarded to server AND the mock's SSE response round-trips back. Exactly the shape you want for adapter-behavior regression coverage. Tiny note: 10ms `sleep` before dispatch is a scheduler race mitigation, not a correctness issue.

## Round 2 scope-coverage summary

### Fully audited in Round 2

- `crates/clashd/src/domain_lists.rs`
- `crates/clashd/src/policy/eval.rs`
- `crates/clashd/src/policy/engine.rs` (via `engine/tests.rs`)
- `crates/adversary-detector/src/scanner.rs`
- `crates/adversary-detector/src/proxy.rs`
- `crates/adversary-detector/src/digest.rs`
- `crates/adversary-detector/src/middleware.rs`
- `crates/adversary-detector/src/profiles.rs`
- `crates/zeroclawed/tests/e2e/onecli_proxy.rs`
- `crates/zeroclawed/tests/e2e/config_sanity.rs`
- `crates/zeroclawed/tests/e2e/adapter_edge_cases.rs`
- `crates/zeroclawed/tests/e2e/property_tests.rs`
- `crates/zeroclawed/tests/e2e/security_tests.rs`
- `crates/zeroclawed/tests/loom.rs`
- `crates/zeroclawed/src/auth.rs`
- `crates/zeroclawed/src/proxy/auth.rs`
- `crates/zeroclawed/src/config/validator.rs` (status: no tests; gap documented)
- `crates/zeroclawed/src/sync.rs`
- `crates/zeroclawed/src/adapters/mod.rs`
- `crates/zeroclawed/src/router.rs`

### Unaudited in Round 2 (inline `#[cfg(test)]` modules, in-scope but out of time)

Listed roughly in descending test-count; those with more tests / security-critical names should be prioritized for a follow-up pass.

High priority (security/correctness-adjacent, large test surfaces):
- `crates/zeroclawed/src/commands.rs` (44 tests — by far the largest inline test module)
- `crates/zeroclawed/src/install/ssh.rs` (23)
- `crates/zeroclawed/src/install/cli.rs` (23)
- `crates/zeroclawed/src/context.rs` (19)
- `crates/zeroclawed/src/config.rs` (18)
- `crates/zeroclawed/src/install/executor.rs` (17)
- `crates/zeroclawed/src/adapters/cli.rs` (17)

Medium priority (moderate surface):
- `crates/zeroclawed/src/channels/whatsapp.rs` (16)
- `crates/zeroclawed/src/install/model.rs` (13)
- `crates/zeroclawed/src/channels/telegram.rs` (13)
- `crates/zeroclawed/src/channels/signal.rs` (13)
- `crates/zeroclawed/src/adapters/acpx.rs` (13)
- `crates/zeroclawed/src/adapters/openclaw_native.rs` (12)
- `crates/zeroclawed/src/channels/matrix.rs` (11)
- `crates/zeroclawed/src/adapters/openclaw.rs` (11)

Low priority (smaller surfaces):
- `crates/zeroclawed/src/providers/alloy.rs` (9)
- `crates/zeroclawed/src/install/health.rs` (9)
- `crates/zeroclawed/src/adapters/nzc_native.rs` (9)
- `crates/zeroclawed/src/install/json5.rs` (8)
- `crates/zeroclawed/src/adapters/zeroclaw.rs` (7)
- `crates/zeroclawed/src/persistent_context.rs` (6)
- `crates/zeroclawed/src/hooks/memory.rs` (6)
- `crates/zeroclawed/src/adapters/acp.rs` (6)
- `crates/zeroclawed/src/install/wizard.rs` (5)
- `crates/zeroclawed/src/proxy/traceloop/test.rs` (4)
- `crates/zeroclawed/src/unified_context.rs` (3)
- `crates/zeroclawed/src/proxy/gateway.rs` (3)
- `crates/zeroclawed/src/install/migration_types.rs` (3)
- `crates/zeroclawed/src/proxy/helicone_router.rs` (2)
- `crates/zeroclawed/src/proxy/alloy_router.rs` (2)
- `crates/zeroclawed/src/providers/mod.rs` (2)
- `crates/zeroclawed/src/adapters/openclaw_channel.rs` (2)

### Round 2 cross-cutting themes

1. **"Tests its own helper, not zeroclawed code."** `tests/e2e/{config_sanity, adapter_edge_cases, property_tests, security_tests}.rs` and `tests/loom.rs` all follow this pattern: a test-local helper is defined in the test file and then validated. No production code path is exercised. If repointed at real zeroclawed types, these files could provide real coverage; as-is they are aspirational.
2. **Silent-green on network errors.** `tests/e2e/onecli_proxy.rs` follows the same pattern flagged in Round 1's `security-proxy/tests/integration.rs` — if the server is unreachable, the test `return`s with a `println!` and passes.
3. **Silent-green on "Clean OR Review OR Ok"-style match arms.** Seen in `middleware.rs:249`, `proxy.rs:575`, `scanner.rs:280`. When a test "passes either way" it can't distinguish intended behavior from regression.
4. **Default-constant tautologies.** `profiles.rs:357,371,380` re-assert the same constants the constructor hard-codes. Same pattern as Round 1's `test_default_config`/`test_retry_config_defaults`. Replace with behavioral invariants (monotonicity, self-consistency) — `profiles.rs:390 test_profiles_are_progressively_stricter` is a good model.
5. **Validator has NO tests (`config/validator.rs`).** A 290-line module that gates agent/identity/alloy/proxy/security config has a commented-out `mod tests` block. This is the single biggest coverage gap found in Round 2 — higher impact than any individual REWRITE above.
6. **Security fixture contains apparent real Telegram IDs** (`auth.rs:99, 108`). Per CLAUDE.md this public repo must not ship real chat IDs. Verify and sanitize.
7. **Rate-limiter tautology** (`proxy.rs:659 test_rate_limiter_cooldown_calculation`): impl echoes config verbatim; test verifies the echo. Either make cooldown dynamic or drop the test.


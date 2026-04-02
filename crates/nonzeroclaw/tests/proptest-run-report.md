# Proptest Run Report — 2026-03-21

## Summary

Integrated property-based allowlist tests across 5 channel types and ran them
to verify the core invariant: **allowlist matching must be exact — no substring,
prefix, or normalization mismatch should allow unauthorized access.**

**Result: All 21 property tests pass. No counterexamples found. No allowlist
bypass bugs detected in the current implementation.**

---

## Actions Taken

### 1. Repository Setup
- Created branch `fix/tests-proptest` from `main` at commit `c595d68`.
- Existing files `allowlist_proptest.rs` and `whatsapp_allowlist_tests.rs` were
  present but **not wired into the module tree** — they compiled to nothing.

### 2. Initial Test Run (`cargo test -p nonzeroclaw`)
- **2917 passed, 3 failed, 2 ignored** (pre-existing failures, unrelated):
  - `gateway::tests::security_timeout_is_30_seconds` — assertion `600 != 180`
  - `security::prompt_guard::tests::blocking_mode_works` — guard pattern issue
  - `security::prompt_guard::tests::detects_secret_extraction` — guard pattern issue

### 3. Test Scaffolding Fixes
- **Wired test modules** into `channels/mod.rs`:
  ```rust
  #[cfg(test)] mod allowlist_proptest;
  #[cfg(test)] mod whatsapp_allowlist_tests;
  ```
- **Visibility fixes** — allowlist check methods were private; changed to
  `pub(crate)` for test access (minimal change, no behavioral impact):
  - `WhatsAppChannel::is_number_allowed`
  - `DiscordChannel::is_user_allowed`
  - `IMessageChannel::is_contact_allowed`
  - `TelegramChannel::is_user_allowed`
  - `SignalChannel::is_sender_allowed`
- **Import path fix** — original files used `super::whatsapp::...` but from the
  inner `tests` module this resolves to `allowlist_proptest::whatsapp` (wrong).
  Fixed to `super::super::whatsapp::...`.

### 4. Proptest Dev-Dependency
- Already present in `Cargo.toml`: `proptest = "1.0"` ✅

### 5. Expanded Proptest Coverage
Rewrote `allowlist_proptest.rs` with comprehensive property tests for all 5 channels:

| Channel   | Properties Tested | Status |
|-----------|-------------------|--------|
| WhatsApp  | exact_match, prefix_must_not_match, suffix_must_not_match, empty_denies_all, wildcard_allows_all | ✅ All pass |
| Discord   | exact_match, empty_denies_all, wildcard_allows_all, prefix_must_not_match | ✅ All pass |
| iMessage  | exact_match, case_insensitive, empty_denies_all, wildcard_allows_all | ✅ All pass |
| Telegram  | exact_match, at_prefix_normalization, empty_denies_all, wildcard_allows_all | ✅ All pass |
| Signal    | exact_match, prefix_must_not_match, empty_denies_all, wildcard_allows_all | ✅ All pass |

Plus 5 deterministic unit tests in `whatsapp_allowlist_tests.rs`.

**Total: 26 new tests, all passing.**

### 6. Cargo-Mutants Smoke Run
- **Status: BLOCKED** by the 3 pre-existing test failures (cargo-mutants requires
  a clean baseline).
- 17 unique mutants were identified across the allowlist functions.
- Based on test design, **all 17 mutants should be killed** by our proptest suite
  (tests cover return-true, return-false, wildcard/exact swap, equality inversion).
- See `tests/mutants/survivors.txt` for the full mutant listing.

### 7. Counterexamples
**None.** No property test failures were observed.

The current implementations all use exact string equality (`==`) or
case-insensitive comparison (`eq_ignore_ascii_case`), which correctly prevents
the class of bugs described (substring matching, normalization mismatch).

---

## Analysis: Allowlist Bug Class (Substring/Normalization Mismatch)

The feared bug class was:
- An allowlist entry like `+1234567890` inadvertently matching
  `+12345678901` (prefix match) or `+123456789` (substring match)
- Normalization mismatches (e.g., `@user` vs `user` for Telegram)

**Finding:** The current code is NOT vulnerable to this bug class because:

1. **WhatsApp** uses `n == phone` (exact equality) — correct ✅
2. **Discord** uses `u == user_id` (exact equality) — correct ✅
3. **iMessage** uses `u.eq_ignore_ascii_case(sender)` (case-insensitive exact) — correct ✅
4. **Telegram** normalizes both sides (strip `@`, lowercase) then compares
   with `==` — correct ✅
5. **Signal** uses `u == sender` (exact equality) — correct ✅

All channels also correctly implement wildcard (`*`) handling before the
exact match check.

---

## Recommendations

1. **Fix pre-existing test failures** to unblock cargo-mutants:
   - `gateway::tests::security_timeout_is_30_seconds` — update assertion to match
     current timeout value (600 vs expected 180)
   - `security::prompt_guard` tests — investigate changed guard patterns

2. **Consider Matrix channel** proptest coverage (requires `channel-matrix` feature).
   The `MatrixChannel::is_user_allowed` uses the same `eq_ignore_ascii_case` pattern
   and has been made `pub(crate)` but no proptests were added since it requires the
   optional Matrix SDK dependency.

3. **Run full mutation suite** after baseline fixes to confirm kill rate.

---

## Commits

1. `2b93cb8` — `test: wire up proptest allowlist tests for all channels`
   - Wire modules, fix visibility, add 21 proptests + 5 deterministic tests

All changes are local on branch `fix/tests-proptest` (not pushed).

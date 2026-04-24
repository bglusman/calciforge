# Consolidation findings — #28 (onecli → security-proxy merge)

Working notes captured while planning the consolidation refactor. Each
finding is something surprising/non-obvious that should be addressed by
the consolidation PR (or documented as accepted).

Status: work-in-progress. Dated 2026-04-24.

---

## Finding 1: Dual env-var conventions, silently diverging

**Summary**: `security-proxy` and `onecli-client` have parallel credential
loading paths that use *different* env-var naming conventions.

- `security-proxy/src/credentials.rs` line 27:
  ```rust
  if let Some(provider) = key.strip_prefix("ZEROGATE_KEY_") {
  ```
  → loads `ZEROGATE_KEY_OPENAI`, `ZEROGATE_KEY_ANTHROPIC`, etc.

- `onecli-client/src/vault.rs` line 23:
  ```rust
  let env_var = format!("{}_API_KEY", name.to_uppercase());
  ```
  → loads `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.

**Why it matters**: a user setting `ANTHROPIC_API_KEY=sk-...` will find
that onecli sees it but security-proxy does not. If both services are
running, requests through one path succeed and requests through the
other fail with "no credentials found". Debugging time: high.

**Consolidation decision needed**:
- Settle on **one** convention. Recommend `<NAME>_API_KEY` —
  conventional, obvious to new users, already how most SDKs read their
  keys.
- During transition, accept both and warn on the legacy form. Scrub
  `ZEROGATE_KEY_*` reads after a deprecation window.

## Finding 2: Different provider-detection strategies

- `security-proxy`: matches by **destination host** (`detect_provider("api.openai.com") → "openai"`). Uses pattern matching on substrings in the Host header of the incoming request.
- `onecli-client`: matches by **URL path segment** (`/proxy/openai/v1/chat/completions` → provider = "openai"). Client must know the convention.

Both are reasonable but serve different access patterns:
- Host-based is a drop-in MITM proxy: client makes normal requests, the
  gateway figures out what to inject.
- Path-based is an explicit convenience API: client picks the provider
  name in the URL.

**Consolidation decision needed**: support both (the codebases already
do this collectively). The unified gateway should accept:
1. `/proxy/:provider/...` — explicit path-based (legacy onecli)
2. Any other URL, with destination extracted from `X-Target-URL` header
   or Host header — MITM-style (security-proxy current)

Making path-based a thin wrapper over host-based is cleanest.

## Finding 3: Auth-header scheme inconsistency

- `security-proxy`: has a per-provider match (`openai`/`openrouter`/
  `kimi`/`github` → `Authorization: Bearer`; `anthropic` → `x-api-key`;
  default → Bearer). Covers 7 providers hardcoded.
- `onecli-client`: only `Authorization: Bearer` (all providers) plus
  special-case `X-Subscription-Token` for `brave`.

**Implication**: if a request for anthropic goes through onecli-client,
it sets `Authorization: Bearer ...` which anthropic will reject (they
want `x-api-key`). The e2e tests at
`crates/zeroclawed/tests/e2e/onecli_proxy.rs` may have been working only
because no one actually tried anthropic through that path.

Let me check this claim — it's a worthwhile adversarial test.

## Finding 4: Config schema divergence

- `security-proxy` reads `agents.json` with per-agent `env_key`:
  ```json
  {"provider": "openai", "env_key": "OPENAI_API_KEY"}
  ```
  → this means agents.json controls WHICH env var name maps to WHICH
  provider. Arbitrary mapping, not auto-inferred.

- `onecli-client` has no equivalent — derives env var name from the
  provider/secret name directly.

After consolidation, we should have **one** config schema. The
agents.json model is richer (lets you override per deployment) so
extend it to be the canonical config for the unified gateway.

## Finding 5: Resolver in onecli ≠ injector in security-proxy

- `onecli-client/src/vault.rs::get_secret()` is a full resolver: env →
  fnox → vaultwarden. Returns `Result<String>`.
- `security-proxy/src/credentials.rs::CredentialInjector::load_from_env()`
  is a one-shot, startup-time env scan into a DashMap. No fnox, no
  vaultwarden, no on-demand resolution.

**Implication**: today, if you add a key to the vault AFTER
security-proxy starts, security-proxy doesn't pick it up. onecli picks
it up on next request.

Consolidation: **the resolver must be consulted per-request**, not
just at startup. DashMap can stay as a cache but must have TTL and
on-miss fall to the resolver. Otherwise rotation is broken.

## Finding 6: Hardcoded "vault.enjyn.com" in `vault.rs`

`VaultConfig::default` sets `url` to `"https://vault.enjyn.com"` when
`ONECLI_VAULT_URL` is unset. This is a **hardcoded fallback URL
revealing infrastructure**. Flagged in CLAUDE.md's "what to use
instead" section — env vars should be mandatory with no
"helpful" default.

Fix (either during consolidation or as separate cleanup): make the URL
mandatory — no default. If the env var is unset, fail closed with
"vaultwarden URL not configured; set ONECLI_VAULT_URL".

User memory notes this is planned for the rename/history-rewrite
pass. Consolidation PR can either fix forward (add no default) or
defer; recommend fixing forward since we're touching the file anyway.

## Finding 7: No actual policy enforcement in the gateway path

Before deletion in this branch, `onecli-client/src/policy.rs` was
explicitly deprecated: "fails closed". The actual enforcement lives in
`clashd`. But nothing in the current `security-proxy/src/proxy.rs`
calls out to clashd on every request either — the scanner handles
adversarial content but not policy decisions.

**Consolidation opportunity**: add a pre-request clashd policy check
in the unified gateway. Otherwise the architecture claim of
"clashd is the policy engine" is aspirational.

Track as task (don't block #28 on it); gateway should call clashd
`/check` before substituting.

## Finding 8: Substitution is a greenfield feature, not a migration

The vision in `docs/rfcs/agent-secret-gateway.md` §3 describes
`{{secret:NAME}}` substitution in URL/headers/body. **Neither
security-proxy nor onecli-client implements this today.** All current
injection is keyed off provider detection; the token syntax is a
design proposal.

This means #29 (substitution) is not "refactor the existing
substitution into the unified crate" — it's new code entirely. The
consolidation PR (#28) should create the module with a no-op
implementation so that #29 becomes a focused "fill in the body"
change.

## Finding 9 (meta): LLM-written tests need independent quality review

Own observation while writing the T4 adversarial tests in
`crates/onecli-client/tests/vault_fallthrough.rs` — LLM-generated tests
risk passing the "compiles and asserts something" bar without passing
the "actually catches regressions" bar. Examples that nearly slipped:
an assertion that `result.is_err()` without inspecting the error; a
name that checked "exact uppercase transform" which is technically an
implementation detail, not a behavior.

Adopt systematically:
- Outside-in TDD for new features (#29, #30, #31). Write a failing
  **behavior** test first. Then minimum code to pass. Refactor.
- Every test must have a name that reads like the spec it enforces.
- Every test must have an assertion against externally-observable
  behavior (return value, side effect, error variant), not just
  existence of output.
- A test that tests the mock is testing the wrong thing.
- Given/When/Then structure in doc-comment when helpful.

Task #34 (test quality review) tracks a workspace-wide audit.

## Finding 10: e2e onecli proxy test assumes a running service

`crates/zeroclawed/tests/e2e/onecli_proxy.rs` has:
```rust
async fn start_onecli() -> String {
    // ...
    // Probably returns localhost URL, tests skip if unreachable
}
```
And there's a skip message: "Skipping test: OneCLI not running on {}".
So these tests *opt out* when onecli isn't up. After the binary goes
away, these tests need to point to security-proxy (same endpoints
under the new merged routes) or be rewritten.

Not a blocker, but plan the rewrite as part of #28 to avoid shipping
dead skip-branches.

---

## Running punch-list for the #28 PR

- [ ] Delete `onecli-client/src/main.rs` (after security-proxy
      absorbs `/vault/:secret` and `/proxy/:provider`)
- [ ] Delete `security-proxy/src/credentials.rs` duplicate — replace
      with `onecli-client/src/vault.rs::get_secret` (finding 1, 5)
- [ ] Settle on `<NAME>_API_KEY` env convention; deprecate
      `ZEROGATE_KEY_*` with warning (finding 1)
- [ ] Extend anthropic/brave style per-provider auth-header formatting
      to the unified gateway (finding 3)
- [ ] Unify agents.json as the single config schema (finding 4)
- [ ] On-demand resolver call in hot path; cache with TTL (finding 5)
- [ ] Remove hardcoded `vault.enjyn.com` fallback (finding 6)
- [ ] Stub substitution module returning "no references found" so #29
      can slot in without API churn (finding 8)
- [ ] Rename crate `onecli-client` → TBD (candidate:
      `secret-proxy-client`); update workspace Cargo.toml, CI matrix,
      zeroclawed's path dep
- [ ] Update e2e tests in zeroclawed crate to hit security-proxy
      instead of onecli (finding 9)
- [ ] Update install.sh: remove onecli bin, keep fnox install, add
      security-proxy `/vault/:secret` reference to docs

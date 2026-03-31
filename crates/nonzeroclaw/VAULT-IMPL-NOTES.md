# Vault Implementation Notes

_Written: 2026-03-30_
_Crate: `nonzeroclaw` (module `src/vault/`)_

---

## What Was Implemented

### Core Traits and Types (`vault/adapter.rs`, `vault/types.rs`, `vault/error.rs`)

- **`VaultAdapter` trait** — `async fn get_secret`, `store_secret`, `unlock`; object-safe via `async_trait`
- **`NoopVaultAdapter`** — always returns `VaultError::NotConfigured`; used when `backend = "none"`
- **`Secret`**, **`SecretValue`**, **`SessionToken`** — opaque value types with `Debug` redaction and best-effort Drop zeroing
- **`SecretPolicy` enum** — `Auto`, `PerUse`, `Session`, `TimeBound { ttl: Duration }`
- **`VaultError` enum** — `thiserror`-derived; covers all failure modes

### Approval Relay (`vault/approval.rs`)

- **`ApprovalRelay` trait** — `async fn request_approval(&self, key, context) -> Result<ApprovalDecision>`
- **`ApprovalDecision` enum** — `Approved`, `Denied`, `TimedOut`
- **`NoopApprovalRelay`** — always returns `Approved` immediately; used for `Auto` policy
- **`ChannelApprovalRelay`** — takes an `Arc<dyn Fn(key, context, oneshot::Sender<ApprovalDecision>) -> Pin<Box<...>>>` callback; uses `tokio::time::timeout` with configurable TTL; callback drops = `TimedOut`; actual Signal/Telegram wiring is **not** here — it plugs in via the callback

### Config Schema (`vault/config.rs`)

- **`VaultBackend` enum** — `None`, `BitwardenCli` (serialized as `"none"` / `"bitwarden-cli"`)
- **`VaultConfig` struct** — `backend`, `bw_path`, `session_ttl_secs`, `secrets: HashMap<String, VaultSecretConfig>`
- **`VaultSecretConfig`** — `bw_item_id`, `policy: SecretPolicyConfig`, `ttl_secs: Option<u64>`
- **`SecretPolicyConfig` enum** — TOML-serializable mirror of `SecretPolicy`
- **`VaultSecretConfig::to_runtime_policy()`** — converts config policy to runtime `SecretPolicy`
- `VaultConfig` wired into top-level `Config` struct as `pub vault: VaultConfig` (`#[serde(default)]`)

### Vault Manager (`vault/manager.rs`)

- **`VaultManager`** — combines `Arc<dyn VaultAdapter>` + `Arc<dyn ApprovalRelay>` + `Arc<VaultConfig>`
- **`access_secret(key)`** — policy dispatch: `Auto` → direct fetch; `PerUse` → relay every time; `Session` → relay once, cache; `TimeBound` → relay once, cache with expiry
- **`access_secret_with_context(key, context)`** — same but forwards rich context string to relay
- **`invalidate_approval(key)`** and **`clear_approval_cache()`** — for session end / explicit revocation
- **`from_config(VaultConfig)`** — convenience constructor (uses `NoopApprovalRelay`; swap relay for prod)
- Approval cache: `tokio::sync::Mutex<HashMap<String, CachedApproval>>`; TTL-aware validity check

### BitwardenCliAdapter (`vault/bitwarden.rs`, feature-gated `bitwarden-cli`)

- **`BwRunner` trait** — abstracts `bw` subprocess for testability: `unlock`, `get_password`, `create_item`
- **`ProcessBwRunner`** — real implementation; `tokio::process::Command`; handles `NotFound` vs generic I/O; `create_item` writes JSON to stdin
- **`BitwardenCliAdapter<R: BwRunner>`** — generic over runner; session token cached in `Arc<Mutex<Option<SessionToken>>>`; auto-re-unlocks on TTL expiry; master password sourced from `BW_MASTER_PASSWORD` env var at `from_config()` time (never logged)
- **`login_item_json()`** — minimal Bitwarden login item JSON for `bw create item`

### Files Changed (outside `vault/`)

| File | Change |
|------|--------|
| `src/lib.rs` | Added `pub mod vault;` |
| `src/main.rs` | Added `mod vault;` |
| `src/config/schema.rs` | Added `pub vault: crate::vault::VaultConfig` to `Config` struct; updated all 3 `Default` impls |
| `src/onboard/wizard.rs` | Added `vault: crate::vault::VaultConfig::default()` to both `Config { ... }` struct literals |
| `Cargo.toml` | Added `bitwarden-cli = []` feature |

---

## What Was Skipped / Stubbed

### `ChannelApprovalRelay` actual channel wiring
The `ChannelApprovalRelay` callback interface is complete and correct — it accepts a `tokio::oneshot::Sender<ApprovalDecision>` and can be wired to any channel. The actual Signal/Telegram message delivery and `/approve` command parsing is **not implemented** — it belongs in the channel layer (`channels/signal.rs`, `channels/telegram.rs`) in a future PR.

### `bw` binary not-found / BW_MASTER_PASSWORD UX
`from_config()` reads `BW_MASTER_PASSWORD` from env at construction time. If the env var is absent, `unlock()` returns `UnlockFailed`. There is no UI prompt, no keyring integration, no interactive unlock flow. Future work: integrate with `dialoguer` for interactive unlock or pull from the existing `SecretsConfig` encrypted store.

### `store_secret` item lookup / upsert
`store_secret` always creates a new Bitwarden item. It does not check for existing items with the same name or update them. `bw edit item` / `bw delete item` are not implemented. Upsert logic belongs in the next PR.

### `zeroize` crate
`Secret` and `SecretValue` have best-effort Drop zeroing, but the compiler may copy the bytes. Proper cryptographic zeroing requires the `zeroize` crate (it was not added to keep deps minimal for Phase 1).

### `EnvAdapter`
The planning doc mentions an `EnvAdapter` (reads from env vars, no approval flow). Not implemented — the `NoopVaultAdapter` fills that role for now; a real env adapter would be ~30 lines.

### Multi-approver / batch approval
The design doc discusses batch approvals and multi-approver. Not implemented — the interface supports single-operator approval per key; batch is a UI concern.

### Approval persistence across restarts
The approval cache is in-memory only. A restart clears all cached session/time-bound approvals. The design doc notes JSONL/DB persistence as future work.

### `cargo doc` doc-tests
Doc examples in comments use `ignore` markers — they're not runnable without a real `bw` binary or mock wiring. Integration tests (not unit tests) are the right vehicle for end-to-end doc examples.

---

## What the Next Implementation PR Should Tackle

1. **Channel approval wiring** — implement `ChannelApprovalRelay` callback in `channels/signal.rs` and `channels/telegram.rs`; add `/approve <id>` and `/deny <id>` command parsing; send formatted approval request messages with inline buttons (Telegram) or text (Signal)

2. **Approval ID tracking** — add `ApprovalId` to `ApprovalRequest` so the channel handler can route responses to the right pending request; needed for concurrent pending approvals

3. **`VaultManager` integration into `agent` turn loop** — call `vault_manager.access_secret_with_context(key, context)` in the tool execution path when a tool declares a vault dependency in its schema

4. **`EnvAdapter`** — reads `<KEY_UPPER>` env var directly; policy always `Auto`; useful for dev/CI

5. **`bw` unlock interactive prompt** — if `BW_MASTER_PASSWORD` is unset, trigger a channel message to the operator asking for the master password (via approval relay), receive it once, zero it after use

6. **Approval cache persistence** — write approved decisions to a JSONL file so session-scoped approvals survive a restart within the same logical session

7. **`bw` item upsert** — `store_secret` should check for existing items (`bw list items --search <name>`) and use `bw edit item` if found

8. **`zeroize` integration** — add `zeroize` crate, derive `Zeroize` on `Secret` / `SecretValue` / `SessionToken`

---

## Testing Summary

| Test file | Tests | Feature |
|-----------|-------|---------|
| `vault/config.rs` | 3 | (default) |
| `vault/manager.rs` | 10 | (default) |
| `vault/bitwarden.rs` | 6 | `bitwarden-cli` |
| **Total** | **19** | |

All 19 tests pass with `cargo test -p nonzeroclaw --features bitwarden-cli vault`.
All 13 non-feature tests pass with `cargo test -p nonzeroclaw vault` (default features).
Full `cargo check -p nonzeroclaw` clean (warnings only, no errors).

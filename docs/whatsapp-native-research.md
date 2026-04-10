# ZeroClawed-Native WhatsApp Research

**Date:** 2026-04-10
**Status:** Research complete — implementation planned

## Current Architecture

ZeroClawed's WhatsApp channel is currently a **webhook receiver + identity router + reply sender**:

```
WA user → NZC (wa-rs session) → POST /webhooks/whatsapp → ZeroClawed
                                                           │
                              identity resolution          │
                              agent dispatch               │
                                                           ↓
WA user ← NZC (wa-rs session) ← POST /tools/invoke ← ZeroClawed reply
```

The actual WhatsApp protocol handling (QR pairing, WA Web encryption, send/receive at the wire level) lives in NonZeroClaw's `whatsapp-web` feature. ZeroClawed only handles routing.

## Proposed Native Implementation

Replace the sidecar with direct WhatsApp Web protocol implementation using `whatsapp-rust`.

### whatsapp-rust Crate Analysis

**Package:** `whatsapp-rust` v0.5.0  
**License:** MIT  
**Repository:** https://github.com/jlucaso1/whatsapp-rust

**Key Features:**
- Async Rust library for WhatsApp Web API
- Inspired by `whatsmeow` (Go) and `Baileys` (TypeScript)
- Comprehensive feature set:
  - `Client` — Main client struct for WhatsApp session
  - `message` — Message handling (send/receive)
  - `groups` — Group management
  - `media` — Media upload/download
  - `presence` — Presence/online status
  - `pair` / `pair_code` — QR and pairing code authentication
  - `store` — Pluggable storage (SQLite via `whatsapp-rust-sqlite-storage`)

**Dependencies:**
- `tokio` — Async runtime (already in zeroclawed)
- `serde` / `serde_json` — Serialization (already in zeroclawed)
- `waproto` — WhatsApp protocol buffers
- `wacore` / `wacore-binary` — Core WhatsApp crypto
- Optional: `moka` for caching, `whatsapp-rust-tokio-transport` for transport

**API Example (from docs):**
```rust
use whatsapp_rust::Client;

// Create client with storage
let client = Client::builder()
    .store(my_store)
    .build();

// Connect and authenticate (QR pairing)
client.connect().await?;

// Send message
client.send_text(jid, "Hello from ZeroClawed!").await?;
```

### Implementation Plan

#### Phase 1: Dependency Integration
- [ ] Add `whatsapp-rust` to `crates/zeroclawed/Cargo.toml`
- [ ] Add feature flag `native-whatsapp` to conditionally compile
- [ ] Resolve any dependency conflicts

#### Phase 2: Native Channel Implementation
- [ ] Create `whatsapp_native.rs` channel adapter
- [ ] Implement QR code pairing flow
- [ ] Implement credential storage (reuse existing paths)
- [ ] Map `whatsapp-rust` events to ZeroClawed message types

#### Phase 3: Feature Parity
- [ ] Text message send/receive
- [ ] Media message handling (images, audio, documents)
- [ ] Group chat support
- [ ] Read receipts
- [ ] Message reactions
- [ ] Reply/quote context

#### Phase 4: Migration & Testing
- [ ] Config toggle: `native = true` in channel config
- [ ] Side-by-side testing with webhook implementation
- [ ] Deprecate webhook receiver after validation

### Configuration Changes

```toml
[[channels]]
kind = "whatsapp"
enabled = true

# New: Native mode (default false for backward compat)
native = true

# Existing webhook config (used when native = false)
# nzc_endpoint = "http://127.0.0.1:18789"
# nzc_auth_token = "..."

# New: Native config (used when native = true)
store_path = "/var/lib/zeroclawed/whatsapp/session.db"
pairing_timeout_seconds = 120

# Common config
allowed_numbers = ["+15555550001"]
webhook_listen = "0.0.0.0:18795"  # Can reuse for health/compat
```

### Risks & Considerations

1. **Stability:** `whatsapp-rust` is relatively new (v0.5.0). The maintainer also maintains Baileys, which suggests expertise but also shared maintenance burden.

2. **WhatsApp TOS:** Using unofficial WhatsApp clients carries account ban risk. The current webhook approach proxies through OpenClaw which may have different risk characteristics.

3. **Session Migration:** Existing NZC-linked sessions cannot be migrated. Users will need to re-pair.

4. **Dependency Chain:** `whatsapp-rust` brings in protobuf, crypto, and storage crates. Need to audit for supply chain security.

### Next Steps

1. **Spike:** Create minimal prototype using `whatsapp-rust` to verify:
   - Compilation with zeroclawed's existing deps
   - QR pairing works
   - Message send/receive works

2. **Decision:** Based on spike results, decide on full implementation vs. continued webhook approach.

3. **If proceeding:** Create feature branch, implement Phase 1-2, test on staging.

## References

- [whatsapp-rust on crates.io](https://crates.io/crates/whatsapp-rust)
- [whatsapp-rust docs](https://docs.rs/whatsapp-rust/0.5.0/whatsapp_rust/)
- [GitHub repository](https://github.com/jlucaso1/whatsapp-rust)
- Current implementation: `crates/zeroclawed/src/channels/whatsapp.rs`

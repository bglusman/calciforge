# Lucien Setup Notes — 2026-03-15

Completed by Librarian subagent. All 5 parts done.

---

## Part 1: mxbai-embed-large on inference VM (.125) ✅

- **Model pulled:** `mxbai-embed-large:latest` (669 MB, 334M params, F16 GGUF)
- **Embedding dims:** 1024 ✅
- **Ollama bind:** Already configured `OLLAMA_HOST=0.0.0.0` — no changes needed
- **Network reachability from .210:** ✅ Confirmed `http://10.0.0.30:11434` is accessible

---

## Part 2: NonZeroClaw config updated (.210) ✅

Config at `/etc/nonzeroclaw/config.toml` updated with SQLite memory backend:

```toml
[memory]
backend = "sqlite"
auto_save = true
store_path = "/etc/nonzeroclaw/memory.db"
embedding_provider = "custom:http://10.0.0.30:11434/v1"
embedding_model = "mxbai-embed-large"
embedding_dimensions = 1024
vector_weight = 0.7
keyword_weight = 0.3
min_relevance_score = 0.4
conversation_retention_days = 90
```

**Note:** Provider kept as `custom:http://10.0.0.20:18801/v1` (Librarian's kimi-proxy) since
.210 has no local kimi-proxy. Task spec said 127.0.0.1:18801 but that would break things — this
is the correct working endpoint.

Service restarted and running cleanly. No errors in logs.

**Note on memory.db:** The SQLite file is lazy-initialized — it will appear at
`/etc/nonzeroclaw/memory.db` after the first conversation is auto-saved. This is normal behavior.

---

## Part 3: Brain transplant ✅

Copied from Librarian (.229) to Lucien's workspace (`/etc/nonzeroclaw/workspace/`):
- `MEMORY.md` ✅
- `TOOLS.md` ✅
- `USER.md` ✅
- `memory/` directory (all daily files) ✅
- `AGENTS.md` NOT copied (Lucien has his own identity — see Part 4)

**Verification:** Asked Lucien about Brian's family:
> "Brian has two kids: Simon (born 2021) and Sylvia (born 2022). His partner is Renee — she's an attorney at CAMBA in Brooklyn, previously with Legal Aid Society."

Brain transplant successful. ✅

---

## Part 4: Lucien identity (AGENTS.md on .210) ✅

Written to `/etc/nonzeroclaw/workspace/AGENTS.md`.

Identity block:
- Name: Lucien — The Dream Librarian (📚)
- Location: CT 1200 at 10.0.0.10, co-resident with PolyClaw
- Peer: Librarian at 10.0.0.20
- Runtime: NonZeroClaw 0.1.0
- Memory: SQLite + mxbai-embed-large (1024 dims)
- Channels: Matrix (coming soon), Telegram via PolyClaw

**Verification:** Asked Lucien who he is:
> "I'm **Lucien** — the Dream Librarian. I run on **CT 1200 at 10.0.0.10**, co-resident with PolyClaw. My peer Librarian lives at 10.0.0.20. I'm built on **NonZeroClaw**..."

Identity loaded correctly. ✅

All TATTOOS from Librarian's AGENTS.md are included in Lucien's AGENTS.md — they apply universally.

---

## Part 5: PolyClaw Matrix channel scaffold ✅

Files modified/created:
- `crates/polyclaw/src/config.rs` — Added Matrix fields to `ChannelConfig`
  - `homeserver: Option<String>`
  - `access_token_file: Option<String>`
  - `room_id: Option<String>`
  - `allowed_users: Vec<String>`
- `crates/polyclaw/src/channels/mod.rs` — Added `pub mod matrix;`
- `crates/polyclaw/src/channels/matrix.rs` — Full stub with TODOs, run() and send_message()
- `MATRIX-SETUP-NEEDED.md` — Manual steps for Brian

**Compile check:** `cargo check --package polyclaw` → clean (23 pre-existing warnings, 0 errors) ✅

---

## Manual Steps Still Required

### Matrix bot account (Brian must do)

1. Register `@lucien:matrix.org` (or similar) at https://app.element.io/#/register
2. Get access token via login API
3. Save token to `~/.polyclaw/secrets/matrix-token` on .210 (where PolyClaw runs)
4. Tell us Brian's Matrix ID (e.g. `@brian:matrix.org`)
5. Create a private E2EE room, invite the bot, get the room ID
6. Update `~/.polyclaw/config.toml` with the Matrix channel config (see MATRIX-SETUP-NEEDED.md)

### To fully activate Matrix in PolyClaw code

1. Add `matrix-sdk` to `crates/polyclaw/Cargo.toml`
2. Implement the TODOs in `crates/polyclaw/src/channels/matrix.rs`
3. Wire `channels::matrix::run()` into `main.rs`

Full instructions: `MATRIX-SETUP-NEEDED.md`

---

## Security Note

`/etc/nonzeroclaw/config.toml` has mode 644 (world-readable). NonZeroClaw warns about this
on startup. It contains an API key. Brian should run:
```bash
ssh root@10.0.0.10 "chmod 600 /etc/nonzeroclaw/config.toml"
```

---

## Summary

| Part | Status |
|------|--------|
| mxbai-embed-large on .125 | ✅ Done (1024 dims, network-accessible) |
| NonZeroClaw SQLite memory config | ✅ Done (pending first conversation to create .db) |
| Brain transplant | ✅ Done (MEMORY, TOOLS, USER, memory/ copied) |
| Lucien identity (AGENTS.md) | ✅ Done (identifies correctly as Lucien) |
| PolyClaw Matrix scaffold | ✅ Done (compiles, needs bot account) |
| Matrix bot account | ⏳ Needs Brian to create manually |

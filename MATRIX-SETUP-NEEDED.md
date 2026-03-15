# Matrix Bot Setup — Manual Steps Required

PolyClaw has a Matrix channel scaffold (`crates/polyclaw/src/channels/matrix.rs`) ready to
activate once a bot account exists and `matrix-sdk` is wired in.

## What's needed

### 1. Matrix bot account
Create one of the following:
- `@lucien:matrix.org` — dedicated Lucien identity (preferred)
- Or reuse `@librarian-assistant:matrix.org` if one already exists

Registration: https://app.element.io/#/register (matrix.org homeserver)

### 2. Bot access token
After registering, log in and retrieve the access token:
```bash
curl -XPOST 'https://matrix.org/_matrix/client/v3/login' \
  -H 'Content-Type: application/json' \
  -d '{
    "type": "m.login.password",
    "user": "@lucien:matrix.org",
    "password": "YOUR_PASSWORD_HERE"
  }'
```
Save the `access_token` from the response to:
```
~/.polyclaw/secrets/matrix-token
```

### 3. Brian's Matrix ID
Brian needs to tell us his Matrix ID (format: `@username:server.org`).
This goes in `allowed_users` in `~/.polyclaw/config.toml`.

### 4. Create and share a private E2EE room
1. Brian creates a new private room in his Matrix client (Element/Fluffychat/etc.)
2. Enable E2EE for the room
3. Invite `@lucien:matrix.org` (or whatever the bot account is)
4. Copy the Room ID (format: `!abc123xyz:matrix.org`) — found in room settings

### 5. Update PolyClaw config
Add to `~/.polyclaw/config.toml`:

```toml
[[channels]]
kind = "matrix"
homeserver = "https://matrix.org"
access_token_file = "~/.polyclaw/secrets/matrix-token"
room_id = "!ROOM_ID_HERE:matrix.org"
allowed_users = ["@brian:matrix.org"]  # Brian's Matrix ID
enabled = true
```

### 6. Add matrix-sdk dependency
In `crates/polyclaw/Cargo.toml`, add:
```toml
matrix-sdk = { version = "0.7", features = ["e2e-encryption", "rustls-tls"], default-features = false }
```
Then implement the TODOs in `crates/polyclaw/src/channels/matrix.rs`.

### 7. Wire matrix::run() into main.rs
In `crates/polyclaw/src/main.rs`, add the Matrix channel alongside Telegram:
```rust
// Spawn Matrix channel if enabled
if config.channels.iter().any(|c| c.kind == "matrix" && c.enabled) {
    tokio::spawn(channels::matrix::run(config.clone(), router.clone(), cmd_handler.clone(), ctx_store.clone()));
}
```

## Routing for Lucien via Matrix

PolyClaw routes Matrix messages to the agent configured for the sender's identity.
Lucien (NonZeroClaw at 10.0.0.10:18793) is the intended Matrix agent.

In `~/.polyclaw/config.toml`, add/update:
```toml
[[agents]]
id = "lucien"
kind = "zeroclaw"
endpoint = "http://10.0.0.10:18793"
api_key = "sk-example-replace-with-your-key"
timeout_ms = 120000
aliases = ["nzc", "dream"]

[[routing]]
identity = "brian"        # or whatever identity maps to Brian's Matrix ID
default_agent = "lucien"  # route Matrix messages to Lucien
```

## E2EE Note

For E2EE rooms, `matrix-sdk` requires key verification. The bot should support
interactive key verification or use `trust_settings` for cross-signing.
Consider using `matrix-sdk`'s `verification` feature.

---

*Scaffolded by Librarian subagent on 2026-03-14. Contact Brian via Telegram/Signal to complete.*

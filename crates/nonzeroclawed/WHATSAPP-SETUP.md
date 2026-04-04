# WhatsApp Channel — Setup Guide

## Architecture

```
WA user  ──→  NZC (wa-rs session on .210)  ──→  POST /webhooks/whatsapp  ──→  NonZeroClawed
                                                                                   │
                                                  identity resolution              │
                                                  agent dispatch (Librarian/NZC)   │
                                                                                   ↓
WA user  ←──  NZC (wa-rs session on .210)  ←──  POST /tools/invoke  ←──  NonZeroClawed reply
```

## Step 1: NonZeroClawed config (`/root/.nonzeroclawed/config.toml` on 10.0.0.10)

Add this `[[channels]]` block:

```toml
[[channels]]
kind          = "whatsapp"
enabled       = true

# NZC / OpenClaw gateway that owns the WhatsApp Web session.
# NonZeroClawed sends replies by POSTing to {nzc_endpoint}/tools/invoke.
# If NonZeroClawed is co-located with OpenClaw on .210, use 127.0.0.1.
# If NonZeroClawed is on .229 (Librarian), point to .210 where NZC runs.
nzc_endpoint  = "http://127.0.0.1:18789"
nzc_auth_token = "REPLACE_WITH_AUTH_TOKEN"

# NonZeroClawed's own webhook listener — NZC will POST incoming WA messages here.
# Must be reachable from wherever NZC is running.
webhook_listen = "0.0.0.0:18795"
webhook_path   = "/webhooks/whatsapp"

# Optional HMAC secret for X-Hub-Signature-256 header verification.
# Set the same value in NZC config as its webhook_forward_secret.
# webhook_secret = "change-me-to-a-random-secret"

# Allowed sender phone numbers (E.164).
# Must match identity aliases below.
allowed_numbers = ["+15555550001", "+14085551234"]
```

## Step 2: Identity aliases

For each allowed WA number, add a `whatsapp` alias to the `[[identities]]` block:

```toml
[[identities]]
id           = "brian"
display_name = "Brian"
role         = "owner"

[[identities.aliases]]
channel = "telegram"
id      = "8465871195"

[[identities.aliases]]
channel = "whatsapp"
id      = "+15555550001"   # E.164 format
```

## Step 3: NZC forwarding config (on the NZC instance at .210)

NZC needs to forward incoming WA messages to NonZeroClawed instead of processing them
locally. Add to NZC's `zeroclaw.toml`:

```toml
[channels_config.whatsapp]
# session_path for WhatsApp Web (wa-rs)
session_path = "~/.zeroclaw/whatsapp-session.db"

# Forward incoming messages to NonZeroClawed's webhook
webhook_forward_url    = "http://127.0.0.1:18795/webhooks/whatsapp"
# webhook_forward_secret = "change-me-to-a-random-secret"  # Must match NonZeroClawed's webhook_secret

allowed_numbers = ["+15555550001", "+14085551234"]
```

## Step 4: QR pairing

Once NZC is configured with `session_path`, start NZC — it will print a QR code.
Scan it from WhatsApp on the phone. The session is persisted to the SQLite DB.
After that, incoming messages flow:

```
WA  →  NZC (wa-rs receives)  →  NZC forwards to NonZeroClawed webhook
                                        ↓
                               NonZeroClawed routes → Agent
                                        ↓
                               NonZeroClawed POSTs reply to NZC /tools/invoke
                                        ↓
                                NZC sends WA reply
```

## Step 5: Firewall

Ensure port 18795 is open on the NonZeroClawed host (or at least reachable from NZC).

If NZC and NonZeroClawed are on the same host:
```
# No firewall changes needed — both use localhost
```

If NZC is on .210 and NonZeroClawed is on .229:
```bash
ufw allow from 10.0.0.10 to any port 18795
```

## Webhook payload format

NonZeroClawed expects the standard WhatsApp Cloud API format:

```json
{
  "object": "whatsapp_business_account",
  "entry": [{
    "changes": [{
      "value": {
        "messages": [{
          "from": "15555550001",
          "type": "text",
          "text": { "body": "Hello!" },
          "timestamp": "1699999999"
        }]
      }
    }]
  }]
}
```

The `from` field may omit the leading `+` — NonZeroClawed normalises to E.164 format
before identity lookup.

## Reply API

NonZeroClawed sends replies by POSTing to `{nzc_endpoint}/tools/invoke`:

```json
{
  "tool": "message",
  "args": {
    "action": "send",
    "channel": "whatsapp",
    "target": "+15555550001",
    "message": "Agent reply text here"
  }
}
```

This uses OpenClaw's built-in `message` tool — the same mechanism agents use
internally. NZC must have a live WA Web session for this to succeed.

## Health check

```bash
# Check NonZeroClawed webhook listener is up
curl http://localhost:18795/health

# Test with a synthetic webhook (should return 200 + "ok")
curl -X POST http://localhost:18795/webhooks/whatsapp \
  -H "Content-Type: application/json" \
  -d '{"object":"whatsapp_business_account","entry":[{"changes":[{"value":{"messages":[{"from":"15555550001","type":"text","text":{"body":"test"},"timestamp":"1699999999"}]}}]}]}'
```

## TODO: HMAC verification

The `verify_hmac_sha256` function in `channels/whatsapp.rs` is currently a
placeholder that accepts any non-empty signature header. To enable real HMAC-SHA256
verification, add these crates to `Cargo.toml`:

```toml
hmac = "0.12"
sha2 = "0.10"
```

Then replace the placeholder in `verify_hmac_sha256` with:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn verify_hmac_sha256(secret: &str, body: &[u8], sig_header: &str) -> bool {
    let expected_hex = match sig_header.strip_prefix("sha256=") {
        Some(h) => h,
        None => return false,
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(body);
    let computed = mac.finalize().into_bytes();
    let computed_hex = hex::encode(computed);
    // Constant-time comparison
    computed_hex == expected_hex
}
```

Also add `hex = "0.4"` to Cargo.toml.

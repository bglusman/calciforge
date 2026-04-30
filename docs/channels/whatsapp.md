---
layout: default
title: WhatsApp Channel Setup
---

# WhatsApp Channel

Calciforge's WhatsApp channel is a **webhook receiver**. It accepts incoming
messages from any compatible WhatsApp gateway and sends replies back through the
gateway's outbound API. Calciforge does not own the WhatsApp Web session itself.

The contract is the wire format, not a specific product:

- **Inbound:** the gateway POSTs to `/webhooks/whatsapp` in the WhatsApp Cloud API
  webhook payload format (see [Webhook payload format](#webhook-payload-format) below).
- **Outbound:** Calciforge POSTs replies to `{gateway}/tools/invoke` with the body
  shape documented under [Reply API](#reply-api).

Any gateway that implements those two endpoints will work. The known-working
implementation is [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw)'s
`whatsapp-web` feature, which is what this guide configures. If you're running
a different gateway, point Calciforge at it and adjust the auth token accordingly.

> **Future work:** embedding the WhatsApp protocol directly into Calciforge (so
> no external gateway is required) is tracked in the project backlog. It would
> use `zeroclawlabs::WhatsAppWebChannel` (which wraps `whatsmeow-rs`) as a Rust
> library — distinct from running ZeroClaw as a separate daemon.

## Architecture

```
WA user  ──→  WhatsApp gateway (e.g. ZeroClaw)  ──→  POST /webhooks/whatsapp  ──→  Calciforge
                                                                                          │
                                                              identity resolution         │
                                                              agent dispatch              │
                                                                                          ↓
WA user  ←──  WhatsApp gateway (e.g. ZeroClaw)  ←──  POST /tools/invoke  ←──  Calciforge reply
```

## Prerequisites

- A WhatsApp gateway that implements the wire protocol described above and has
  an active WhatsApp Web session. ZeroClaw with `whatsapp-web` enabled is the
  reference implementation; any compatible alternative is fine.

## Step 1: Channel config

Add to `~/.calciforge/config.toml`:

```toml
[[channels]]
kind = "whatsapp"
enabled = true

# ZeroClaw gateway that owns the WhatsApp Web session.
# Calciforge sends replies by POSTing to {zeroclaw_endpoint}/tools/invoke.
# Use 127.0.0.1 if co-located; use the host IP if running on a separate machine.
zeroclaw_endpoint = "http://127.0.0.1:18789"
zeroclaw_auth_token = "REPLACE_WITH_AUTH_TOKEN"

# Calciforge's webhook listener — ZeroClaw will POST incoming WA messages here.
# Must be reachable from wherever ZeroClaw is running.
webhook_listen = "0.0.0.0:18795"
webhook_path = "/webhooks/whatsapp"

# Optional HMAC-SHA256 secret for X-Hub-Signature-256 header verification.
# Set the same value in ZeroClaw as its webhook_forward_secret.
# webhook_secret = "change-me-to-a-random-secret"

# Allowed sender phone numbers in E.164 format.
# Must match identity aliases below.
allowed_numbers = ["+15555550001"]
```

| Field | Required | Default | Description |
|---|---|---|---|
| `zeroclaw_endpoint` | yes | — | URL of the ZeroClaw gateway |
| `zeroclaw_auth_token` | yes | — | Bearer token for the gateway |
| `webhook_listen` | no | `0.0.0.0:18795` | Address Calciforge listens on for incoming WhatsApp webhooks |
| `webhook_path` | no | `/webhooks/whatsapp` | URL path for incoming webhooks |
| `webhook_secret` | no | — | HMAC-SHA256 secret; when set, Calciforge rejects requests with invalid or missing `X-Hub-Signature-256` headers |
| `allowed_numbers` | yes | `[]` | E.164 phone numbers allowed to interact |
| `scan_messages` | no | `false` | Enable inbound adversarial content scanning |

## Step 2: ZeroClaw forwarding config

In ZeroClaw's config, point WhatsApp message delivery at Calciforge's webhook. Also
configure the QR-linked session path:

```toml
[channels_config.whatsapp]
session_path = "~/.zeroclaw/whatsapp-session.db"
webhook_forward_url    = "http://127.0.0.1:18795/webhooks/whatsapp"
# webhook_forward_secret = "change-me-to-a-random-secret"  # must match Calciforge's webhook_secret
allowed_numbers = ["+15555550001"]
```

Start ZeroClaw — it prints a QR code. Scan from WhatsApp on your phone to pair the session.
After pairing, the session persists to the SQLite DB and survives restarts.

## Step 3: Identity config

The alias `id` is the E.164 phone number. The leading `+` is required:

```toml
[[identities]]
id = "operator"
display_name = "Alice"
role = "admin"
aliases = [
    { channel = "whatsapp", id = "+15555550001" },
]

[[routing]]
identity = "operator"
default_agent = "librarian"
allowed_agents = ["librarian"]
```

Phone numbers from `allowed_numbers` that don't match any identity alias are silently
dropped. Calciforge normalises the `from` field to E.164 before lookup.

## Step 4: Firewall

If ZeroClaw and Calciforge are on the same host, no changes needed — both use localhost.

If they're on separate hosts, open port 18795 on the Calciforge host from the ZeroClaw host:

```bash
ufw allow from <zeroclaw-host-ip> to any port 18795
```

## Step 5: Verify

```bash
calciforge doctor   # validates config
calciforge          # start; send a WhatsApp message from an allowed number
```

Health check the webhook listener and test with a synthetic payload:

```bash
curl http://localhost:18795/health

curl -X POST http://localhost:18795/webhooks/whatsapp \
  -H "Content-Type: application/json" \
  -d '{
    "object": "whatsapp_business_account",
    "entry": [{
      "changes": [{
        "value": {
          "messages": [{
            "from": "15555550001",
            "type": "text",
            "text": { "body": "test" },
            "timestamp": "1699999999"
          }]
        }
      }]
    }]
  }'
```

A `200 ok` response means the webhook is reachable. The message will be dropped (unknown
identity) unless `15555550001` is in an identity alias.

## Webhook payload format

Calciforge accepts the standard WhatsApp Cloud API format. The `from` field may omit the
leading `+` — Calciforge normalises to E.164 before identity lookup.

## Reply API

Calciforge sends replies by POSTing to `{zeroclaw_endpoint}/tools/invoke`:

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

ZeroClaw must have a live WhatsApp Web session for the reply to reach the user.

## HMAC verification

When `webhook_secret` is set, Calciforge verifies the `X-Hub-Signature-256` header on
every incoming request using HMAC-SHA256. Requests with a missing or invalid signature
are rejected with HTTP 401. Set the same secret in ZeroClaw as `webhook_forward_secret`
to keep the two sides in sync.

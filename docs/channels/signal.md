---
layout: default
title: Signal Channel Setup
---

# Signal Channel

Calciforge receives Signal messages via a **webhook** posted by a running
[ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) or OpenClaw instance that owns
the Signal session. Replies are sent back through the same gateway.

## Architecture

```
Signal user  ──→  ZeroClaw (Signal session host)  ──→  POST /webhooks/signal  ──→  Calciforge
                                                                                          │
                                                              identity resolution          │
                                                              agent dispatch               │
                                                                                          ↓
Signal user  ←──  ZeroClaw (Signal session host)  ←──  POST /tools/invoke  ←──  Calciforge reply
```

## Prerequisites

- A running ZeroClaw or OpenClaw instance with an active Signal session and its auth token

## Step 1: Channel config

Add to `~/.calciforge/config.toml`:

```toml
[[channels]]
kind = "signal"
enabled = true

# ZeroClaw / OpenClaw gateway that owns the Signal session.
# Calciforge sends replies by POSTing to {zeroclaw_endpoint}/tools/invoke.
# Use 127.0.0.1 if co-located; use the host IP if running on a separate machine.
zeroclaw_endpoint = "http://127.0.0.1:18789"
zeroclaw_auth_token = "REPLACE_WITH_AUTH_TOKEN"

# Calciforge's webhook listener — ZeroClaw will POST incoming Signal messages here.
# Must be reachable from wherever ZeroClaw is running.
webhook_listen = "0.0.0.0:18796"
webhook_path = "/webhooks/signal"

# Optional HMAC-SHA256 secret for X-Hub-Signature-256 header verification.
# Set the same value in ZeroClaw as its webhook_forward_secret.
# webhook_secret = "change-me-to-a-random-secret"

# Allowed sender phone numbers in E.164 format.
# Must match identity aliases below.
allowed_numbers = ["+15555550001"]
```

| Field | Required | Default | Description |
|---|---|---|---|
| `zeroclaw_endpoint` | yes | — | URL of the ZeroClaw/OpenClaw gateway |
| `zeroclaw_auth_token` | yes | — | Bearer token for the gateway |
| `webhook_listen` | no | `0.0.0.0:18796` | Address Calciforge listens on for incoming Signal webhooks |
| `webhook_path` | no | `/webhooks/signal` | URL path for incoming webhooks |
| `webhook_secret` | no | — | HMAC-SHA256 secret; when set, Calciforge rejects unsigned requests |
| `allowed_numbers` | yes | `[]` | E.164 phone numbers allowed to interact |
| `scan_messages` | no | `false` | Enable inbound adversarial content scanning |

## Step 2: ZeroClaw forwarding config

In ZeroClaw's config, point Signal message delivery at Calciforge's webhook:

```toml
[channels_config.signal]
webhook_forward_url    = "http://127.0.0.1:18796/webhooks/signal"
# webhook_forward_secret = "change-me-to-a-random-secret"  # must match Calciforge's webhook_secret
allowed_numbers = ["+15555550001"]
```

## Step 3: Identity config

The alias `id` is the E.164 phone number. The leading `+` is required:

```toml
[[identities]]
id = "operator"
display_name = "Alice"
role = "admin"
aliases = [
    { channel = "signal", id = "+15555550001" },
]

[[routing]]
identity = "operator"
default_agent = "librarian"
allowed_agents = ["librarian"]
```

Phone numbers from `allowed_numbers` that don't match any identity alias are silently
dropped. Calciforge normalises the `from` field to E.164 before lookup (adds `+` prefix
if absent, strips spaces and dashes).

## Step 4: Firewall

If ZeroClaw and Calciforge are on the same host, no changes needed — both use localhost.

If they're on separate hosts, open port 18796 on the Calciforge host from the ZeroClaw host:

```bash
ufw allow from <zeroclaw-host-ip> to any port 18796
```

## Step 5: Verify

```bash
calciforge doctor   # validates config
calciforge          # start; send a Signal message from an allowed number
```

Check logs for `identity resolved` on a known number, or `no identity for signal/<number>`
on an unknown one. Run a health check against the webhook listener:

```bash
curl http://localhost:18796/health
```

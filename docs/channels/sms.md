---
layout: default
title: Text/iMessage Channel Setup
---

# Text/iMessage Channel

Calciforge exposes text/iMessage routing as `kind = "sms"`. Under the hood it uses
the `zeroclawlabs::LinqChannel` transport, which can send and receive
iMessage, RCS, and SMS through the Linq Partner API.

Inbound messages arrive as Linq webhooks. Outbound replies go through the Linq
API, but still pass through Calciforge identity resolution, routing, security
scan settings, and artifact fallback rendering.

```text
phone user  ->  Linq webhook  ->  Calciforge  ->  agent
phone user  <-  Linq API      <-  Calciforge  <-  agent
```

## Configure

```toml
[[channels]]
kind = "sms"
enabled = true
sms_linq_api_token_file = "~/.calciforge/secrets/linq-token"
sms_from_phone = "+15555550001"
sms_webhook_listen = "0.0.0.0:18798"
sms_webhook_path = "/webhooks/sms"
allowed_numbers = ["+15555550100"]

# Recommended for public webhooks.
# sms_linq_signing_secret_file = "~/.calciforge/secrets/linq-webhook-secret"

# Optional security scan for inbound messages.
# scan_messages = true
```

```toml
[[identities]]
id = "operator"
display_name = "Operator"
role = "owner"
aliases = [
  { channel = "sms", id = "+15555550100" },
]
```

## Linq Webhook

Point the Linq Partner webhook at:

```text
https://YOUR-HOST.example.com/webhooks/sms
```

If `sms_linq_signing_secret_file` or `sms_linq_signing_secret` is configured,
Calciforge verifies `X-Webhook-Timestamp` and `X-Webhook-Signature` before
parsing the payload.

## Verify

```bash
calciforge doctor
calciforge
```

Send `!ping` from an allowed phone number. Calciforge replies to the Linq
conversation id when the webhook includes one, otherwise it replies directly to
the sender phone number.

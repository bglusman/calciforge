---
layout: default
title: Text/iMessage Channel Setup
---

# Text/iMessage Channel

Calciforge exposes text/iMessage routing as `kind = "sms"`. The implemented
transport today is `zeroclawlabs::LinqChannel`, which can send and receive
iMessage, RCS, and SMS through the Linq Partner API. RCS is the richer carrier
messaging format that can support more app-like features when the provider and
device both support them.

Linq is useful, but it should not be the only path. Twilio is the obvious next
provider adapter for SMS/MMS and, where the account and sender are approved,
RCS. Twilio's Programmable Messaging API supports SMS, MMS, RCS, and WhatsApp
from one Message resource, and its RCS docs describe branded profiles, read
receipts, rich content, and SMS fallback. That fits Calciforge's provider
adapter model better than pretending every text transport is the same castle
door.

Inbound messages arrive as Linq webhooks: HTTP calls Linq sends to your
Calciforge listener. Outbound replies go through the Linq API, but still pass
through Calciforge identity resolution, routing, security scan settings, and
artifact fallback rendering.

```text
phone user  ->  Linq webhook  ->  Calciforge  ->  agent
phone user  <-  Linq API      <-  Calciforge  <-  agent
```

## Configure

```toml
[[channels]]
kind = "sms"
enabled = true
sms_linq_api_token_file = "~/.config/calciforge/secrets/linq-token"
sms_from_phone = "+15555550001"
sms_webhook_listen = "0.0.0.0:18798"
sms_webhook_path = "/webhooks/sms"
allowed_numbers = ["+15555550100"]

# Recommended for public webhooks.
# sms_linq_signing_secret_file = "~/.config/calciforge/secrets/linq-webhook-secret"

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

## Provider Roadmap

Implemented:

- **Linq** — current text/iMessage/RCS/SMS transport through
  `zeroclawlabs::LinqChannel`.

Likely next:

- **Twilio Programmable Messaging** — SMS/MMS first, RCS where the sender,
  carrier, and country support it. See Twilio's
  [Programmable Messaging](https://www.twilio.com/docs/messaging) and
  [RCS Business Messaging](https://www.twilio.com/docs/rcs) docs.
- **Homelab phone gateway** — Android or modem-backed SMS for operators who
  want the phone number to stay on hardware they own. This is SMS, not a real
  RCS replacement, but it is much friendlier to basement-and-breadboard
  deployments than a paid carrier API.

Not promised yet:

- **General-purpose RCS without a provider** — RCS business messaging still
  depends on carrier/provider approval in practice. Calciforge should expose it
  cleanly when a provider offers it, but should not imply that self-hosted RCS
  is as straightforward as self-hosted Matrix or a USB LTE modem.

## Channel UI

Plain SMS is text-only. RCS can support suggested replies/actions through
provider-backed RCS Business Messaging, but Calciforge treats that as a richer
channel capability rather than assuming every `kind = "sms"` route can render
buttons.

For now, use deterministic text commands in SMS/iMessage and optionally keep
Telegram open as a Calciforge control surface for button-based agent/model
selection. Active selections are keyed by Calciforge identity and apply across
the operator's configured channels.

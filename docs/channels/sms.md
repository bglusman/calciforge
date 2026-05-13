---
layout: default
title: Text/iMessage Channel Setup
---

# Text/iMessage Channel

Calciforge exposes text routing as `kind = "sms"`. The current stable backend
is Linq, which can send and receive iMessage, RCS, and SMS through the Linq
Partner API. An experimental Twilio backend can send and receive SMS/RCS
through Twilio Programmable Messaging. RCS is the richer carrier messaging
format that can support more app-like features when the provider and device
both support them.

Inbound messages arrive as provider webhooks. Outbound replies go through the
provider API, but still pass through Calciforge identity resolution, routing,
security scan settings, and artifact fallback rendering.

```text
phone user  ->  provider webhook  ->  Calciforge  ->  agent
phone user  <-  provider API      <-  Calciforge  <-  agent
```

## Linq Config

```toml
[[channels]]
kind = "sms"
enabled = true
sms_provider = "linq"
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

## Twilio Config

Twilio support is experimental. It uses Twilio's standard Messaging webhook
format for inbound SMS/RCS and the Message resource for outbound replies. RCS
fallback is mostly a Twilio Messaging Service/Sender Pool concern, so the
Calciforge config usually points at `sms_twilio_messaging_service_sid` instead
of a single `sms_from_phone`.

```toml
[[channels]]
kind = "sms"
enabled = true
sms_provider = "twilio"
sms_twilio_account_sid_file = "~/.config/calciforge/secrets/twilio-account-sid"
sms_twilio_auth_token_file = "~/.config/calciforge/secrets/twilio-auth-token"
sms_twilio_messaging_service_sid = "MG_TEST_MESSAGING_SERVICE_SID"
sms_twilio_webhook_public_url = "https://calciforge.example.com/webhooks/sms"
sms_webhook_listen = "0.0.0.0:18798"
sms_webhook_path = "/webhooks/sms"
allowed_numbers = ["+15555550100"]
```

For local tunnels only, you can set:

```toml
sms_twilio_disable_signature_validation = true
```

Do not use that on a public endpoint. Twilio signs the externally configured
webhook URL, so `sms_twilio_webhook_public_url` must match the URL configured in
Twilio, including scheme, host, path, and query string.

## Linq Webhook

Point the Linq Partner webhook at:

```text
https://YOUR-HOST.example.com/webhooks/sms
```

If `sms_linq_signing_secret_file` or `sms_linq_signing_secret` is configured,
Calciforge verifies `X-Webhook-Timestamp` and `X-Webhook-Signature` before
parsing the payload.

## Twilio Webhook

Point the Twilio phone number, RCS sender, or Messaging Service inbound webhook
at the same path:

```text
https://YOUR-HOST.example.com/webhooks/sms
```

Twilio sends `application/x-www-form-urlencoded` fields such as `MessageSid`,
`From`, `To`, `Body`, `NumMedia`, and rich-message fields like `ButtonPayload`
or `InteractiveData`. Calciforge verifies `X-Twilio-Signature` when
`sms_twilio_webhook_public_url` is configured.

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

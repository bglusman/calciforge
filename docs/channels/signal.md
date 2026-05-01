---
layout: default
title: Signal Channel Setup
---

# Signal Channel

Calciforge's Signal channel embeds [`zeroclawlabs::SignalChannel`][zclaw] as a
library and talks to [`signal-cli-rest-api`][scra] (or any compatible
`signal-cli daemon --http` front-end) directly. There is no separate ZeroClaw
daemon in the runtime path.

[zclaw]: https://docs.rs/zeroclawlabs
[scra]: https://github.com/bbernhard/signal-cli-rest-api

The wire-protocol contract is `signal-cli`'s JSON-RPC + SSE API. As long as
your daemon implements that (signal-cli-rest-api is the reference; any
compatible re-implementation works), Calciforge will connect.

## What this gateway does

`signal-cli-rest-api` owns the Signal session (registration, encryption, the
libsignal store) and exposes it over HTTP. `zeroclawlabs::SignalChannel` is
the Rust client that subscribes to the SSE event stream for inbound messages
and POSTs JSON-RPC `send` requests for outbound replies. Calciforge wires
that client into its identity resolver, command dispatcher, and agent router.

ZeroClaw is no longer required. `signal-cli-rest-api` is the only external
dependency, and it is a generic Signal automation tool — not Calciforge- or
agent-specific.

## Architecture

```
Signal user  ──→  signal-cli-rest-api  ──→  zeroclawlabs::SignalChannel  ──→  Calciforge
```

Inbound and outbound flow through the same daemon over HTTP/SSE; the SSE
listener and the JSON-RPC sender live inside the Calciforge process.

## Prerequisites

- A running `signal-cli-rest-api` (or `signal-cli daemon --http`) with a
  registered Signal account. See the project README for registration steps
  (one-time SMS or QR-link).

## Step 1: Channel config

Add to `~/.config/calciforge/config.toml`:

```toml
[[channels]]
kind = "signal"
enabled = true

# HTTP URL of signal-cli-rest-api. Calciforge subscribes to SSE events at
# {url}/api/v1/events and sends via JSON-RPC at {url}/api/v1/rpc.
signal_cli_url = "http://127.0.0.1:8080"

# The bot's registered Signal number (E.164).
signal_account = "+15555550001"

# Allowed sender phone numbers in E.164 format. Must match identity aliases
# below. Use "*" to allow any number (not recommended).
allowed_numbers = ["+15555550001"]

# Optional: restrict to a single Signal group. Replies go back to that group.
# Use the literal "dm" to filter to direct messages only.
# signal_group_id = "group.abc123…"

# Optional: drop attachment-only messages (no text body). Default false.
# signal_ignore_attachments = false

# Optional: drop Signal "story" messages. Default false.
# signal_ignore_stories = false

# Optional: force plain text choice rendering. Default "auto".
# The current embedded Signal backend is text-first either way.
# ui_mode = "text"
```

| Field | Required | Default | Description |
|---|---|---|---|
| `signal_cli_url` | yes | — | HTTP URL of `signal-cli-rest-api` |
| `signal_account` | yes | — | Bot's registered Signal number (E.164) |
| `allowed_numbers` | yes | `[]` | E.164 senders allowed to interact |
| `signal_group_id` | no | — | Restrict to a specific group; or `"dm"` for DMs only |
| `signal_ignore_attachments` | no | `false` | Drop attachment-only messages |
| `signal_ignore_stories` | no | `false` | Drop story messages |
| `ui_mode` | no | `"auto"` | Reserved for channel-native controls; set `"text"` to force plain text fallback |
| `scan_messages` | no | `false` | Enable inbound adversarial content scanning |

## Channel UI

Signal currently renders Calciforge choices as deterministic text fallback.
Agent choices, model choices, session lists, and approval decisions all use the
shared choice model, so replies include actionable commands such as
`!agent switch <id>`, `!model use <id>`, `!switch <agent> <session>`,
`!approve <id>`, and `!deny <id>`.

The embedded transport sends through `zeroclawlabs::Channel::SendMessage`,
which exposes text, recipient, threading, cancellation, and attachments, but
does not expose Signal-native quick replies or buttons. If that backend grows a
safe native-control API later, Calciforge can render the same shared choices
without changing the command handlers. Set `ui_mode = "text"` to keep this
channel text-only for bridge-heavy or constrained clients.

## Migrating from the legacy webhook config

The previous Signal channel was a webhook receiver that forwarded replies
through a separate ZeroClaw daemon. If your config still has these fields:

- `zeroclaw_endpoint`, `zeroclaw_auth_token`
- `webhook_listen`, `webhook_path`, `webhook_secret`

…remove them from the `[[channels]]` block where `kind = "signal"` and
replace with the new shape above. These fields are also rejected by the
embedded WhatsApp channel; neither Signal nor WhatsApp uses a Calciforge webhook
sidecar in the current schema.

## Step 2: Identity config

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

Phone numbers in `allowed_numbers` that don't match any identity alias are
silently dropped at the auth boundary.

## Step 3: Run signal-cli-rest-api

The standard deployment is the upstream Docker image:

```bash
docker run -d --name signal-api \
  -p 8080:8080 \
  -v signal-cli-config:/home/.local/share/signal-cli \
  -e MODE=json-rpc \
  bbernhard/signal-cli-rest-api
```

`MODE=json-rpc` is required — Calciforge talks JSON-RPC + SSE, not the
older REST endpoints.

## Step 4: Verify

```bash
calciforge doctor   # validates config
calciforge          # start; send a Signal message from an allowed number
```

Check logs for `Signal channel listening via SSE on …`. A health check on
`signal-cli-rest-api` itself:

```bash
curl http://127.0.0.1:8080/v1/health
```

## Channel UI

Signal is a high-priority channel for future richer controls, but the current
interface should be treated as text-first. Agent choices, model choices,
session lists, and approval decisions all render through the shared choice
model, but the embedded Signal transport sends the text fallback because
`zeroclawlabs::Channel::SendMessage` does not expose Signal-native controls.
Keep using deterministic commands such as `!agent switch <id>`, `!model use
<id>`, `!switch <agent> <session>`, `!approve <id>`, and `!deny <id>` until the
Signal transport exposes safe native affordances.

You can also use Telegram as the Calciforge control surface for buttons while
continuing the main chat in Signal. Active agent/model selections are keyed by
Calciforge identity, so choices made through Telegram apply to the same
operator's Signal route.

<div class="channel-ui-grid">
  <figure>
    <img src="../assets/channel-ui-signal-fallback.svg" alt="Signal text fallback for agent and model selection">
    <figcaption>Signal remains text-first while richer channel controls are evaluated.</figcaption>
  </figure>
</div>

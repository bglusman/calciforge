---
layout: default
title: Matrix Channel Setup
---

# Matrix Channel

Calciforge connects to Matrix via the [Client-Server API v3](https://spec.matrix.org/v1.9/client-server-api/)
using **HTTP long-polling** (`/sync`). No webhook endpoint or open firewall port required.

> **No end-to-end encryption.** The Matrix channel sends and receives plaintext `m.text`
> events only. E2EE is not supported due to compile-time dependency conflicts in the
> current workspace. Do not use this channel in rooms where E2EE is required.

## Architecture

```
Matrix user  ──→  homeserver  ──→  Calciforge (/sync long-poll)
                                          │
                                  identity resolution
                                  (allowed_users check)
                                  agent dispatch
                                          │
Matrix user  ←──  homeserver  ←──  Calciforge (PUT /send/m.room.message)
```

## Prerequisites

1. **Register a Matrix account** for the bot on your homeserver (or matrix.org for testing).
   The account does not need to be a human account — a plain `@calciforge-bot:example.com`
   works fine.
2. **Generate an access token** for that account:

```bash
curl -s -X POST 'https://matrix.example.com/_matrix/client/v3/login' \
  -H 'Content-Type: application/json' \
  -d '{
    "type": "m.login.password",
    "user": "@calciforge-bot:example.com",
    "password": "botpassword"
  }' | grep access_token
```

   Copy the `access_token` value from the response.

3. **Find the room ID** for the room you want the bot to listen in:
   - In most clients: room settings → Advanced → Internal room ID
   - Format: `!abc123def456:example.com`
   - The bot will auto-accept room invites from users listed in `allowed_users`

## Step 1: Save the access token

```bash
install -m 600 /dev/null ~/.calciforge/secrets/matrix-token
printf '%s' 'syt_YOUR_ACCESS_TOKEN_HERE' > ~/.calciforge/secrets/matrix-token
```

## Step 2: Channel config

Add to `~/.calciforge/config.toml`:

```toml
[[channels]]
kind = "matrix"
enabled = true
homeserver = "https://matrix.example.com"
access_token_file = "~/.calciforge/secrets/matrix-token"
room_id = "!abc123def456:example.com"
allowed_users = ["@operator:example.com"]
```

| Field | Required | Description |
|---|---|---|
| `homeserver` | yes | Full URL of the Matrix homeserver |
| `access_token_file` | yes | Path to file containing the bot's access token |
| `room_id` | yes | Internal room ID (starts with `!`) |
| `allowed_users` | yes | Matrix user IDs permitted to send commands; empty list allows all room members (not recommended) |
| `scan_messages` | no (`false`) | Enable inbound adversarial content scanning |
| `allow_chat_secret_set` | no (`false`) | Allow `!secure set` via Matrix (not recommended) |

## Step 3: Identity config

The alias `id` is the full Matrix user ID including homeserver:

```toml
[[identities]]
id = "operator"
display_name = "Alice"
role = "admin"
aliases = [
    { channel = "matrix", id = "@alice:example.com" },
]

[[routing]]
identity = "operator"
default_agent = "librarian"
allowed_agents = ["librarian"]
```

Messages from Matrix users not in `allowed_users` are ignored before identity resolution.
Messages from `allowed_users` members with no matching identity alias are also dropped.

## Step 4: Invite the bot

Invite `@calciforge-bot:example.com` to the room. Calciforge will auto-accept the invite
if the inviting user's Matrix ID is in `allowed_users`.

## Step 5: Verify

```bash
calciforge doctor   # validates config
calciforge          # start; send a message in the room
```

The bot responds to commands (`!help`, `!ping`, `!agents`, etc.) and routes other messages
to the default agent for the sender's identity.

---
layout: default
title: Channel Secret Input Deprecation
---

# Channel Secret Input Deprecation

Status: proposed direction

Calciforge should treat chat-based secret value entry as a last-resort
path, not as the normal onboarding flow. The preferred paths are:

- `!secure input NAME` or `!secure bulk LABEL`, which returns a
  short-lived LAN paste URL without sending the value through chat
- `paste-server NAME` or `paste-server --bulk ...` for short-lived
  local browser input
- `fnox set NAME` on the host, when fnox is installed/configured
- future MCP-driven local paste URLs for agents that discover missing
  secrets

Calciforge and `fnox` can share the same `fnox.toml` and profile.
Installing the `fnox` binary remains useful for manual management
(`fnox set/list/tui`) and as the current default local storage backend
for `paste-server`, even if Calciforge eventually grows a no-external-
binary store or a fnox-library write path.

The channel path is attractive when traveling or operating from a
phone, but raw secret values then land in chat transport, client
history, backups, notification previews, and provider retention. That
is a very different threat model from host-local input.

Risk is channel-dependent:

- Self-hosted encrypted Matrix is the least bad case, especially when
  the homeserver, clients, and retention settings are under the same
  operator's control.
- Signal is still a chat-history tradeoff, but end-to-end encryption
  makes it less concerning than provider-readable channels.
- Telegram is a poor fit for raw secrets: ordinary bot chats are not
  end-to-end encrypted, and bot/API/provider retention makes accidental
  long-lived exposure easy.

Recommended product direction:

- Keep public docs focused on `!secure input`, host-local
  `paste-server`, and direct fnox input for operators who want CLI/TUI
  management.
- Gate chat value entry behind per-channel config such as
  `allow_chat_secret_set = true`.
- Treat off-LAN paste links as a separate design: short-lived,
  authenticated tunnel/proxy only, never a long-lived public form.
  Calciforge supports `CALCIFORGE_PASTE_PUBLIC_HOST` for stable LAN
  addresses and `CALCIFORGE_PASTE_PUBLIC_BASE_URL` for a reverse proxy,
  but the proxy design still needs hardening guidance.
- Keep any channel-flow copy blunt: only for low-stakes keys, travel
  emergencies, or values the operator plans to rotate afterward.
- Consider a future removal path once MCP/local paste flows are
  ergonomic enough that the chat fallback no longer earns its risk.

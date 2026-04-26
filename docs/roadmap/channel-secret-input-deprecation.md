# Channel Secret Input Deprecation

Status: proposed direction

Calciforge should treat chat-based secret value entry as a last-resort
path, not as the normal onboarding flow. The preferred paths are:

- `fnox set NAME` on the host
- `paste-server NAME` or `paste-server --bulk ...` for short-lived
  local browser input
- future MCP-driven local paste URLs for agents that discover missing
  secrets

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

- Keep public docs focused on direct fnox input and the local web UI.
- Gate chat value entry behind per-channel config such as
  `allow_chat_secret_set = true`.
- Keep any channel-flow copy blunt: only for low-stakes keys, travel
  emergencies, or values the operator plans to rotate afterward.
- Consider a future removal path once MCP/local paste flows are
  ergonomic enough that the chat fallback no longer earns its risk.

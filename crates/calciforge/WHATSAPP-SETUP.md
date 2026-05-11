# WhatsApp Channel Setup

This crate-local guide is intentionally only a pointer now. The old
ZeroClaw/OpenClaw webhook sidecar setup was retired, and keeping that full
example here caused stale instructions to diverge from the supported channel.

Use the canonical channel guide instead:

- [docs/channels/whatsapp.md](../../docs/channels/whatsapp.md)

Current summary:

- Configure `[[channels]]` with `kind = "whatsapp"`.
- Store the WhatsApp Web session with `whatsapp_session_path`.
- Add `whatsapp` aliases to `[[identities]]`.
- Remove legacy webhook fields such as `zeroclaw_endpoint`,
  `zeroclaw_auth_token`, `webhook_listen`, `webhook_path`, and
  `webhook_secret`; Calciforge rejects them for `kind = "whatsapp"`.

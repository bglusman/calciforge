# Local secret-input web UI — RFC

Status: PARTIALLY IMPLEMENTED — `crates/paste-server` provides the
input-only, new-by-default local form. MCP integration is still a
follow-up; for now the CLI prints the single-use URL.

## Origin

User asked: does fnox have a UI for the secret-input case? And —
should we build a small input-only web UI as a complement to
chat-based secret input, with a "new only / no update" default to
make compromise less catastrophic?

## fnox UI status

- **No web UI** in the fnox binary
- `fnox tui` is a terminal UI for browsing/editing — not what we want
- Vault providers (1Password, vaultwarden) bring their own UIs but
  they're focused on retrieval, not narrow input

So if we want input-only, we build it.

## Design

A tiny HTTP server bound to `127.0.0.1:<random>`, spawned today by
the `paste-server` CLI and eventually by agents calling the MCP
`add_secret_request` tool. It returns a one-shot URL plus a token the
user must open out-of-band.

### URL flow

1. Operator runs `paste-server NAME [DESCRIPTION]`, or a future agent
   flow calls the MCP, which:
   - allocates a random port (or uses a configured one)
   - generates a 32-byte random token
   - records `(token, NAME, expires_at, status: pending)` in an
     in-memory map keyed by token
   - prints the URL + token to the originating channel
2. User opens `http://127.0.0.1:<port>/paste/<token>` in a browser
3. Browser shows a single text field labeled with the secret name +
   description. Submit POSTs the value to `/paste/<token>` (POST).
4. Server validates the token, calls `FnoxClient::set(name, value)`,
   marks the entry `done`, and renders a confirmation page that
   shows the **first/last N characters** (configurable, default 4
   each) so the user can verify "yes that's the value I pasted"
   without re-displaying the full value.
5. Server shuts down the listener immediately after one successful
   submission (or on token expiry — default 5 min).

### "New only / no update" default

User's refinement, and a good one. The submission handler refuses
when the secret already exists in fnox unless the user passed an
explicit `?update=1` query param (which the URL we generate
deliberately omits). Rationales:

- **Eliminates accidental clobber** — paste a value into the wrong
  tab and you don't silently overwrite a working secret.
- **Compromised-browser blast radius shrinks** — an attacker who can
  hijack the localhost form can add new secrets but cannot rotate
  existing ones, so existing-cred theft is unaffected. (Rotation
  goes through `fnox set` CLI on the host, with shell-history /
  audit semantics.)
- **Audit trail stays clean** — every value-change event is "user
  ran `fnox set` deliberately", not "user clicked a link from chat".

If a user really wants to rotate via this UI, they can pass
`?update=1` themselves once they understand what they're doing —
small friction, big safety win.

### First/Last N preview (verification mode)

Default off; toggleable per-deployment via config. When on, after
successful set:

```
Stored secret 'BRAVE_KEY' (sk-b…2x4q)
   ✓ value submitted matches the truncated preview above.
   The full value is in the vault; this page will not display it.
```

Truncation length defaults to 4 chars each end, configurable up to
8. Reasoning:

- 4-and-4 is enough to confirm "right secret, right paste" by
  visual match against another out-of-band source (e.g., the
  dashboard you copied from).
- A leak of 8 total chars is a meaningful entropy haircut on a
  30-char API key but doesn't collapse it (still ~2^110 entropy on
  a random base64 secret).
- For low-entropy or short tokens, deployments can disable the
  preview entirely.

### Threat model deltas vs channel value entry

| Property | Chat-channel value entry | This UI |
|---|---|---|
| Value transits chat transport | YES (Telegram/Matrix logs forever) | NO |
| Value visible in client history | YES | NO (only in browser tab during paste) |
| URL is single-use, expiring | n/a | YES (random token, 5 min default) |
| Rotates existing secret by accident | YES | NO (default off) |
| Visible to other shell users via `ps` | YES briefly | NO (server reads via HTTPS form post) |
| Requires a browser on the LAN | NO | YES (limitation §12.8 of agent-secret-gateway) |

Good security improvement on every front EXCEPT the LAN
requirement (which is the same constraint we already documented in
RFC §12.8).

### Threats this design DOESN'T fix

- Compromised browser session (extension reading the form) — same
  risk as any web form.
- DNS rebinding from an attacker page → POST to localhost — mitigate
  with token validation + Origin header check; document the
  fixed-localhost-binding threat.
- The user pasting into the wrong tab (the page IS this tab, but if
  they have multiple paste-NAME tabs open simultaneously, the
  right-form check is "does the page header still say the same NAME
  I expected?")

### Implementation cost

- ~250 LoC Rust crate `crates/paste-server`:
  - `axum` (already a workspace dep) HTTP server
  - 1 GET (form), 1 POST (submit), 1 GET (confirmation)
  - in-memory token store, single-listener-per-request lifecycle
  - `FnoxClient` (PR #21) for the actual set
- ~50 LoC integration: MCP `add_secret_request` returns a URL
  instead of a stub message.
- Frontend: 1 minimal HTML form (~40 lines, no framework). Don't
  ship JS; submit is a plain HTML form POST.
- Tests: 4-5 given/when/then around token validation, expiry,
  new-only enforcement, preview rendering.

Small project. ~1 day to MVP, ~3 days to merge-ready with full
test coverage.

### What I'd skip

- **No HTTPS** — localhost-only binding, plain HTTP is fine and
  avoids the cert-management hassle. If we ever expose this beyond
  localhost (e.g., via Tailscale), HTTPS becomes mandatory.
- **No JS framework** — single HTML form, plain POST. Less code,
  fewer attack surfaces.
- **No CSS framework** — minimal inline styles. The page is used
  for ~10 seconds at a time.
- **No persistent token storage** — in-memory map; if the daemon
  restarts mid-paste, you start over. Acceptable for the use case.

## Recommendation

**Build it.** Specifically:

1. Implement the input-only, new-only-default version as described.
2. Make the verification preview opt-in per deployment (so
   conservative users disable it entirely).
3. Treat this as the canonical path for any secret where chat-
   transport retention is unacceptable.
4. The MCP `add_secret_request` tool already exists (PR #23,
   currently stubbed) — wire it to spawn this server so the agent
   can offer the URL to the user automatically when it discovers a
   missing secret.
5. Keep channel-based value entry out of primary user docs while
   deprecation/opt-in policy settles. See
   `docs/roadmap/channel-secret-input-deprecation.md`.

## Out of scope here (parking)

- Mobile users on cellular — same constraint as before; needs a
  separate "remote paste" path (Tailscale, mobile browser opening
  laptop's localhost via tunnel, etc.). Not solving today.
- Multi-tenant — the form has no auth beyond the URL token because
  it's localhost-only. If we ever go remote, add an HTTP basic-auth
  layer in front.
- Updates UI — explicitly omitted. If we add it later, it's a
  separate URL with explicit "I am rotating this secret"
  confirmation step.

## Decision

Awaiting green-light to scaffold the crate. Estimating one focused
day to ship a working MVP gated behind a config feature flag.

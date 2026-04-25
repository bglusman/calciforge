# Fnox Secret Input UI - Roadmap Note

Status: proposal  
Related work: `!secure` command flow, OneCLI/fnox wrapper work

## Context

Upstream fnox is documented as a CLI-first encrypted/remote secret manager:

- Repository: <https://github.com/jdx/fnox>
- Configuration reference: <https://fnox.jdx.dev/reference/configuration.html>

The public docs cover `fnox init`, `fnox set`, `fnox get`, `fnox exec`, shell
integration, provider configuration, and global/project config. I did not find
a built-in fnox web UI in the upstream README or documentation.

ZeroClawed can still benefit from a very small local UI for one operation:
capturing new secret values without putting them into chat history.

## Product Goal

Provide a local, intentionally narrow web form for entering new secrets into
fnox as a complement to `!secure`.

The UI should feel like a secure input appliance, not a secret browser.

## Non-Goals

- No general secret management dashboard.
- No plaintext secret readback.
- No search/list page showing secret values.
- No default update/overwrite behavior.
- No remote multi-user management in the first version.

## Default Behavior

| Behavior | Default |
|---|---|
| Bind address | `127.0.0.1` |
| Secret operation | create-only |
| Updates | disabled |
| Plaintext output | never |
| Audit values | names and operation only |
| fnox value transport | stdin, not argv |
| CSRF protection | required |

## Minimal Flow

1. Operator opens the local UI.
2. Operator enters secret name, optional profile/config scope, and value.
3. UI asks for value confirmation without displaying the value.
4. Server calls the fnox wrapper with the value on stdin.
5. UI displays only success/failure, secret name, and optional submitted-value
   prefix/suffix confirmation.

Prefix/suffix confirmation is only derived from the value currently submitted
in the request. It must not call `fnox get` or read the stored secret back out.

## Update Policy

Create-only should be the default because overwrites are where accidental loss
and confusing UX tend to happen.

Possible modes:

| Mode | Behavior |
|---|---|
| `create_only` | Fail if the secret exists. Default. |
| `confirm_update` | Allow update only after explicit per-request confirmation. |
| `allow_update` | Allow update when config explicitly enables it. |

If fnox itself does not provide an atomic "set only if absent" primitive for a
provider, the wrapper should treat create-only as best effort and make that
clear in the response/audit event.

## Suggested API

Routes stay local-only by default:

```text
GET  /health
GET  /
POST /secrets
```

Example request body:

```json
{
  "name": "OPENAI_API_KEY",
  "value": "submitted secret value",
  "confirm_value": "submitted secret value",
  "profile": "default",
  "scope": "project",
  "allow_update": false
}
```

Example response:

```json
{
  "status": "ok",
  "operation": "create",
  "name": "OPENAI_API_KEY",
  "fingerprint": {
    "length": 42,
    "prefix": "sk-p",
    "suffix": "abcd"
  }
}
```

The fingerprint is optional and should be configurable. It is useful for human
matching, but it is still secret-derived metadata.

## Security Requirements

- The server must not log request bodies.
- The UI must set `Cache-Control: no-store`.
- The form should use `autocomplete="off"` and avoid retaining values after
  submission.
- The server must enforce maximum value size.
- The server should reject secret names outside an allowlisted character set.
- The server should use a short-lived CSRF token or same-origin nonce.
- The server should refuse non-loopback binds unless explicitly configured.
- The server should pass values to fnox over stdin and keep secret values out of
  command-line args, structured logs, error strings, and panic messages.

## Implementation Sketch

The smallest useful implementation can live in `onecli-client` or a separate
small binary. A separate binary is cleaner if the UI has a different lifecycle
from the runtime credential proxy.

Recommended first slice:

1. Add a `FnoxClient::set_stdin` wrapper that takes `name`, `value`, scope, and
   profile, and never formats the value into argv.
2. Add a tiny Axum service bound to `127.0.0.1`.
3. Render a server-side HTML form with no frontend build pipeline.
4. Add route tests that assert create-only defaults, no plaintext response, and
   no argv secret leakage.
5. Add one Playwright/browser smoke test only if the UI grows beyond the simple
   server-rendered form.

## Open Questions

- Should this be a standalone `fnox-input` binary or a mode of `onecli`?
- Should the UI require a local bearer token even on loopback?
- Does fnox expose a native create-if-absent operation per provider, or does the
  wrapper need provider-specific safeguards?
- Should submitted-value prefix/suffix be enabled by default, or only shown
  behind a checkbox?

## Recommendation

Build this, but keep it smaller than `!secure` rather than larger:

- first version: local, create-only, write-only, server-rendered;
- later versions: explicit update mode and better provider-specific existence
  checks;
- never add secret browsing or plaintext readback to this UI.

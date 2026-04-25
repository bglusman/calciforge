# Adversarial Code Review - 2026-04-25

This is a threat-oriented review of the current repository, focused on paths where
an attacker could bypass an intended trust boundary, extract secrets, or make a
security control appear stronger than it is. It is not a full audit.

## Scope

- `crates/zeroclawed`: channel webhooks, proxy setup, local model hooks
- `crates/onecli-client`: vault/proxy service defaults and secret-returning routes
- `crates/host-agent`: command execution and approval-token surfaces by search
- CI/security workflow coverage by spot check

## Highest Priority Findings

### 1. WhatsApp webhook HMAC currently accepts any non-empty signature

**Severity:** Critical  
**Evidence:** `crates/zeroclawed/src/channels/whatsapp.rs:824`,
`crates/zeroclawed/src/channels/whatsapp.rs:918`

When `webhook_secret` is configured, inbound WhatsApp webhook requests call
`verify_hmac_sha256`. The helper strips `sha256=`, ignores the secret and body,
and returns `true` for any non-empty signature header.

This creates a dangerous failure mode: the config and setup docs imply HMAC
protection, but an attacker who can reach the webhook only needs to include a
syntactically plausible `X-Hub-Signature-256` header.

**Recommended fix:**

- Replace the placeholder with the same `hmac` + `sha2` implementation already
  used by the Signal channel.
- Add tests for valid signature, wrong secret, malformed hex, missing prefix
  behavior, and tampered body.
- Consider making WhatsApp reject startup when `webhook_secret` is configured
  but signature verification is not compiled or available.

### 2. OneCLI exposes a bearer-token oracle on a network-wide default bind

**Severity:** High  
**Evidence:** `crates/onecli-client/src/config.rs:75`,
`crates/onecli-client/src/main.rs:53`, `crates/onecli-client/src/main.rs:58`,
`crates/onecli-client/src/main.rs:273`

The OneCLI service defaults to `0.0.0.0:8081` and registers `GET
/vault/:secret`, which returns the resolved token in JSON. I did not find an
authentication check around this route in the OneCLI service.

That means a default deployment can become a network-reachable secret disclosure
endpoint if the process has environment-backed or VaultWarden-backed secrets
available. Even if this route is intended for debugging, it is too sharp for a
wide bind default.

**Recommended fix:**

- Default `ONECLI_BIND` to `127.0.0.1:8081`.
- Disable `/vault/:secret` unless an explicit development flag is set.
- Prefer proxy-only injection paths that never return plaintext secrets.
- If the route remains, require authentication and audit each access without
  logging the token value.

### 3. Proxy startup logs configured backend headers

**Severity:** High  
**Evidence:** `crates/zeroclawed/src/proxy/mod.rs:144`

Proxy backend creation logs `headers = ?backend_config.headers` at info level.
Configured headers often contain `Authorization`, provider API keys, gateway
tokens, tenant identifiers, or other values that should not appear in normal
logs.

**Recommended fix:**

- Log only header names, not values.
- Redact known sensitive names such as `authorization`, `x-api-key`,
  `api-key`, `cookie`, and `set-cookie`.
- Add a unit test for the redaction helper so future proxy changes cannot
  accidentally reintroduce header-value logging.

### 4. Local model hooks execute config-provided shell through `sh -c`

**Severity:** Medium  
**Evidence:** `crates/zeroclawed/src/local_model/mod.rs:177`

Local model lifecycle hooks run a config-provided string via `sh -c`. This may
be acceptable for explicitly trusted local-admin hooks, but it is a large
execution surface if any installer, config migration, UI, or remote management
path can write that setting.

**Recommended fix:**

- Document hooks as trusted local-admin code execution, not as data.
- Prefer an argv-style hook configuration (`program` plus `args`) for new
  configurations.
- If string hooks remain, keep them out of any remotely writable or generated
  config surface and avoid including the full script in user-facing errors.

## Positive Signals

- Signal webhook HMAC uses `hmac` + `sha2` and has tests for valid signatures,
  wrong secret, malformed signature, and tampered body.
- Host-agent command execution search shows direct argv-style `Command` usage
  for privileged adapters, not broad shell interpolation.
- The repository has a `secret-scan` workflow using Gitleaks for pushes and PRs.
- The installer SSH shell quoting tests include semantic shell execution cases,
  so the quoting layer has at least one meaningful injection regression guard.

## Follow-Up Fix Order

1. Fix WhatsApp HMAC and add tests.
2. Lock down OneCLI defaults and plaintext vault route behavior.
3. Redact proxy header logging.
4. Reframe local model hooks as trusted code execution and consider argv-style
   configuration.

## Notes For The Fnox Secret Input UI Idea

A small fnox companion UI could be useful, but it should be designed as a
write-only secret intake flow rather than a secret browser.

Security defaults to preserve:

- Bind to localhost by default.
- Support create-only by default; updates require an explicit `--allow-update`
  or config flag.
- Never display stored plaintext after submission.
- Allow optional prefix/suffix confirmation only from the value currently typed
  in the form, not by reading the stored value back out.
- Submit secret values over POST and pass them to fnox over stdin, not argv.
- Audit secret names and operations, never values.

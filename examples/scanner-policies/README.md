# Calciforge Scanner Policy Examples

These Starlark policies are starting points for operator-owned security rules
loaded through `[[security.scanner_checks]] kind = "starlark"`.

They run in-process with `load()` disabled and a bounded call stack. Treat them
as configuration-layer policy: keep rules small, explicit, and easy to audit.

## Policies

- `allowed-destinations.star` reviews or blocks credential-shaped content when
  it is sent to destinations outside the configured allowlist.
- `command-denylist.star` blocks common destructive shell-command patterns in
  agent-visible content.
- `credential-language.star` reviews or blocks messages that request, reveal, or
  redirect credentials.

## Configuration

```toml
[[security.scanner_checks]]
kind = "starlark"
path = "/etc/calciforge/scanner-policies/credential-language.star"
fail_closed = true
max_callstack = 64
```

Copy policies into your Calciforge config directory before referencing them.
Edit the constants near the top of each file to fit your deployment.

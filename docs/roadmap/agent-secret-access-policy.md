---
layout: default
title: Agent Secret Access Policy
---

# Agent Secret Access Policy

Status: Implemented for MCP/CLI discovery and security-proxy substitution.

Calciforge currently keeps secret *values* out of agent context, but it
also gates secret-name discovery and placeholder substitution when a
Calciforge identity is known.

What exists today:

- `mcp-server list_secrets` and `calciforge-secrets list` expose fnox
  secret names visible to that process, filtered by the active secret
  access policy when `CALCIFORGE_AGENT_ID`, `CALCIFORGE_USER_ID`, or
  `CALCIFORGE_CHANNEL[_ID]` is set.
- `secret_reference` / `calciforge-secrets ref NAME` build
  `{{secret:NAME}}` placeholders and never return values; known
  identities may only build references for allowed names.
- `security-proxy` substitutes values at the network boundary, and
  refuses substitution for known request identities unless a policy rule
  allows the secret.
- per-secret destination allowlists can block substitution to
  disallowed hosts.

Policy shape:

```toml
[security.secret_access]
[[security.secret_access.rules]]
agents = ["research-*"]
users = ["brian"]
channels = ["signal"]
secrets = ["BRAVE_*", "SEARCH_*"]
```

Selectors are conjunctive: if a rule sets `agents`, `users`, and
`channels`, all configured selectors must match. Empty selector lists are
wildcards for that selector type. Secret patterns support `*`.

Identity sources:

- MCP and `calciforge-secrets` read `CALCIFORGE_AGENT_ID`,
  `CALCIFORGE_USER_ID`, and `CALCIFORGE_CHANNEL_ID` /
  `CALCIFORGE_CHANNEL`.
- `security-proxy` reads `x-calciforge-agent-id`, legacy `x-agent-id`,
  `x-calciforge-user-id`, and `x-calciforge-channel-id` /
  `x-calciforge-channel`, then strips these identity headers before
  forwarding upstream.

Compatibility rule: unknown identity preserves process-scoped behavior
for existing deployments. Known identity fails closed: no matching rule
means no discovery, no reference, and no substitution. Destination
allowlists remain a second, independent gate.

Remaining hardening work:

- ensure all managed agent launchers set stable identity env vars or
  headers by default.
- add operator examples to generated install output once the managed
  launcher path is finalized.

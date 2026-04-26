# Agent Secret Access Policy

Status: Roadmap.

Calciforge currently keeps secret *values* out of agent context, but it
does not yet enforce per-agent secret ACLs.

What exists today:

- `mcp-server list_secrets` and `calciforge-secrets list` expose fnox
  secret names visible to that process.
- `secret_reference` / `calciforge-secrets ref NAME` build
  `{{secret:NAME}}` placeholders and never return values.
- `security-proxy` substitutes values at the network boundary.
- per-secret destination allowlists can block substitution to
  disallowed hosts.

What does not exist yet:

- per-agent filtering of discoverable secret names.
- per-agent authorization before a placeholder is substituted.
- user/channel-scoped secret policy.

Likely implementation:

- add a shared policy module used by MCP, CLI, and `security-proxy`.
- express policy in config as agent/user/channel selectors plus
  allowed secret-name patterns.
- fail closed: if an agent identity is known and no matching rule
  allows the secret, do not list it and do not substitute it.
- keep destination allowlists as a second, independent check.

Until this lands, treat secret discovery as process-scoped and
destination-restricted, not agent-restricted.

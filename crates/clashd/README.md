# clashd

Policy sidecar for OpenClaw — centralized Starlark-based policy enforcement with domain filtering, threat intelligence feeds, and per-agent scoping.

## Overview

clashd evaluates every OpenClaw tool call through a Starlark policy before execution. Policies can:

- **Block/allow based on tool and arguments**
- **Filter by domain** (exact match, regex patterns, subdomain matching)
- **Query threat intelligence feeds** (malware lists, ad servers, etc.)
- **Apply per-agent rules** (different policies for different agents)
- **Require custodian approval** for sensitive operations

## Quick Start

```bash
# Build
cargo build -p clashd --release

# Run with default policy
CLASHD_POLICY=./config/default-policy.star ./target/release/clashd

# Run with custom agent configs
CLASHD_POLICY=./config/default-policy.star \
  CLASHD_AGENTS=./config/agents.json \
  ./target/release/clashd
```

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CLASHD_PORT` | `9001` | HTTP server port |
| `CLASHD_POLICY` | `~/.clash/policy.star` or `/etc/clash/policy.star` | Path to Starlark policy file |
| `CLASHD_AGENTS` | `~/.clash/agents.json` or `/etc/clash/agents.json` | Per-agent policy configs |

### Starlark Policy (`policy.star`)

The policy file must define an `evaluate(tool, args, context)` function:

```python
def evaluate(tool, args, context):
    """
    Evaluate a tool call.

    Args:
        tool: Tool name (e.g., "exec", "browser", "gateway")
        args: Tool arguments (dict)
        context: Evaluation context with:
            - agent_id: Which agent is calling (string or None)
            - domain: Extracted domain from args (string or None)
            - domain_lists: List of threat feeds that matched (list of strings)
            - agent_allowed_domains: Agent-specific allow list
            - agent_denied_domains: Agent-specific deny list

    Returns:
        "allow" | "deny" | "review" | {"verdict": "...", "reason": "..."}
    """

    # Example: Block domains in threat feeds
    if context.get("domain_lists"):
        return {"verdict": "deny", "reason": "Domain in blocklist"}

    # Example: Require review for gateway config changes
    if tool == "gateway" and args.get("action", "").startswith("config."):
        return {"verdict": "review", "reason": "Config changes need approval"}

    # Default allow
    return "allow"
```

### Agent Configuration (`agents.json`)

```json
{
  "agents": [
    {
      "agent_id": "librarian",
      "allowed_domains": [],
      "denied_domains": ["example.net"],
      "domain_list_sources": [
        {
          "name": "urlhaus-malware",
          "url": "https://urlhaus.abuse.ch/downloads/text/",
          "refresh_secs": 21600
        }
      ]
    }
  ]
}
```

| Field | Description |
|-------|-------------|
| `agent_id` | Unique agent identifier |
| `allowed_domains` | Domains this agent may access (empty = no restriction) |
| `denied_domains` | Domains this agent may NOT access |
| `domain_list_sources` | Dynamic threat feeds to fetch |

## Domain List Formats

clashd supports multiple domain list formats:

### Plain List
```
malware.com
phishing.org
# Comments allowed
badactor.net
```

### HOSTS Format
```
0.0.0.0 malware.com
127.0.0.1 adware.org
```

### Regex Patterns
Lines starting with `~` are regex patterns:
```
~.*\.malware\.com      # Match any subdomain of malware.com
~^tracking\..*          # Match domains starting with "tracking."
```

### URL Lists
Domains extracted from URLs:
```
http://malware.com/c2
https://phishing.org/login
```

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `GET /` | | Version and feature info |
| `GET /health` | | Health check |
| `POST /evaluate` | POST | Evaluate a tool call |
| `GET /domains/summary` | | List loaded domain lists |
| `GET /domains/check/{domain}` | | Check a domain against lists |

### Evaluate Request

```json
POST /evaluate
{
  "tool": "browser",
  "args": {"url": "https://example.com"},
  "context": {"agent_id": "librarian"}
}
```

### Evaluate Response

```json
{
  "verdict": "allow" | "deny" | "review",
  "reason": "Optional explanation"
}
```

## OpenClaw Integration

The `zeroclawed-policy-plugin` connects OpenClaw to clashd:

1. Plugin installed in OpenClaw's plugin directory
2. Plugin calls `POST /evaluate` before each tool execution
3. Response determines if tool is blocked, allowed, or needs approval

See `crates/zeroclawed-policy-plugin/` for the plugin implementation.

## Threat Intelligence Sources

Pre-configured sources in example config:

| Source | URL | Refresh |
|--------|-----|---------|
| URLHaus | `https://urlhaus.abuse.ch/downloads/text/` | 6h |
| StevenBlack Hosts | `https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts` | 24h |
| Yoyo Ad Servers | `https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=0` | 12h |

Add any HOSTS-format or plain-text blocklist URL.

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   OpenClaw  │────▶│ Policy Plugin│────▶│   clashd    │
│   (Agent)   │     │ (before_tool │     │  (Sidecar)  │
│             │     │    _call)    │     │             │
└─────────────┘     └──────────────┘     └──────┬──────┘
                                                │
                    ┌───────────────────────────┼──────────┐
                    │                           ▼          │
                    │  ┌────────────┐    ┌────────────┐   │
                    │  │  Starlark  │    │   Domain   │   │
                    │  │  Policy    │    │   Lists    │   │
                    │  │  Engine    │    │   Manager  │   │
                    │  └────────────┘    └────────────┘   │
                    │          │                │          │
                    │          ▼                ▼          │
                    │  ┌────────────────────────────────┐  │
                    │  │      Threat Intel Feeds        │  │
                    │  │  (URLHaus, StevenBlack, etc.)  │  │
                    │  └────────────────────────────────┘  │
                    └──────────────────────────────────────┘
```

## Security Model

- **Fail-closed**: If clashd is unreachable, the plugin blocks the tool call
- **No bypass**: All tool calls go through the policy hook
- **Audit trail**: Every evaluation logged with verdict and reason
- **Dynamic updates**: Threat feeds refresh automatically without restart

## License

MIT — see LICENSE in repository root.

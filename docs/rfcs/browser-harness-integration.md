# Browser-harness integration — spike

Status: SPIKE — 30-minute investigation, not a build commitment.
Filed because the user asked whether `browser-use/browser-harness`
could be wired into us (Claude Code, openclaw, or calciforge) to give
agents a usable browser primitive.

## TL;DR

**Yes, integrating browser-harness is straightforward, and three of
the four obvious integration points are cheap.** The harness is
~592 lines of Python that connects an agent to a *user-running*
Chrome via CDP and ships a small set of pre-imported helpers
(`new_tab`, `click_at_xy`, `capture_screenshot`, `js(...)`,
`http_get`, `cdp(...)`). The native usage model is "agent reads
SKILL.md and shells out to `browser-harness <<PY ... PY`" — Claude
Code already supports this pattern via skills.

The recommended first step is **(A) Claude Code skill** — zero code
changes on our side, ~5 minutes of setup, gives Brian browser
automation in any Claude Code session today. **Don't build an MCP
wrapper or a Rust port until that's been used in anger and we know
what's missing.**

## What browser-harness actually is

- ~592 lines of Python; a binary CLI (`browser-harness`) installed
  via `uv tool install -e .`
- Connects to the user's already-running Chrome via the Chrome DevTools
  Protocol (CDP) — no Playwright, no headless browser, no separate
  browser process
- Exposes a heredoc API: `browser-harness <<'PY' ... PY` — the script
  body runs in a Python REPL with the helpers pre-imported
- Self-healing: agents can `js(...)` arbitrary DOM inspection or use
  `cdp("Domain.method", params)` for anything the helpers don't cover
- Optional remote daemon (cloud-hosted browser profiles) gated by
  `BROWSER_USE_API_KEY` — paid feature, not required for local use
- Stores "cookies-only login state" in profiles; explicitly designed
  not to surface raw credentials to the agent

## Hard requirements

- Chrome or Chromium running on the same host as the agent
- Chrome's remote-debugging checkbox enabled once per profile
  (auto-sticky after first toggle)
- Python + `uv` toolchain installed
- The agent's executor must be able to run a subprocess pipeline with
  heredoc-style stdin

## Integration options, ranked by cost

### (A) Claude Code skill — RECOMMENDED FIRST STEP

Cost: ~5 minutes per machine. Zero code changes.

1. `git clone https://github.com/browser-use/browser-harness && cd browser-harness && uv tool install -e .`
2. Copy `SKILL.md` from the repo into `~/.claude/skills/browser-harness/SKILL.md`
3. Done — Claude Code reads the skill on session start, knows to call
   `browser-harness <<PY ... PY` for browser actions

Caveat: skill text gets injected into the session prompt — adds tokens
to every Claude Code invocation. That's true for all Claude Code
skills; mention it as a tradeoff, don't try to engineer around it.

### (B) calciforge-MCP tool wrapper — SECOND STEP IF (A) CONFIRMS VALUE

Cost: ~half a day. Adds a `browse(action, params)` tool to
`crates/mcp-server` (the MCP server we just scaffolded for secret
discovery in PR #23). The tool would shell out to `browser-harness`
exactly as the skill does, but agents would discover it via MCP
instead of reading a skill prompt.

Tradeoffs vs (A):
- ✓ Works for agents that don't have skill support (opencode-style
  consumers)
- ✓ Runs through one place we control — natural place to add a
  `clashd` policy check on browser actions
- ✓ Doesn't bloat every session prompt
- ✗ Requires every browser action to go through MCP request/response
  (browser-harness's heredoc model is more expressive — multi-step
  scripts in one invocation)
- ✗ We'd have to design tool-shaped wrappers for every helper or
  expose a single "run this Python" tool, which is just MCP-flavored
  shell access

The honest read: (B) is only worth it if multi-agent (non-Claude)
browser support becomes a real need. Otherwise the skill model in (A)
matches browser-harness's actual design intent.

### (C) Rust port — DON'T

Cost: weeks. browser-harness is small (592 lines) but the value isn't
in line count — it's in being a thin shim over CDP that lets the agent
write helpers on the fly. Porting to Rust loses the "agent edits the
helper file mid-task" property. If we want browser automation in the
calciforge daemon itself (e.g., for autonomous channel-driven web
tasks), a Rust port would be appropriate. We're nowhere near that
need.

### (D) Run browser-harness inside calciforge-daemon as a subprocess pool

Cost: 1-2 days of glue. calciforge daemon spawns persistent
browser-harness processes per identity, exposes them as a channel
adapter (e.g., `[[agents]] kind = "browser"`). Lets the agent send
"please log in to X and do Y" via Telegram and have it execute
end-to-end without the user being at a Chrome window.

Tradeoffs:
- ✓ Powerful — closes the loop on async web automation
- ✗ Big — needs Chrome running headfully on the host (X server / VNC
  setup if it's a Linux container), credential injection through
  the security-proxy, output capture/streaming
- Defer to after a real use case appears.

## Concrete decision

Set up (A) on Brian's Mac today. It's 5 minutes; the value is
immediate. Use it for two weeks. If by then there's a clear pattern
of "I want to do this from Telegram, not from Claude Code" or "I want
opencode/zeroclaw to do this too", graduate to (B). Don't pre-commit
to (C) or (D).

## Security/secret considerations

- The harness's profile mechanism stores "cookies-only login state" —
  on its face, raw secrets aren't surfaced. Verify this claim before
  trusting it for accounts that hold high-value secrets.
- `BROWSER_USE_API_KEY` (cloud-daemon feature) should be a
  `{{secret:BROWSER_USE_API_KEY}}` reference per the substitution
  RFC §3 once we wire it.
- Agents using browser-harness can scrape any page the user is
  logged in to. That's a categorical capability expansion — worth
  noting in any deployment docs that ship the integration.
- A clashd policy check on the `browse` MCP tool (option B) would
  give us per-domain/per-action gating, something the skill model (A)
  can't enforce.

## What I did NOT do

- Did not install browser-harness locally yet — option (A) is yours
  to do at your console (the `chrome://inspect` checkbox needs a
  human eyeball), and the cost of doing it for you is no lower than
  the cost of doing it yourself.
- Did not write any code. Pure investigation + recommendation.
- Did not file a follow-up task — the next move is "Brian uses (A)
  for two weeks then we triage", which is human-driven, not a queued
  engineering item.

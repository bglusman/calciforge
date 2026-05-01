---
layout: default
title: Product UX Direction
---

# Product UX Direction

Status: working direction

Calciforge currently exposes a lot of power through chat commands,
installer prompts, config files, docs, and service logs. That is
workable for early operators, but it makes the product feel clunky
because the user has to remember hidden state: which agent is active,
which channel they are speaking through, which host owns a local link,
which security layer is in force, and which command shape is safe.

The product should feel like a control surface between a person, their
agents, and the security gateway. Chat remains valuable, but commands
should behave more like a small conversational CLI: discoverable,
consistent, state-aware, and easy to recover from.

## Principles

- Make current state visible. `!status` should explain active agent,
  active session/model, channel identity, gateway health, and pending
  approvals in terms the operator can act on.
- Prefer recognition over recall. Help should show common next actions,
  not only syntax. Error replies should include the exact corrected
  command or the next safest command.
- Separate safe paths from risky fallbacks. Secret input, proxy bypass,
  agent switching, and model dispatch should label their trust boundary
  clearly.
- Keep text interfaces scriptable. Where a host CLI exists, support
  stable flags and eventually machine-readable output; where chat is the
  interface, use the same nouns and verbs.
- Give long-running work an explicit lifecycle. Starting, waiting,
  approving, resuming, and cancelling agent/orchestrator work should
  share one vocabulary across Telegram, Matrix, SMS/iMessage, and any
  local web client.

## Command Shape

The `!` prefix is not sacred. It is useful because it is easy to detect
and unlikely to collide with natural agent prompts, but it also makes
Calciforge feel like a bot command layer bolted onto an agent
conversation.

Near term, keep `!` for compatibility but make commands more
predictable:

- Use stable nouns: `agent`, `session`, `model`, `secret`, `policy`,
  `task`.
- Prefer noun/verb aliases in chat while preserving old commands.
  The first implemented set is `!agent list`,
  `!agent switch custodian`, `!session list claude-acpx`,
  `!model list`, `!model use dispatcher`, and
  `!secret input OPENAI_API_KEY`.
- Keep one-line shortcuts for frequent actions: `!status`, `!help`,
  `!approve`, `!deny`.
- Add "did you mean" recovery for unknown commands and missing
  arguments.

Longer term, consider a mode where the channel adapter can accept
slash-style commands, quick replies, buttons, or a local web control
panel when the channel supports them. The text grammar should remain
the source of truth so agents and humans can use the same operations.

## Channel-Native Affordances

Calciforge should not force every channel into plain text when the
transport offers better controls. Keep text as the universal fallback,
but model richer controls as optional channel capabilities:

- Choice controls: buttons, quick replies, polls, or menus for actions
  such as switching agent/model, selecting a dispatcher, approving a
  tool call, or choosing a secret-input flow.
- Media and artifact controls: native image/file/audio delivery where
  the channel supports it, with text fallback that names the artifact
  and gives the safe next action.
- State signals: reactions, read receipts, typing indicators, status
  updates, or pinned summaries when those affordances exist and do not
  leak sensitive information.

Expose these capabilities through configuration, not channel assumptions.
`ui_mode = "auto"` can enable safe native controls for a direct channel while
`ui_mode = "text"` keeps bridge-heavy setups, such as WhatsApp through Matrix
or Beeper, on the plain text interface. Button presses should always call the
same command handlers as text input so both modes stay behaviorally identical.

- Forms/deep links: local web forms for secret input, policy review, or
  dispatcher configuration when chat controls are too limited.

iMessage and WhatsApp likely have useful non-text surfaces. Telegram,
Matrix, and SMS/iMessage need explicit research against their current
APIs and the libraries Calciforge uses before committing to a shared
abstraction. A reasonable architecture is a channel capability trait:
handlers ask for a high-level interaction such as "single choice",
"approval", "artifact", or "form link"; each channel renders the best
native affordance it can, then falls back to deterministic text.

WhatsApp is worth treating as a dependency-risk item. If the embedded
WhatsApp Web library cannot expose reply buttons or lists safely, Calciforge
can still ship text/media support and use Telegram or the local web UI as a
control surface. A narrow fork may be justified later if native WhatsApp
controls become important enough and the upstream crate does not accept or
prioritize the needed API surface.

## Secret Input UX

`!secure input` and `!secure bulk` should read as local-network paste
flows:

- The reply must say the link works from browsers that can reach the
  Calciforge host on the LAN.
- The generated link must use a reachable LAN host or a configured
  public base URL, never `127.0.0.1` for a chat-started LAN flow.
- Off-LAN links require an authenticated reverse proxy or short-lived
  tunnel. The paste server should not be exposed directly to the public
  internet.
- `!secure set` should remain a clearly marked fallback because the raw
  value passes through chat history.

## Local Web Control Surface

A local web UI would reduce chat-command pressure without replacing
chat:

- Show active identities, channels, agents, sessions, model routes, and
  gateway health.
- Provide guided forms for secret input, dispatcher selection, and
  channel testing.
- Show pending approvals and recent blocked requests with safe redacted
  detail.
- Offer copyable chat commands for the same operation so the user learns
  the text surface rather than being trapped in the UI.

This can start as localhost/LAN-only and later support authenticated
remote access if the security model is explicit.

## References

- [Command Line Interface Guidelines](https://clig.dev/) for
  discoverability, examples, state visibility, standard flags, and
  recoverable errors.
- [GitHub CLI accessibility notes](https://github.blog/engineering/user-experience/building-a-more-accessible-github-cli/)
  for treating terminal/text interfaces as distinct from web UI but
  still accessibility-sensitive.
- [GitHub Docs content design principles](https://docs.github.com/en/contributing/writing-for-github-docs/content-design-principles)
  for goal-oriented, high-impact documentation.
- [Nielsen Norman Group usability heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/)
  for visibility of system status, recognition rather than recall, and
  recoverable errors.

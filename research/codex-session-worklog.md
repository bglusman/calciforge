# Codex Session Work Log

Last updated: 2026-04-26 13:23 EDT.

## Automation handoff

Heartbeat `calciforge-overnight-worker-loop` should treat this file as the
durable handoff anchor before relying on chat memory. On each wakeup:

1. inspect `git status --short --branch`;
2. inspect local service health and `.210` service health;
3. avoid printing secrets from configs, tokens, or logs;
4. keep work on Codex-owned branches/worktrees;
5. do not merge PRs unless explicitly authorized in-thread;
6. update this file before stopping.

Current immediate resume order:

1. Debug local exec-backed `codex/gpt-5.5` gateway failures on
   `127.0.0.1:18083`; direct `codex exec -m gpt-5.5` works, but the gateway
   returned `service_unavailable`. Status: fixed locally by removing the stale
   `--ask-for-approval` CLI arg from local config and using `--ephemeral`.
   Verified authenticated gateway response from `codex/gpt-5.5`.
2. Finish `.210` repair. Status: remote Rust build completed after freeing
   disk and using `CARGO_BUILD_JOBS=1` with no release LTO. The resulting Linux
   binary was installed to `/usr/local/bin/calciforge`, `zeroclawed.service`
   restarted cleanly, and HTTP `/health` responds. The duplicate proxy-only
   systemd unit remains disabled. A subsequent `cargo install fnox --locked`
   again starved SSH banner exchange; the local SSH client was killed, but
   `.210` still accepts TCP/22 without completing SSH banner exchange while
   HTTP `/health` remains available. Do not start more remote build jobs until
   SSH recovers or is restarted externally.
3. After `.210` SSH recovers, configure it to consume the Mac subscription
   gateway via a file-backed provider key without printing that key. The Mac
   LAN address is `192.168.1.175`, and its gateway is healthy at
   `http://192.168.1.175:18083`.
4. Continue Matrix manual/E2E testing. Real `matrix.enjyn.com` account creation
   is blocked unless a registration token or existing non-bot account token is
   found/provided. Ephemeral homeserver testing remains viable for CI.
5. Implement cross-channel one-off reply only after the gateway/deployment path
   is stable, because it needs a shared channel-send abstraction.

## Active deployment repair

- Local Mac deployment is running Calciforge from this worktree build.
- `.210` deployment is running Calciforge from `/root/calciforge-codex-deploy`.
- `fnox` was present on the Mac but lacked global config; fixed with
  `fnox init --global --skip-wizard`.
- `.210` lacked `fnox`; cargo fallback failed because Linux was missing
  `pkg-config` / `libudev-dev`. Installer now installs those prerequisites
  before `cargo install fnox`.
- Installer now validates `fnox list`, initializes global fnox config when
  missing, and sets service PATHs to include `$HOME/.cargo/bin`.
- `.210` root disk was 99% full due stale build artifacts. Removed
  `/root/calciforge-codex-deploy/target` and npm cache to restore about 2.9 GB
  free before rebuilding.
- `cargo install fnox --locked` is not currently a safe unattended repair path
  on `.210`; it compiles a large dependency graph including OpenSSL and can
  starve SSH on the small VM. Prefer a prebuilt/package install path or compile
  off-box and copy the binary.

## Model gateway / subscription-backed work

- Direct Kimi gateway on `.210` responds through `http://127.0.0.1:8083`.
- Codex CLI on the Mac can run `codex exec -m gpt-5.5` using the existing
  subscription/OAuth context.
- Claude CLI on the Mac can run `claude -p` non-interactively.
- Added an executable-backed gateway provider path for `[[proxy.providers]]`
  with `backend_type = "exec"`, so synthetic models can include targets like
  `codex/gpt-5.5` and `claude/sonnet` without extracting subscription
  credentials into API keys.
- Added chat-completion bearer auth enforcement when `[proxy].api_key` or
  `api_key_file` is configured. Also enforced the same auth on `/v1/models`
  so exposing the Mac gateway on the LAN does not disclose configured model
  names without a token.
- Local Mac gateway verification:
  - unauthenticated `/v1/models` returns `401`;
  - authenticated `/v1/models` returns the configured synthetic models;
  - authenticated `codex/gpt-5.5` chat completion returned
    `codex-gateway-ok-2`.
- PR #54 review feedback was addressed and pushed:
  - installer test now asserts required steps and semantic ordering instead of
    exact full-vector equality;
  - Matrix mock test now uses `tokio::sync::Mutex`, owns mock server shutdown,
    and awaits aborted Matrix task cleanup.
  All four Copilot review threads were replied to and resolved. GitHub CI was
  green on `666ed225` when checked.

## Matrix manual testing

Goal: create/configure a Matrix identity controlled by Codex so it can send
messages to the real Calciforge Matrix channel as an external user.

Current status:

- Public registration on `matrix.enjyn.com` requires `m.login.registration_token`.
- No Synapse/Conduit/Dendrite admin service or homeserver config was found on
  `.210` or `.229`.
- Existing `.210` Calciforge Matrix bot is `@lucien:matrix.enjyn.com` and
  currently allows `@bglusman:beeper.com`.
- Next viable paths:
  1. obtain/create a Matrix registration token for `matrix.enjyn.com`;
  2. use an existing non-bot Matrix account token if one is available;
  3. stand up an ephemeral local Matrix homeserver for CI-style E2E, while
     keeping real-server manual testing blocked on account creation.

Do not use the bot's own token as the sender for inbound testing; Calciforge
intentionally ignores its own Matrix events.

## Requested feature backlog

- Cross-channel one-off reply command: allow an inbound message on one channel
  to request that the reply be delivered to a different Calciforge channel.
- Likely use cases: cron jobs, tooling integrations, and tests where the input
  channel is not the desired notification channel.
- Needs a channel-send abstraction shared by Telegram, Matrix, WhatsApp rather
  than direct same-channel `send_reply` calls embedded in each adapter.

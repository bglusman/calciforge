# Codex Session Work Log

Last updated: 2026-04-26 23:25 EDT.

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

1. Finish PR #54 review hygiene. Current head `01fa8c1e` had passing named CI,
   but GitHub still showed stale unresolved Copilot threads plus two real
   follow-ups: `scripts/exec-models/claude-print.sh` passing prompts via argv
   and `crates/calciforge/src/proxy/exec_gateway.rs` using `eprintln!` for
   cleanup logging. Both were fixed locally after `01fa8c1e`; commit and push
   them, then re-check review threads and CI. Do not merge.
2. Run local Matrix real E2E once Docker is actually usable. Homebrew now has
   `docker` and `docker-compose` installed, but Docker daemon/Colima/Lima
   availability still needs checking. If Docker is unavailable, install/start an
   appropriate local runtime or record the exact blocker.
3. Audit real deployment readiness for daily-driver use. Treat docs/readme
   “mature for personal use” language as provisional until Mac and `.210`
   install/config are proven by smoke tests from real channels and gateway
   endpoints.
4. Verify local Mac gateway and `.210` gateway health/config without printing
   secrets: fnox installed and initialized, api key files readable by services,
   provider URLs reachable, active agents sane, `.210` consuming the Mac
   subscription gateway, and broken legacy `.229` custodian route either fixed
   or disabled.
5. Validate `!model` propagation end-to-end for the agents the user actually
   uses: openclaw, custodian, Max/dad agent replacement path, and gateway-backed
   agents. Synthetic selections should affect future agent dispatch where the
   adapter supports model override.
6. Continue Matrix manual/E2E testing. Do not block on `matrix.enjyn.com`
   account creation for CI: use the disposable Synapse E2E harness added in
   `scripts/matrix-real-e2e.py`. It starts a real homeserver, registers a
   Calciforge bot user plus a separate allowed sender, opens a direct Matrix
   chat from the sender, and verifies Calciforge replies through the real
   Matrix Client-Server API.
7. Implement cross-channel one-off reply only after the gateway/deployment path
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

Goal: verify both deterministic Matrix logic and real homeserver behavior.

Current status:

- The in-process Matrix API test remains useful for fast, deterministic
  coverage inside `cargo test`.
- Added `scripts/matrix-real-e2e.py` for the missing real-server layer. It
  starts Synapse in Docker, registers `@calciforge:localhost` and
  `@alice:localhost`, starts Calciforge with no configured `room_id`, has Alice
  open a direct chat/invite Calciforge, waits for the bot to auto-join, sends a
  real Matrix message, and waits for the real Matrix reply.
- Added a `matrix-real-e2e` GitHub Actions job in
  `.github/workflows/integration-tests.yml`.
- Local Mac cannot run this script until Docker is installed; validation here
  was limited to Python bytecode compilation and the existing Matrix mock test.
- First GitHub Actions run exposed harness issues rather than Calciforge logic:
  the readiness deadline included cold-ish Rust compile time, and Synapse left
  Docker-owned files in the temp directory. The harness now reads stdout and
  stderr, allows a longer readiness window, tolerates cleanup permissions, and
  the workflow prebuilds Calciforge before running the Matrix script.
- Second GitHub Actions run showed Calciforge itself handled the real DM path:
  auto-join, message receive, CLI dispatch, and response generation all logged
  correctly. The assertion failed because the test observer used `/sync` polling
  in a way that missed the reply event. The harness now verifies replies via the
  room history endpoint for the DM room.
- After the basic DM E2E went green in GitHub Actions, the harness was expanded
  to cover command happy paths in the same real DM: `!ping`, `!help`, `!agents`,
  `!status`, `!metrics`, `!model`, `!sessions <non-acpx-agent>`, `!switch`,
  `!default`, and default/active CLI dispatch through two mock agents.
- That command-expanded run exposed another harness issue: the script isolated
  Calciforge state by setting `HOME` to the temp directory, which also hid
  rustup's default toolchain from `cargo run` in GitHub Actions. The harness now
  prefers the prebuilt `target/debug/calciforge` binary, supports an explicit
  `CALCIFORGE_BIN`, and preserves `RUSTUP_HOME` for local cargo fallback.
- Public registration on `matrix.enjyn.com` still requires
  `m.login.registration_token`. That only blocks manual testing against the
  production homeserver, not CI E2E coverage.

Do not use the bot's own token as the sender for inbound testing; Calciforge
intentionally ignores its own Matrix events.

## Requested feature backlog

- Cross-channel one-off reply command: allow an inbound message on one channel
  to request that the reply be delivered to a different Calciforge channel.
- Likely use cases: cron jobs, tooling integrations, and tests where the input
  channel is not the desired notification channel.
- Needs a channel-send abstraction shared by Telegram, Matrix, WhatsApp rather
  than direct same-channel `send_reply` calls embedded in each adapter.

## 2026-04-26 live routing / .229 reachability update

- Mac live Calciforge currently has `brian -> custodian` in active-agent state.
  Recent Telegram failures route through that `custodian` agent, not through the
  default `codex` agent.
- The Mac `custodian` agent endpoint is configured to target `.229` port
  `18789` with the `openclaw-native` adapter. `.229` has no listener on that
  port.
- `.229` is reachable over SSH and its nonzeroclaw service is healthy locally,
  but the gateway listens only on `127.0.0.1:18793`. From the Mac, both
  the stale `18789` endpoint and direct `.229:18793` refuse TCP connections.
- `.229` also has `[hooks] enabled = false`, so simply exposing `18793` would
  not automatically make the current `openclaw-native` `/hooks/agent` call path
  valid. Fix requires either a compatible adapter/endpoint or enabling and
  verifying hooks.
- Source fix in progress: `!model` now lists and activates all synthetic model
  classes, and channel adapters pass the active synthetic model override into
  agent dispatch. This makes `!model` selections affect future chat messages for
  gateway-backed adapters such as `openclaw-http`/`nzc-http`.
- Source fix in progress: the Codex CLI adapter default arguments were updated
  to remove obsolete `--ask-for-approval never` and use `--ephemeral` instead.
- Added synthetic model gateway E2E coverage for alloy, cascade, dispatcher, and
  oversized-context rejection using a deterministic mock backend.

## 2026-04-26 PR #54 / Mac deploy update

- Pushed PR #54 updates through `7bcc263a`.
- Review feedback was addressed and all open review threads were resolved.
- Local push gate passed: fmt, clippy, gitleaks, workspace unit tests, and loom.
- Additional local checks passed: `cargo test -p calciforge --bins`, proxy-focused tests,
  Python bytecode checks, `bash -n scripts/install.sh`, and synthetic model gateway E2E.
- Mac deployment updated `/Users/admin/.local/bin/calciforge` from the release build
  with a timestamped binary backup, then restarted `com.calciforge.calciforge`.
- Mac service health is OK on port `18083`.
- Direct Codex CLI smoke test with `gpt-5.5` and the new `--ephemeral` args succeeded.
- Live active agent for `brian` was reset from the broken `.229` `custodian` endpoint
  to `codex`; the previous active-agent state file was backed up.
- `.229` remains intentionally unmodified: its nonzeroclaw gateway is loopback-only
  on `18793`, the Mac config had been targeting stale port `18789`, and hooks are
  disabled there, so exposing a port alone would not fix the `openclaw-native`
  `/hooks/agent` path.

## 2026-04-26 .210 Mac subscription gateway update

- PR #54 CI is fully green and the PR is mergeable.
- `.210` SSH is healthy; `zeroclawed`, `nonzeroclaw`, and `nonzeroclaw-david`
  services are active.
- `.210` can reach the Mac gateway at `192.168.1.175:18083`; unauthenticated
  model listing returns `401`, as expected.
- Copied the Mac gateway bearer token into a root-only file on `.210` at
  `/etc/calciforge/mac-gateway-api-key` with mode `0600`.
- Added a `mac-subscription` HTTP provider to `.210` Calciforge pointing at the
  Mac gateway, plus `local-kimi-gpt55` and `claude-kimi-gpt55` dispatchers.
- Validated the modified `.210` config before restart, backed up the prior
  config, restarted only `zeroclawed.service`, and confirmed health on `8083`.
- Verified authenticated `.210 -> Mac gateway -> Codex gpt-5.5` chat completion
  with a minimal smoke prompt. The response matched the expected sentinel.
- Follow-up security hardening: `.210` still has at least one provider credential
  stored inline in a systemd drop-in. Move that into an `EnvironmentFile` or
  service-specific secret file before treating the deployment as clean.

## 2026-04-26 exec-model synthetic update

- Promoted executable-backed model gateway support from only
  `[[proxy.providers]] backend_type = "exec"` into first-class `[[exec_models]]`
  synthetic models.
- `[[exec_models]]` are black-box synthetic leaves: Calciforge renders a chat
  transcript, invokes the configured binary/script without shell interpolation,
  and wraps stdout or `{output_file}` contents as a chat completion.
- Synthetic model composition now recursively flattens alloys, cascades,
  dispatchers, and exec models as a DAG. Cycles are rejected during synthetic
  manager initialization.
- Added example wrappers under `scripts/exec-models/` for Codex CLI, Claude
  CLI, and a generic stdin CLI. These are documented as starting points because
  CLI flags and vendor subscription terms can change.
- Verification passed: `cargo test -p calciforge --bins`,
  `python3 scripts/model-gateway-synthetic-e2e.py`, shell syntax checks for the
  exec-model wrapper scripts, and a focused `tiktoken-estimator` test.

## 2026-04-26 late PR #54 / overnight handoff update

- Updated heartbeat `calciforge-overnight-worker-loop` to fire every 45 minutes
  and carry the current deployment-readiness priorities.
- PR #54 was rebased on `main` and force-pushed at `01fa8c1e`. Named GitHub CI
  checks were passing, with final aggregate jobs still in progress when the
  user asked for overnight continuity.
- Addressed additional Copilot feedback locally after `01fa8c1e`:
  - `claude-print.sh` now leaves prompt text on stdin instead of passing it as
    an argv argument to `claude -p`;
  - `ExecGateway` cleanup now uses structured `tracing::warn!` rather than
    `eprintln!`.
- Local verification for those follow-up edits passed:
  `cargo test -p calciforge proxy::exec_gateway --bin calciforge`,
  `cargo clippy -p calciforge --all-targets -- -D warnings`, shell syntax checks
  for exec-model wrappers, and `git diff --check`.
- Commit and push these two follow-up fixes next, then re-fetch PR #54 review
  threads. Expect many unresolved threads to be stale/outdated; only current
  non-outdated feedback should drive more code changes.
- Homebrew reports `docker 29.4.1` and `docker-compose 5.1.3` installed. Next
  step was to verify the Docker daemon/runtime and run
  `python3 scripts/matrix-real-e2e.py` locally.

## 2026-04-26 local Docker / Matrix E2E update

- Docker client was installed but no daemon was running. Installed Colima via
  Homebrew and started it with Docker runtime.
- Local Docker now reports server `29.2.1` on Ubuntu inside Colima.
- First local Matrix E2E run exposed Python 3.9 incompatibility:
  `TemporaryDirectory(ignore_cleanup_errors=...)` is unavailable. Added a
  compatibility helper that uses `ignore_cleanup_errors` on newer Python and
  falls back cleanly on Python 3.9.
- `python3 scripts/matrix-real-e2e.py` now passes locally against a real Synapse
  container on this Mac. It verified real registration/login, direct Matrix DM
  invite/join, command happy paths, `!switch`, `!default`, and CLI dispatch.
- Next deployment-readiness priority: audit actual Mac and `.210` configs and
  services for daily-driver correctness without printing secrets.

## 2026-04-26 late deployment hardening update

- PR #54 head `ac1ec7ed` had no unresolved review threads when rechecked.
  GitHub CI was green except aggregate jobs still finishing at the time.
- `.210` services were active and its gateway health endpoint responded, but
  `fnox` was still missing. Installed upstream `jdx/fnox` release `v1.23.0`
  from the x86_64 Linux tarball instead of compiling on the small VM; `fnox
  list` now succeeds.
- Patched `scripts/install.sh` so local Linux installs and remote node deploys
  prefer upstream fnox release tarballs before falling back to `cargo install`.
  This directly addresses the `.210` failure mode where compiling fnox can
  starve SSH and exhaust small-node resources.
- Added `api_key_file` to `[[agents]]` so gateway-backed channel agents can use
  bearer token files instead of inline tokens. `openclaw-http`,
  `openclaw-channel`, `openclaw-native`, `zeroclaw-http`, `zeroclaw-native`,
  and `zeroclaw` token resolution now all use the shared file/inline/env
  resolver appropriate to each adapter.
- Mac live deployment:
  - backed up `/Users/admin/.calciforge/config.toml`;
  - added a `gateway` agent pointing at `http://127.0.0.1:18083`, using the
    proxy API key file and default model `local-kimi-gpt55`;
  - made `brian` default and active agent `gateway`;
  - removed `{prompt}` from the live Claude exec provider args so prompts stay
    on stdin with the updated exec gateway;
  - validated the config, installed a fresh release build to
    `/Users/admin/.local/bin/calciforge`, restarted only
    `com.calciforge.calciforge`, and confirmed health.
- Smoke tests passed without printing secrets:
  - Mac local gateway authenticated `/v1/models` listed five models;
  - Mac `local-kimi-gpt55` chat completion returned the expected sentinel;
  - `.210` has `fnox 1.23.0`, health OK, four gateway models, and authenticated
    access to the Mac gateway;
  - `.210 local-kimi-gpt55` chat completion returned the expected sentinel.
- Docs status wording was softened from “solo-operator mature” to
  “solo-operator usable and actively hardening,” with explicit smoke-test
  expectations for new deployments.
- Verification after source edits: `cargo test -p calciforge --bin
  calciforge`, `cargo clippy -p calciforge --all-targets -- -D warnings`,
  `bash -n scripts/install.sh`, and `git diff --check`.

Remaining deployment follow-ups:

1. The old Mac `com.zeroclawed.*` launchd services are still present/running.
   Disable them once the user confirms no legacy path is still needed.
2. `claude-cli` as a channel-facing `kind = "cli"` agent still uses argv-based
   message passing if explicitly enabled. It is no longer in Brian's allowed
   agent list; prefer the exec-model/gateway path for Claude subscription use.

## 2026-04-27 .210 credential cleanup / user notification

- Sent one low-detail Telegram status message through the configured bot after
  generating the message via the local `local-kimi-gpt55` gateway route. The
  message intentionally contained no secrets or private config values.
- Moved `.210` `nonzeroclaw.service`'s inline `OPENAI_API_KEY` out of the
  systemd drop-in and into `/etc/calciforge/nonzeroclaw.env` with mode `0600`.
  The drop-in now uses `EnvironmentFile=/etc/calciforge/nonzeroclaw.env` and
  keeps only non-secret provider/model environment names inline.
- Restarted `nonzeroclaw.service`; it is active and its local health endpoint
  reports `status=ok`.

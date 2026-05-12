---
layout: default
title: Architecture Laws Action Plan
---

# Architecture Laws Action Plan

This plan turns the architecture review into ordered, testable work. The goal
is not cosmetic cleanup; each track removes a source of duplicated behavior,
surprising operator outcomes, or unclear ownership.

## Working Principles

- Prefer one policy or protocol decision point, with transport-specific code at
  the edges.
- Split large files by responsibilities that can be tested independently.
- Keep adapter and channel contracts explicit enough that operators can tell
  which paths are stable, legacy, experimental, or unsupported.
- Make each refactor land in small behavior-preserving slices with focused
  tests before changing user-visible behavior.

## 1. Channel Inbound Pipeline

Problem: SMS, Signal, and WhatsApp channel handlers repeat the same auth,
telemetry, inbound scan, command dispatch, agent dispatch, and reply flow.

Action:

- Extract a shared `InboundMessagePipeline` or `ChannelRuntime`.
- Keep channel-specific code responsible only for sender resolution,
  conversation key strategy, reply rendering/sending, and capabilities.
- Add one golden-path test per channel plus shared tests for scan, command, and
  dispatch ordering.

Gate:

- No channel loses existing command behavior.
- Shared tests prove commands are evaluated before agent dispatch and scanned
  text is what reaches downstream agents.

## 2. Command Handler Split

Problem: `commands.rs` owns parsing, help text, agent switching, model
selection, session state, approvals, rendering, and tests in one oversized
module.

Action:

- Move parser/token handling to `commands/parser.rs`.
- Move agent selection to `commands/agent_selection.rs`.
- Move model selection to `commands/model_selection.rs`.
- Move downstream sessions to `commands/sessions.rs`.
- Move security/approval commands to `commands/secure.rs` and
  `commands/approvals.rs`.
- Move user-facing response construction to `commands/render.rs`.

Gate:

- Existing command tests keep passing after each extraction.
- New module tests cover parser edge cases before behavior changes.

## 3. Security Proxy Policy Pipeline

Problem: `security-proxy` has separate HTTP proxy and MITM request paths that
duplicate URL/header/body substitution, provider browsing checks, preflight
checks, credential injection, outbound scanning, and inbound scanning.

Action:

- Introduce a shared `PolicyPipeline`.
- Return typed decisions such as `Allow`, `Block`, and `Strip`.
- Keep JSON vs HTML response shaping in the proxy/MITM transport edges.

Gate:

- One shared test matrix covers every blocking policy.
- Proxy and MITM transports map the same policy decision to their current
  response formats unless a deliberate behavior change is documented.

## 4. Installer Executor Split

Problem: the installer executor mixes orchestration, SSH command construction,
OpenClaw JSON patching, plugin deployment, service unit rendering,
systemd/launchd environment generation, hardening, CA trust, and rollback.
Some service rendering also overlaps shell scripts.

Action:

- Extract install orchestration to `install/pipeline.rs`.
- Extract OpenClaw plugin/config work to `install/openclaw.rs`.
- Extract service rendering to `install/systemd.rs` and `install/launchd.rs`.
- Extract security proxy setup to `install/security_proxy.rs`.
- Extract host hardening to `install/hardening.rs`.
- Consolidate service unit rendering so scripts and Rust do not drift.

Gate:

- Existing install tests pass after each module split.
- Package smoke workflows still exercise `calciforge-secrets`, service file
  generation, and OpenClaw plugin metadata.

## 5. Adapter Lifecycle Cleanup

Problem: supported adapter kinds have grown faster than their lifecycle
documentation. Stable, legacy, experimental, and test-only paths can look
equivalent to operators.

Action:

- Maintain adapter lifecycle metadata in the shared agent-kind registry.
- Surface legacy and experimental kinds in `calciforge doctor`.
- Hide unsupported/test-only paths from public factory/config docs.
- Convert candidate integrations to recipes or orchestrators unless they need a
  first-class adapter.

Gate:

- The factory, doctor, docs, and tests agree on known adapter kinds.
- New adapter proposals must declare lifecycle state and graduation criteria.

## 6. Adapter Doctor Hooks

Problem: `calciforge doctor` can validate Calciforge config and generic
endpoint reachability, but first-class agents often have their own runtime
doctor/status commands. A configured OpenClaw gateway can expose the Calciforge
channel route while still failing its own model/runtime checks; a ZeroClaw
endpoint can accept TCP while its daemon, scheduler, or channel freshness state
is unhealthy.

Action:

- Add an adapter-level doctor contract with a default no-op/generic
  implementation.
- Keep generic checks in Calciforge: readable tokens, endpoint URL shape,
  endpoint reachability, model override support, and session capability shape.
- Let first-class adapters add native subdoctor probes:
  - OpenClaw: prefer structured commands such as `openclaw models status --json`
    for model/auth checks, and attach `openclaw doctor` text output as context
    unless a future JSON mode exists.
  - ZeroClaw: use machine-readable exit-code/status modes where available, then
    keep richer human doctor output as context rather than parsing decorative
    terminal text.
- For remote first-class agents, run native subdoctor probes only when install
  metadata maps the endpoint to a managed node; otherwise report that only the
  HTTP/plugin surface was checked.
- Add a separate `calciforge doctor --fix` or `calciforge repair` mode for
  safe idempotent repairs. Plain doctor must keep detecting and reporting drift
  without mutating services.

Gate:

- A first-class adapter with a broken native runtime produces a doctor error or
  clear warning even when its HTTP endpoint accepts TCP.
- Plain doctor never parses human-only decorative output for pass/fail.
- Repair mode is opt-in and limited to Calciforge-managed services/config.

## Execution Order

1. Add lifecycle metadata and doctor warnings for adapters.
2. Extract the shared channel inbound pipeline.
3. Split `commands.rs` behind passing tests.
4. Unify security proxy policy decisions.
5. Split installer executor modules and consolidate service rendering.
6. Add adapter doctor hooks and an explicit repair mode.

The first slice is intentionally small: adapter lifecycle metadata creates a
durable place to record support state before pruning or reshaping adapter
surfaces.

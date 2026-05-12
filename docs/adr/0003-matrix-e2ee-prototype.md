---
layout: default
title: "ADR 0003: Matrix E2EE Prototype"
---

# ADR 0003: Matrix E2EE Prototype

Status: Experimental branch note

Date: 2026-05-12

## Context

Calciforge's Matrix channel currently calls the Matrix Client-Server API
directly with `reqwest`: `/sync`, `/send`, `/join`, media upload, and a small
state check for `m.room.encryption`. That keeps the adapter small, but it cannot
decrypt `m.room.encrypted` events or encrypt replies.

The current Matrix Rust SDK documentation says E2EE support is behind the
`e2e-encryption` feature, persistent E2EE state can use SQLite, and the SDK can
set up an SQLite store from `ClientBuilder`. The docs also call out the hard
parts Calciforge must not hand-wave: device/session identity, room keys,
persistent key storage, restoration, and verification policy.

## Feasibility Result

The older `matrix-sdk = 0.16` dependency still fails in this workspace when
compiled locally:

```text
error: queries overflow the depth limit
```

Updating the optional dependency to `matrix-sdk = 0.17` and compiling it behind
`--features channel-matrix-e2ee` succeeds on this machine with bundled SQLite.
That makes native Matrix E2EE feasible enough for a real prototype.

## Decision

Do not rewrite the production Matrix channel in this branch.

Instead:

- add `channel-matrix-e2ee` as an opt-in Cargo feature;
- add a small `matrix_e2ee` SDK builder probe beside the channel;
- add config fields for E2EE policy and persistent SDK store path;
- make `matrix_e2ee = "require"` and `matrix_e2ee = "experimental-sdk"` fail
  closed instead of silently using the plaintext runtime;
- keep `matrix_e2ee = "warn"` as the default while E2EE is incomplete.

## Follow-Up Plan

1. Replace raw `/sync` with an SDK-backed sync loop that preserves Calciforge's
   existing identity, routing, pending-choice, session, approval, and artifact
   behavior.
2. Restore sessions from access-token plus device ID, or provide a login flow
   that persists device identity so keys remain decryptable after restart.
3. Decide trust policy: permissive encryption with unverified devices, strict
   verified-device mode, or an operator-configurable mode with clear warnings.
4. Send text and artifacts through SDK room APIs so encrypted rooms get
   encrypted replies and encrypted media metadata.
5. Add a disposable homeserver integration test with an encrypted room, a bot
   account, a user account, restart behavior, and at least one key-loss case.

## Open Questions

- How should Calciforge expose Matrix device verification to an operator over
  text-only channels?
- Should Matrix E2EE be available in Docker by default, or only in a heavier
  image with SQLite crypto storage enabled?
- How should bridge-heavy Matrix setups behave when they cannot preserve E2EE
  semantics end to end?

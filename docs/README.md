---
layout: default
title: Documentation Tree
---

# Documentation Tree

This directory is for user-facing and maintainer-facing documentation
that should be reasonably stable:

- `index.md` — GitHub Pages feature tour
- `agent-runtime-contract.md` — how agents learn Calciforge CLI, optional MCP,
  artifact, proxy, and future API surfaces
- `agent-handoff-checklist.md` — maintainer checklist for resuming
  agent-authored branches and reviews
- `agent-adapters.md` — agent adapter selection and evaluation notes
- `agents.md` — agent backends, identities, and routing rules
- `agent-adapters.md` also covers secured recipes, artifact-producing
  CLI integrations, and the early orchestrator support model for async
  work systems.
- `model-gateway.md` — model gateway reference
- `security-gateway.md` — outbound proxy and scanning reference
- `packaging.md` — source, Homebrew, Docker, and release archive install paths
- `staging-test-matrix.md` — local, CI, staging, and release-candidate test tiers
- `MANUAL_INSTALL.md`, `OPS-HARDENING.md`, setup guides — operator docs
- `rfcs/` — durable design proposals
- `roadmap/` — public future-work notes
  - `roadmap/agent-recipes-orchestrators.md` — future support for secured
    recipes, richer artifacts, and async orchestrator backends
  - `roadmap/architecture-laws-action-plan.md` — refactor plan for channel
    pipelines, command handling, security proxy policy, installer structure,
    and adapter lifecycle cleanup

## Status labels

Every durable design, roadmap, or reference page should declare one of these
labels near the top:

- **Implemented** — the behavior exists in code and is expected to work.
- **Experimental** — implemented or partially implemented, but still subject to
  interface or operational changes.
- **Design sketch** — planning material; not a commitment that code exists.
- **Deprecated** — retained for history or migration guidance; do not build new
  work on it.

If a document mixes shipped behavior and future work, label the document with
the highest-risk status and call out the implemented subset explicitly.

Manual candidate-adapter smoke checks live in
`scripts/agent-recipe-smoke.sh`. They install npcsh, OmO/oh-my-opencode, and
Gas Town in disposable Docker containers to verify current CLI surfaces before
turning a recipe into first-class support.

Internal reviews, audit scratchpads, vendor comparisons, and session planning
notes should stay outside the public documentation tree.

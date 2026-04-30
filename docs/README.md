# Documentation Tree

This directory is for user-facing and maintainer-facing documentation
that should be reasonably stable:

- `index.md` — GitHub Pages feature tour
- `agent-adapters.md` — agent adapter selection and evaluation notes
- `agent-adapters.md` also covers secured recipes, artifact-producing
  CLI integrations, and the early orchestrator support model for async
  work systems.
- `model-gateway.md` — model gateway reference
- `security-gateway.md` — outbound proxy and scanning reference
- `staging-test-matrix.md` — local, CI, staging, and release-candidate test tiers
- `MANUAL_INSTALL.md`, `OPS-HARDENING.md`, setup guides — operator docs
- `rfcs/` — durable design proposals
- `roadmap/` — public future-work notes
  - `roadmap/agent-recipes-orchestrators.md` — future support for secured
    recipes, richer artifacts, and async orchestrator backends

Manual candidate-adapter smoke checks live in
`scripts/agent-recipe-smoke.sh`. They install npcsh, OmO/oh-my-opencode, and
Gas Town in disposable Docker containers to verify current CLI surfaces before
turning a recipe into first-class support.

Internal reviews, audit scratchpads, vendor comparisons, and session
planning notes live under [`../research/`](../research/) instead.

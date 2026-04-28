# Staging Test Matrix (Local + GitHub Actions + Cloud)

This document defines a practical, security-first test pyramid for Calciforge.
It complements existing CI by adding realistic staging exercises for releases.

## Goals

- Keep PR feedback fast.
- Exercise realistic multi-service and failure scenarios before release.
- Validate host-agent safety controls under conditions closer to production.
- Preserve reproducible evidence (logs, metrics, artifacts) for each release candidate.

## Test Tiers

### Tier 1 — Local Developer Loop (minutes)

Purpose: fastest iteration while building features/fixes.

Run:

- `cargo test -p <crate>` for touched crates.
- `scripts/manual-docker-test.sh` for the multi-service smoke stack.
- Optional targeted e2e tests with mocked dependencies.

Scope:

- Regression checks for path routing, tool passthrough, config validation.
- Developer-owned troubleshooting with live logs.

Notes:

- Uses `scripts/docker-compose.yml` stack (`mock-llm`, `calciforge`).
- Best for quick reproduction, not final release confidence.

### Tier 2 — GitHub Actions PR Gates (minutes)

Purpose: mandatory correctness gates with high signal and low flake.

Current fit:

- Formatting + clippy.
- Per-crate test matrix.
- Loom concurrency tests.
- Real Matrix DM E2E with disposable Synapse (`scripts/matrix-real-e2e.py`).
- Synthetic model gateway E2E for alloys, cascades, dispatchers, and exec-backed
  model leaves (`scripts/model-gateway-synthetic-e2e.py`).
- Docker model-gateway smoke and installer-adjacent coverage.
- Workspace build/tests with selected live-style tests skipped in CI (`SKIP_LIVE_TESTS=1`).

Scope:

- Deterministic checks suitable for shared runners.
- Prevents regressions from merging to main.

### Tier 3 — Cloud Staging Nightly (30-90 minutes)

Purpose: realism and hardening beyond hosted CI limits.

Automation:

- `.github/workflows/staging-nightly.yml` runs on a schedule and by manual
  dispatch. It currently exercises the Docker model-gateway smoke stack, real
  Matrix DM E2E, and synthetic model gateway E2E.

Recommended environment:

- Dedicated VM(s) or self-hosted runner(s) with Docker available.
- Ability to run privileged/networked scenarios.
- Isolated secrets and disposable test identities.

Suggested scenarios:

1. **Full stack boot + health convergence**
   - Bring up compose stack and assert all health endpoints.
   - Run `calciforge doctor` against the deployed config and retain the summary.
2. **Failure injection**
   - Restart individual services; verify graceful retries and bounded failures.
3. **mTLS/cert lifecycle**
   - Expired cert, wrong CA, revoked cert, and rotation flow checks.
4. **Approval workflow safety**
   - Validate destructive operations require approvals and are audit logged.
5. **Policy enforcement**
   - Verify deny-by-default + route/destination controls under adversarial input.
6. **Load/soak smoke**
   - Sustained request traffic for leak/latency regression detection.

Artifacts to retain:

- Structured logs (`calciforge`, `security-proxy`, `host-agent`).
- Test reports (JUnit or JSON summary).
- Metrics snapshots and latency histograms.
- `calciforge doctor` summaries with secret values redacted.
- Final compose service state and exit codes.

### Tier 4 — Release Candidate Staging (pre-release)

Purpose: strict release blocker on production-like checks.

Requirements:

- Run Tier 3 suite on release candidate commit/tag.
- Include long-running soak (e.g., 4-12h) for critical paths.
- Security signoff: approvals, audit integrity, policy behavior, and rollback drill.

Release gate policy:

- Any Tier 4 failure blocks release.
- Attach artifacts to release record.

## Persistence and Reuse Strategy

Persist across runs:

- Build caches and container layers.
- Deterministic fixtures and synthetic datasets.
- Golden traces/baselines for trend comparison.

Reset per run:

- Runtime state and mutable volumes used for correctness assertions.
- Approval token state and ephemeral auth material.

Rationale: keep speed benefits without hiding stateful bugs.

## Suggested Automation Map

- **On every PR:** Tier 2.
- **Nightly on `main`:** Tier 3.
- **On release branch / tag:** Tier 4.
- **Manual trigger:** Tier 3 rerun for incident/backport validation.

## Immediate Next Steps

1. Add more failure-injection cases to the nightly Docker smoke stack beyond
   the default security-proxy scanner-block assertion.
2. Add release-candidate soak mode with longer runtime and retained latency
   histograms.
3. Add explicit security assertions for approval/audit invariants and outbound
   scanner policy behavior.
4. Extend artifact retention beyond Docker logs and JSONL summaries once metrics
   export is stable.

# AGENTS.md — ZeroClawed Host-Agent

## Project
Build a Rust host-agent for safe RPC access to host resources (ZFS, systemd, PCT) using Unix permissions as the enforcement layer.

## Coding Standards

### Security-First
- Unix permissions do enforcement, code only orchestrates
- Fail-closed design: component death = safe state
- No custom permission logic — rely on `zfs allow`, `sudo`, Unix groups
- Audit everything to append-only logs

### SDD Workflow
Follow Spec-Driven Development phases:
1. **Discovery** — Read spec, explore existing code
2. **Specification** — Define interfaces and data structures
3. **Architecture** — Design module relationships
4. **Implementation** — Write code matching spec
5. **Verification** — Test and validate

### Rust Standards
- Use `anyhow` for error handling
- Structured logging with `tracing`
- Tokio async runtime
- Axum for HTTP/mTLS
- No `unsafe` code
- All async functions must be `Send + Sync`

### Documentation
- Every module has module-level docstring
- Complex functions have inline comments explaining WHY
- Link to spec requirements in code comments

## Architecture

### Components
1. **mTLS Server** — Client cert auth, CN → Unix user
2. **ZFS Handler** — Snapshot/list (delegated), destroy (gated)
3. **Approval Manager** — Token generation, validation, TTL
4. **Audit Logger** — Structured JSONL to `/var/log/clash/`

### Security Model
- Host-agent runs as `clash-agent` (unprivileged)
- ZFS: `zfs allow` delegation (snapshot only)
- Destructive ops: require approval token + sudo
- Audit: append-only with `chattr +a`

## Spec Reference
`/root/.openclaw/workspace/specs/ZEROCLAWED-V3-HOST-AGENT-SPEC.md`

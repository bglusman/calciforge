# PolyClaw Host-Agent — Phase 2 TODO

## Signal Integration

- [ ] Signal webhook receiver endpoint (`POST /webhooks/signal`)
- [ ] Parse Signal messages for "CONFIRM <token>" pattern
- [ ] Map Signal sender to approval tokens
- [ ] Send Signal notifications on pending approvals
- [ ] Configurable Signal gateway endpoint

## systemd Operations

- [ ] `POST /systemctl/restart` endpoint
- [ ] `POST /systemctl/stop` endpoint
- [ ] `POST /systemctl/status` endpoint
- [ ] Service allowlist/blocklist configuration
- [ ] Approval gating for critical services

## PCT (Proxmox Container) Operations

- [ ] `POST /pct/start` endpoint
- [ ] `POST /pct/stop` endpoint
- [ ] `POST /pct/status` endpoint
- [ ] Container allowlist configuration

## Enhanced Security

- [ ] Rate limiting per client CN
- [ ] Operation timeouts and cancellation
- [ ] Configurable approval TTL per operation type
- [ ] Certificate revocation checking (CRL/OCSP)
- [ ] Mutual auth with pre-shared key fallback

## Observability

- [ ] Prometheus metrics endpoint (`/metrics`)
- [ ] Health check with dependency status
- [ ] Structured operation tracing
- [ ] Alerting on failed operations

## Policy Engine

- [ ] Time-based restrictions (maintenance windows)
- [ ] Dataset pattern matching rules
- [ ] Quota-aware operations
- [ ] Dry-run mode for testing

## Client SDK

- [ ] Rust client library
- [ ] Python bindings
- [ ] CLI tool for testing/admin

## Testing

- [ ] Property-based tests for validation
- [ ] Load testing with concurrent clients
- [ ] Chaos testing (cert expiry, network loss)
- [ ] Security audit and fuzzing

## Documentation

- [ ] Architecture decision records (ADRs)
- [ ] Security runbook
- [ ] Incident response procedures
- [ ] Certificate rotation guide

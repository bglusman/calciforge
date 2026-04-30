# Calciforge Backlog

## 🔥 HIGH PRIORITY — Active Work

### Claw-Code Integration
- [ ] Install claw-code on 210 via deploy script
- [ ] Configure Calciforge security proxy for claw-code credentials
- [ ] Create wrapper script: `claw-wrapped` → routes through Calciforge security proxy
- [ ] Test end-to-end: Telegram → calciforge → claw-code → security proxy → provider
- [ ] Document claw-code integration in `docs/claw-code-setup.md`

### ZeroClaw Integration
- [ ] Install zeroclaw on 210 via deploy script (`--with-zeroclaw`)
- [ ] Configure zeroclaw gateway URL to use Calciforge security proxy
- [ ] Create wrapper script: `zeroclaw-wrapped` → routes through Calciforge security proxy
- [ ] Test: Telegram → calciforge → zeroclaw → security proxy → provider
- [ ] Document zeroclaw integration

### Security Proxy + Agent Wrapper Layer
- [ ] Finish fnox-backed secret discovery and substitution across wrappers
- [ ] Configure clash policy for agent tool restrictions
- [ ] Create unified wrapper generation in `calciforge install`
- [ ] Test policy enforcement: block dangerous tools, allow safe ones

### Deployment & Infrastructure
- [ ] Run deploy-210.sh with agents enabled
- [ ] Verify services start cleanly on 210
- [ ] Health check all endpoints
- [ ] Monitor logs for errors

---

## 📋 MEDIUM PRIORITY — Next Up

### Message Batching (from earlier Calciforge prototypes)
- [ ] Implement message buffer per chat/identity
- [ ] While agent processing: accumulate new messages
- [ ] Concatenate with separator (`\n---\n`)
- [ ] Add optional flush delay (e.g., 500ms for rapid-fire DMs)
- [ ] Detect "agent busy" state (in-flight request tracking)
- [ ] Single dispatch with combined context
- **Use case:** operator multi-message DMs with corrections/additions

### Channel Security Gate
- [ ] Evolve scanner checks into a configurable channel MitM gate
- [ ] Intercept inbound messages before agent sees them
- [ ] Filter/group chat messages from untrusted participants
- [ ] Prevent injection attacks, content policy violations
- [ ] Config per-channel: `scan_inbound`, `scan_outbound`, `on_unsafe`
- [x] Add low-latency declarative scanner checks: regexes, keyword lists,
      and size limits
- [x] Build a starter library of editable Starlark scanner policies for common
      operator concerns such as allowed destinations, command denylists, and
      high-risk credential language
- [ ] Evaluate sandboxed WebAssembly scanner checks for arbitrary in-process
      custom logic with fuel, memory limits, and no ambient filesystem/network

### Host-Agent Phase 2
- [ ] Signal webhook receiver for approval confirmations
- [ ] systemd operations (restart/stop/status) with approval gating
- [ ] PCT (Proxmox) operations
- [ ] Rate limiting per client CN
- [ ] Prometheus metrics endpoint

---

## 🔮 LOW PRIORITY — Future Ideas

### Security Hardening
- [ ] Certificate revocation checking (CRL/OCSP)
- [ ] Mutual auth with PSK fallback
- [ ] Security audit and fuzzing
- [ ] Chaos testing (cert expiry, network loss)

### Developer Experience
- [ ] Rust client SDK for host-agent
- [ ] Python bindings
- [ ] CLI admin tool
- [ ] Explore a local web channel for desktop/LAN testing that uses the same
      identity, routing, message-envelope, artifact, and proxy policy paths as
      Telegram, Matrix, and text channels
- [ ] Architecture decision records (ADRs)

### Observability
- [ ] Structured operation tracing
- [ ] Alerting on failed operations
- [ ] Security runbook
- [ ] Incident response procedures

---

## ✅ COMPLETED (Recently)

- [x] Remove vendored zeroclaw crate (use upstream)
- [x] Remove robot-kit, aardvark-sys (use upstream)
- [x] Remove local clash (use crates.io)
- [x] Update deps: zeroclaw 0.6.8, clash 0.6.2
- [x] Sanitize deploy scripts (move to infra/, gitignore)
- [x] Git history filter to remove secrets/artifacts
- [x] CI cleanup (remove zeroclaw from CI matrix)

---

## Notes

**Claw-code repo:** https://github.com/instructkr/claw-code  
**ZeroClaw repo:** https://github.com/zeroclaw-labs/zeroclaw  
**Deploy target:** local operator inventory lives outside the public repo
**Local scripts:** `infra/` (gitignored, not in repo)

**Integration architecture:**
```
User DM → calciforge → [security proxy] → [clash policy] → claw-code/zeroclaw → Provider
```

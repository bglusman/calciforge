# ZeroClawed Backlog

## 🔥 HIGH PRIORITY — Active Work

### Claw-Code Integration
- [ ] Install claw-code on 210 via deploy script
- [ ] Configure OneCLI proxy for claw-code credentials
- [ ] Create wrapper script: `claw-wrapped` → routes through OneCLI + clash
- [ ] Test end-to-end: Telegram → zeroclawed → claw-code → OneCLI → provider
- [ ] Document claw-code integration in `docs/claw-code-setup.md`

### ZeroClaw (zeroclawlabs) Integration  
- [ ] Install zeroclawlabs on 210 via deploy script (`--with-zeroclaw`)
- [ ] Configure zeroclawlabs gateway URL to use OneCLI proxy
- [ ] Create wrapper script: `nzc-wrapped` → routes through OneCLI + clash
- [ ] Test: Telegram → zeroclawed → nzc → OneCLI → provider
- [ ] Document zeroclaw integration

### OneCLI + Clash Adapter Layer
- [ ] Build `onecli-client` credential proxy service
- [ ] Configure clash policy for agent tool restrictions
- [ ] Create unified wrapper generation in `zeroclawed install`
- [ ] Test policy enforcement: block dangerous tools, allow safe ones

### Deployment & Infrastructure
- [ ] Run deploy-210.sh with agents enabled
- [ ] Verify services start cleanly on 210
- [ ] Health check all endpoints
- [ ] Monitor logs for errors

---

## 📋 MEDIUM PRIORITY — Next Up

### Message Batching (from PolyClaw v2)
- [ ] Implement message buffer per chat/identity
- [ ] While agent processing: accumulate new messages
- [ ] Concatenate with separator (`\n---\n`)
- [ ] Add optional flush delay (e.g., 500ms for rapid-fire DMs)
- [ ] Detect "agent busy" state (in-flight request tracking)
- [ ] Single dispatch with combined context
- **Use case:** Brian's multi-message DMs with corrections/additions

### Outpost Channel Gate ("Sentinel")
- [ ] Evolve outpost into configurable channel MitM gate
- [ ] Intercept inbound messages before agent sees them
- [ ] Filter/group chat messages from untrusted participants
- [ ] Prevent injection attacks, content policy violations
- [ ] Config per-channel: `scan_inbound`, `scan_outbound`, `on_unsafe`

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
- [ ] Architecture decision records (ADRs)

### Observability
- [ ] Structured operation tracing
- [ ] Alerting on failed operations
- [ ] Security runbook
- [ ] Incident response procedures

---

## ✅ COMPLETED (Recently)

- [x] Remove vendored nonzeroclaw crate (use upstream)
- [x] Remove robot-kit, aardvark-sys (use upstream)
- [x] Remove local clash (use crates.io)
- [x] Update deps: zeroclawlabs 0.6.8, clash 0.6.2
- [x] Sanitize deploy scripts (move to infra/, gitignore)
- [x] Git history filter to remove secrets/artifacts
- [x] CI cleanup (remove nonzeroclaw from matrix)

---

## Notes

**Claw-code repo:** https://github.com/instructkr/claw-code  
**ZeroClaw repo:** https://github.com/zeroclaw-labs/zeroclaw  
**Deploy target:** 192.168.1.210 (CT on Proxmox)  
**Local scripts:** `infra/` (gitignored, not in repo)

**Integration architecture:**
```
User DM → zeroclawed → [OneCLI proxy] → [clash policy] → claw-code/nzc → Provider
```

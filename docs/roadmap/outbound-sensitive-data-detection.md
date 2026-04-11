# Outbound Sensitive Data Detection

**Status:** Research / Future Roadmap  
**Priority:** Medium  
**Depends on:** Channel interception layer (optional)

## Problem Statement

When agents send outbound messages (responses to users), they may inadvertently include:
- API keys or access tokens
- Private credentials or passwords
- Internal URLs or infrastructure details
- PII (names, emails, phone numbers, SSNs)
- High-entropy secrets (JWTs, database connection strings)

Current implementation removed outbound scanning from the adversary-detector crate to simplify the initial channel integration. This document captures the research directions for re-implementing outbound content filtering.

## Detection Approaches

### 1. High Entropy Detection
- **Technique:** Shannon entropy calculation on strings
- **Thresholds:** >4.5 bits/char for base64, >5.5 for hex
- **Pros:** Catches unknown secret formats, fast
- **Cons:** False positives on compressed data, random IDs, UUIDs
- **Mitigation:** Combine with pattern matching, allowlist common formats

### 2. Regex Pattern Matching
- **API Keys:** `sk-[a-zA-Z0-9]{32,}`, `AKIA[0-9A-Z]{16}`
- **Tokens:** `eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*` (JWT)
- **Private Keys:** `-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----`
- **Connection Strings:** `mongodb(\+srv)?://`, `postgres://`, `mysql://`
- **Credit Cards:** Luhn-validated 13-19 digit sequences
- **SSNs:** `\d{3}-\d{2}-\d{4}` (with contextual keywords)

### 3. Regret Matches
- **Concept:** "I regret including..." — contextual patterns
- **Examples:**
  - `"my password is"`, `"password:"` + high-entropy string
  - `"api key:"`, `"token:"`, `"secret:"` + following value
  - `"don't share this"`, `"private:"` + content
- **Pros:** High precision when context is clear
- **Cons:** Misses secrets without contextual hints

### 4. Machine Learning Classifiers
- **Approach:** Fine-tuned transformer for secret detection
- **Training Data:** GitHub secret scanning public dataset
- **Pros:** Generalizes to new secret types
- **Cons:** Latency, compute cost, false positive tuning

### 5. Dictionary/Allowlist Approach
- **Blocklist:** Known dangerous patterns (private IP ranges, localhost URLs)
- **Allowlist:** Safe patterns (public docs, example.com)
- **Greylist:** Flag for review (internal hostnames, VPN IPs)

## Implementation Design

### Configuration
```yaml
security:
  outbound_scanning:
    enabled: true
    mode: "flag"  # "block", "flag", "log_only"
    detectors:
      high_entropy:
        enabled: true
        min_entropy: 4.5
        min_length: 16
      patterns:
        enabled: true
        patterns_file: "secrets-patterns.json"
      context_keywords:
        enabled: true
        keywords: ["password", "secret", "token", "key", "credential"]
    redaction:
      enabled: true
      mask: "***REDACTED***"
    alerts:
      on_detection: true
      channel: "signal"
      to: "+1XXXXXXXXXX"
```

### Integration Points

1. **Channel Layer** (ZeroClawed)
   - Scan agent responses before transmission
   - Configurable per-channel (DMs vs groups)
   - Respect user trust levels

2. **Tool Result Layer** (OpenClaw)
   - Continue scanning tool outputs (existing)
   - Extend to tool inputs (prevent exfiltration)

3. **Policy Integration** (clash)
   - Policy rule: `outbound_contains_secrets → block`
   - Audit logging for compliance

## Open Questions

1. **Performance:** Can we scan without adding >100ms latency to responses?
2. **Context Awareness:** Should trusted identities (owner) bypass scanning?
3. **Redaction vs Blocking:** Redact and send, or block and alert?
4. **Learning:** Should the system learn from false positive reports?
5. **Scope:** Just agent responses, or also tool call arguments?

## Related Work

- **GitHub Secret Scanning:** 100+ partner patterns, public dataset
- **Gitleaks:** Open-source secret scanner, Go-based, fast
- **TruffleHog:** Entropy + regex, enterprise-grade
- **AWS Macie:** ML-based PII detection for S3

## Next Steps

1. **Research Phase:**
   - Evaluate gitleaks/trufflehog patterns for Rust port
   - Test entropy thresholds on real agent outputs
   - Survey: what secrets have leaked in practice?

2. **Prototype Phase:**
   - Implement entropy + regex scanner
   - Test on 1000+ agent responses for false positive rate
   - Build configuration schema

3. **Integration Phase:**
   - Wire into channel transmission pipeline
   - Add to clash policy engine
   - Deploy behind feature flag

## Risks

- **False positives:** Blocking legitimate content frustrates users
- **Privacy:** Scanner sees all outbound content — audit logging must be minimal
- **Evasion:** Attackers may obfuscate secrets (base64, rot13, character substitution)

## References

- [Gitleaks Patterns](https://github.com/gitleaks/gitleaks/blob/master/config/gitleaks.toml)
- [OWASP Secrets Management Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html)
- [Shannon Entropy in Secret Detection](https://www.kdnuggets.com/2023/02/detecting-secrets-source-code-using-shannon-entropy.html)

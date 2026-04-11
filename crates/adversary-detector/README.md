# adversary-detector

Adversary external content scanning for ZeroClawed. Protects agents from prompt injection, hidden payloads, and malicious web content before it reaches the model context.

## How It Works

All external content access goes through `AdversaryDetector::fetch()`:

```
URL → fetch → SHA-256 digest → cache check → verdict
                     │                              │
                cache hit?                    run scanner
                return cached                (layer 1→2→3)
                verdict (no
                rescan)
```

### Digest-Based Caching

The detector stores `(URL → SHA-256(content)) → verdict` entries. This protects against:

- **Gist/CDN poisoning:** Server serves clean content first, then swaps to malicious. Digest changes → rescan triggered.
- **Cache poisoning attacks:** Same URL, different content = different hash = fresh scan.
- **Static content efficiency:** Same URL, same content = cached verdict, no rescan.

```rust
// First fetch: full scan, verdict persisted
let result = detector.fetch("https://example.com/article").await;

// Second fetch, same content: cache hit, no rescan
let result = detector.fetch("https://example.com/article").await;

// Server changes content: different digest → rescanned
// (happens automatically, no caller action needed)
```

### Human Overrides

```rust
// Mark a URL+digest as human-approved
detector.mark_override(url, &digest).await;

// Future fetches with same digest bypass Blocked verdicts
// If content changes (different digest), override does NOT apply
// (new content = fresh scan, human must re-approve)
```

## Three-Layer Scanning Pipeline

| Layer | What it detects | Mechanism |
|-------|----------------|-----------|
| **Layer 1 — Structural** | Zero-width chars, unicode tags, CSS hiding, base64 blobs | Regex patterns |
| **Layer 2 — Semantic** | Prompt injection phrases, PII harvesting, exfiltration signals | Aho-Corasick + regex, with discussion-context heuristic |
| **Layer 3 — Remote** | Deeper analysis via shared HTTP service (optional) | HTTP POST to adversary service |

Layer 1 and 2 run locally. Layer 3 is optional and non-blocking — if the service is unreachable, L1+L2 results stand.

### Discussion Context Heuristic

Content that is *about* prompt injection (security research, blog posts, CVE analysis) is downgraded from `Unsafe` → `Review`. The heuristic uses a configurable ratio of `discussion_signals / injection_signals`.



### Skip Protection (Trusted Domains)

Domains listed in `skip_protection_domains` bypass scanning entirely — content is fetched and returned as-is with a `Clean` verdict. Use for:

- **Trusted internal domains** — your own APIs, dashboards, documentation sites
- **Controlled testing** — deterministic behavior for CI/CD pipelines
- **Known-safe CDNs** — static asset hosts you trust completely

```rust
let config = ScannerConfig {
    skip_protection_domains: vec![
        "api.internal.example.com".into(),   // exact match
        "*.trusted-cdn.com".into(),            // wildcard: all subdomains
    ],
    ..Default::default()
};
```

| Pattern | Matches | Does not match |
|---------|---------|----------------|
| `example.com` | `https://example.com/path` | `https://sub.example.com/path` |
| `*.example.com` | `https://example.com/path`, `https://sub.example.com/path` | `https://example.org/path` |

**Note:** skip_protection bypasses ALL layers of scanning. Only use for domains you fully control or explicitly trust. For domains where you want content cached after a clean scan (but still rescanned if content changes), use digest caching instead.

## Security Profiles

Four named presets for installation:

| Profile | Scans | Discussion Ratio | Review | Rate | Logging |
|---------|-------|-----------------|--------|------|---------|
| **Open** | web_fetch only | 0.5 (permissive) | auto-pass | 120/min | minimal |
| **Balanced** | web + search | 0.3 | needs approval | 60/min | standard |
| **Hardened** | all tools | 0.15 | blocked | 30/min | verbose |
| **Paranoid** | all + exec | 0.0 (never downgrade) | blocked | 15/min | trace |

```rust
use adversary_detector::{SecurityConfig, SecurityProfile};

let config = SecurityConfig::from_profile(SecurityProfile::Balanced);
let detector = AdversaryDetector::from_config(config.scanner, logger, rate_limit).await;
```

## Verdicts

| Verdict | Meaning | Default behavior |
|---------|---------|-----------------|
| `Clean` | No threats detected | Content passed through |
| `Review` | Ambiguous — needs judgment | Content annotated with warning |
| `Unsafe` | Threat detected | Content blocked, reason returned |

## Modules

- **`proxy`** — Transparent HTTP proxy with digest caching and human overrides
- **`scanner`** — Three-layer content inspection pipeline
- **`middleware`** — Intercepts tool results before they reach the model
- **`patterns`** — Compiled regex and Aho-Corasick pattern sets
- **`digest`** — Persistent URL+hash → verdict store
- **`verdict`** — Verdict types and scan context
- **`profiles`** — Named security presets (open/balanced/hardened/paranoid)
- **`audit`** — Structured logging of all security decisions

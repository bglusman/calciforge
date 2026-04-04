
---

## Channel Intercept / Content Gate ("Censor" / "Sentinel" / TBD)

**Origin:** Brian, 2026-03-31

### Concept
Evolve the outpost scanning layer into a configurable channel MitM (man-in-the-middle) gate that can intercept, inspect, and optionally modify/block messages *before* they reach the agent — not just tool results after the fact.

### Use cases
- **Group chats:** Filter inbound messages from untrusted participants before the agent sees them (injection prevention, content policy)
- **DM chats:** Same, for scenarios where a device might be accessed by untrusted users (shared iPad, unlocked phone, etc.)
- **Outbound filtering:** Optionally gate agent *replies* too — prevent accidental PII leakage or policy violations in responses

### How it differs from current outpost
Current outpost (outpost-lite at 127.0.0.1:9800) scans *tool result content* — it's post-LLM-call, not pre-channel. This new layer would sit at the channel boundary, before the message ever enters the agent loop.

### Architecture sketch
```
[Channel inbound] → [Content Gate] → [Policy check] → [Agent loop]
                         │
                    [Outpost scan]
                    [Regex rules]
                    [Trust level]
                    [Configurable actions: pass / annotate / redact / block]
```

Config would live in ZeroClawed config.toml per-channel or per-identity:
```toml
[[channels.intercept]]
enabled = true
scope = ["group", "dm"]  # or just ["group"]
scan_inbound = true
scan_outbound = true
on_unsafe = "block"       # block | annotate | redact | ask-agent
on_review = "annotate"
```

### Naming thoughts
- "Censor" — accurate but negative connotation
- "Sentinel" — guards the gate, neutral
- "Gatekeeper" — self-explanatory
- "Channel Guard" / "ChanGuard" — descriptive
- Leave as "outpost" with a `channel_mode` flag — consistent naming

### Priority
Low — v3 planning item. No implementation needed yet. Document and revisit when ZeroClawed channel layer is stable.

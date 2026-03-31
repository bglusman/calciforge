# Typo Detection & Handling — Design Notes

_Captured 2026-03-30. Optional feature, not in current sprint._

---

## Problem

LLMs handle typos reasonably well via emergent behavior, but mobile keyboard autocomplete creates systematic, hard-to-predict errors that differ qualitatively from desktop typos. The same user on mobile vs. desktop sends structurally different input. There's currently no way to signal this to the agent.

---

## Design Layers (from simple to complex)

### Layer 1: Channel input_device metadata (low-risk, high value)

Add optional `input_device` hint to PolyClaw channel config:

```toml
[channels.signal_mobile]
# ... channel config ...
input_device = "mobile"   # "mobile" | "desktop" | "unknown" (default)

[channels.telegram_desktop]
input_device = "desktop"
```

PolyClaw passes this as envelope metadata to downstream claws. Claws can use it to:
- Adjust typo tolerance in interpretation
- Include in system prompt: "User is on mobile — expect autocomplete errors, interpret charitably"
- Inform response style (shorter if mobile, etc.)

**Why this matters:** The same message "I want to order the thing we discussed" means something different at a desktop (deliberate) vs. mobile (might be autocomplete noise). Knowing the source changes how confidently to interpret.

### Layer 2: Channel-per-device pattern (user convention, no code needed)

Encourage/document: use different channels for different input contexts. e.g.:
- Signal = mobile (short, typo-prone, casual)
- Telegram = desktop (longer, deliberate, precise)

This is free — PolyClaw already routes per-channel. Just needs documentation and possibly a wizard step: "Is this channel primarily used from mobile?"

### Layer 3: Platform detection from channel metadata

Some messaging APIs expose device/platform info:
- Signal: no reliable device type in the protocol
- Telegram: `from.is_bot` and some platform hints available in message metadata
- WhatsApp: web/app distinction sometimes available

Worth researching per-channel what's actually available. Don't assume uniformity.

### Layer 4: Deterministic pre-pass (conservative, opt-in)

A pre-processing step before the LLM sees the message:

```
raw_input → typo_annotator → annotated_input → LLM
```

The annotator does NOT correct — it annotates:
```
"I want too ordder the thingg" 
→ "[possible typos detected: 'too'→'to'?, 'ordder'→'order'?, 'thingg'→'thing'?]
   Original: I want too ordder the thingg"
```

The LLM then decides whether to apply corrections based on context. This is safer than auto-correction because:
- Original is preserved
- LLM has full context to decide
- False positives are visible and ignorable

**Implementation options:**
- Simple: edit distance heuristic against a common word dictionary
- Better: SymSpell or similar fast spelling correction library (Rust: `symspell` crate)
- Best: context-aware correction (harder, may need a small local model)

**Risk:** Any auto-annotation adds noise and could mislead. Must be very conservative. Default threshold: only flag tokens with edit distance ≤ 1 from a high-frequency word AND the token itself is not in dictionary.

### Layer 5: Per-user learned typo profile (opt-in, persistent)

Store a per-user correction map:

```toml
# ~/.polyclaw/typo-profiles/brian.toml
[corrections]
"teh" = "the"
"hte" = "the"
"waht" = "what"
# ... learned over time
```

Rules:
- Only populated explicitly (user says "I always type X when I mean Y") OR from corrections the agent made that user confirmed
- Never auto-populated from guesses
- User-visible and editable
- Applied as a deterministic pre-pass BEFORE the LLM sees the message
- Logged so user can audit what was corrected

**Why deterministic pre-pass > LLM correction:** Consistent, cheap, auditable. The LLM doesn't need to re-solve known patterns every time.

---

## What to avoid

- **Silent auto-correction:** If we correct "pubic" → "public" silently, user never knows. But if we get it wrong, the agent acts on the wrong thing.
- **Overcorrection:** "I want to send a message to Ann" where "Ann" is a contact name should NOT be flagged as a typo for "and".
- **Learning wrong patterns:** If user deliberately uses "gonna" / "wanna" / abbreviations, don't treat these as typos.
- **Mobile-to-desktop contamination:** Correction profile learned on mobile shouldn't aggressively apply on desktop where the user is being precise.

---

## Implementation in PolyClaw vs NZC

**PolyClaw:** Best place for layer 1 (channel metadata), layer 2 (convention documentation), layer 3 (platform detection), layer 4 (pre-pass annotator). PolyClaw sees all messages before dispatch and has channel context.

**NZC:** Could implement layer 4 as a message pre-processor, layer 5 as a workspace file. But better to keep correction logic in PolyClaw and have NZC just receive already-annotated messages.

---

## Open Questions for Brian

1. Should correction profiles be per-channel or per-user-identity? (User identity is more useful but PolyClaw needs identity resolution first)
2. Should the annotated typo hints be visible to the user in replies? ("I interpreted X as Y — let me know if I'm wrong") or silent?
3. Any prior art worth looking at? (iOS/macOS have system-level autocorrect APIs; Android has similar)
4. Is this worth a small research spike to check what messaging platform metadata is actually available?

---

## Visibility & Strategy: Configurable

The whole strategy — including visibility — should be configurable per-user or per-channel:

```toml
[typo_handling]
mode = "annotate"       # "off" | "annotate" | "correct" | "ask"
visibility = "inline"   # "inline" (LLM sees both) | "silent" | "summary"
threshold = "conservative"  # "conservative" | "moderate" | "aggressive"
profile = "brian"       # per-user learned profile (optional)
```

- `annotate` + `inline`: "possible intended: X" injected before LLM. LLM sees both, decides. Most robust.
- `correct` + `silent`: deterministic pre-pass only, LLM never sees original. Faster, riskier.
- `ask`: agent explicitly asks user "did you mean X?" — most accurate, most friction.

Default: `off`. Opt-in only.

---

## Research Directions

Before implementing Layer 4+, a research spike is warranted:

- **Peter Norvig's spell corrector** (classic, simple, surprisingly good)
- **Mobile-specific error databases**: fat finger confusion matrices, swipe keyboard (e.g. SwiftKey) error patterns, autocomplete substitution patterns
- **Noisy channel models**: formal probabilistic framing for typo correction
- **Prior art in chat/messaging**: Slack, iMessage, WhatsApp — do any do server-side correction? What can we learn?
- **Channel metadata availability**: per-platform investigation — what device/keyboard hints are actually in the wire protocol?
- **SymSpell** (Rust crate available): O(1) spell correction, very fast, good for pre-pass use

Some of this may be a small research subagent task when we're ready to implement.

---

## Priority

Low — quality-of-life feature, not a correctness concern. Worth implementing after the core installer/vault/migration sprint is stable. The channel metadata piece (Layer 1) is the easiest win and could ship with the installer. Layers 4-5 need the research spike first.

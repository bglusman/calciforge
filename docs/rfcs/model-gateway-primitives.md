# RFC: Model-gateway primitives (Alloy + Cascade + Dispatcher)

Status: **draft** — open for discussion before implementation.

## TL;DR

Today zeroclawed has one model-blending primitive — **alloy** — and an implicit on-error fallback behavior. That's not enough:

- Alloys assume constituents are **interchangeable** (any can serve any request). That assumption breaks when constituents have different context windows — e.g., local Qwen 3.5 (32K) + Kimi K2.6 (262K).
- Without size-awareness, a 100K-token request can be silently routed to a 32K model and truncated, losing data with no error.

This RFC proposes:

1. **Three clearly-scoped primitives**, each with a distinct purpose:
   - `[[alloys]]` — blend between **equivalent** models (today, with safety additions)
   - `[[cascades]]` — try in order, fall through on error (today, promoted to a named primitive)
   - `[[dispatchers]]` — pick **by request shape** (new; size-first, extensible to other matchers)
2. **A shared `TokenEstimator` trait** used by all three primitives to reason about whether a request "fits" a model. Default: configurable chars-per-token heuristic. Pluggable: real tokenizers (`tiktoken`, `sentencepiece`).
3. **`min_context_window` safety assertion** at alloy-build time, so silent-truncation footguns fail loudly.

## Problem statement

### Why alloys-as-tier-router is wrong

The user wrote recently:

> I think we also want an alloy that hybridizes local and kimi 2.6 models to see if we can best of both and avoid hitting limits on kimi while still leveraging local compute and hopefully getting results nearly as good as kimi… though not sure how context window sizes will work in alloys, maybe we never addressed that?

This is a real use case — "most requests are small and fit local; occasional large ones need Kimi" — but forcing it into an alloy is a dead-end. Alloys do **random weighted sampling** between constituents. That's meaningful when constituents are ~equivalent and the blend expresses a cost/quality preference. It breaks when one constituent *can't serve* certain requests at all.

The real abstraction the user wants is: "choose smallest-sufficient model per request." That's not sampling — it's routing.

### Why cascade (on-error fallback) also needs size awareness

Cascade picks primary, falls through to secondary on failure. If primary is 200K-context Claude and secondary is 32K Qwen, a 100K request that makes it past the primary (say Claude is rate-limited at second call) falls through to Qwen and silently loses 70% of the context.

All three primitives need to respect context-window math.

## Proposed primitives

### 1. Alloy — blending equivalent models (today, + safety)

**Purpose:** cost/quality blending via sampling. `80% fast + 20% smart = blended average`.

**Assumption:** constituents are interchangeable. **New requirement:** they must have compatible context windows.

**New config fields:**

```toml
[[alloys]]
id = "fast-smart-blend"
strategy = "weighted"
# Effective ceiling for the alloy. If not set, auto-computed as min(constituents.context_window).
# Requests above this ceiling are rejected at alloy level (loudly, not silently).
min_context_window = 200000

[[alloys.constituents]]
model = "gemini-2.5-flash"
context_window = 1048576
weight = 80

[[alloys.constituents]]
model = "claude-haiku-4-6"
context_window = 200000
weight = 20
```

**Validation:** at `AlloyProvider::new()`, error if any constituent's declared `context_window < min_context_window`. Catches the "I didn't mean to put a 32K and a 262K in the same alloy" footgun at config-load time.

**Runtime check:** when request arrives, estimate tokens (via `TokenEstimator`) and reject with clear error if `estimate > min_context_window`. Never truncate silently.

### 2. Cascade — ordered fallback on error (today, named)

**Purpose:** reliability. Try primary, on timeout/5xx/429 try secondary.

**Today's behavior** (implicit in alloy `fallbacks` array) → **promoted** to its own named primitive for clarity:

```toml
[[cascades]]
id = "kimi-with-fallback"
# First success wins. Cascading ONLY on error (not on size).
# Caller's responsibility: ensure request fits the NARROWEST member, OR wrap cascade inside a dispatcher.
[[cascades.steps]]
model = "opencode-go/kimi-k2.6"
context_window = 262144

[[cascades.steps]]
model = "kimi-for-coding"           # Moonshot
context_window = 128000

[[cascades.steps]]
model = "local/qwen3.5-35b"         # last resort, much smaller
context_window = 32768
```

**Runtime behavior:** before trying step N, estimate request tokens; skip to step N+1 if request doesn't fit. Track which steps were attempted for telemetry. Fail with clear error if no step can serve.

**Discussion open:** should cascade skip-on-size be silent, or emit a warning per downgrade? *Recommendation: warning log at INFO level per skipped step; final error if everything skipped.*

### 3. Dispatcher — pick by request shape (new)

**Purpose:** route requests to the **smallest-sufficient** model (or, future: by other properties).

**MVP rule type:** `max_input_tokens` thresholds. Extensible to other matchers later (agent id, content type, time of day, whatever).

```toml
[[dispatchers]]
id = "kimi-smart"
# First matching rule wins. Rules evaluated in declared order.
# Unmatched requests (bigger than any rule's threshold) → error with a clear message.

[[dispatchers.rules]]
when.max_input_tokens = 30000           # with ~2K safety margin from Qwen's 32K
target = "local/qwen3.5-35b"

[[dispatchers.rules]]
when.max_input_tokens = 250000          # ~12K margin from Kimi's 262K
target = "opencode-go/kimi-k2.6"

[[dispatchers.rules]]
when.max_input_tokens = 900000
target = "gemini-2.5-flash"             # 1M context for monsters
```

**Target can be a model OR another primitive:**

```toml
[[dispatchers.rules]]
when.max_input_tokens = 180000
target = "alloy/gemini-claude-200k"      # for ≤180K, blend our 200K-safe alloy
```

This composition is how you get the user's original "kimi + local hybrid" goal safely:
- `dispatcher[ ≤30K → local, ≤250K → kimi-go ]`
- Below 30K, requests hit local (free, fast). Above, Kimi.
- No silent truncation, no wasted Kimi calls on small requests.

### On naming — I'm calling it "dispatcher"

"Router" is overloaded in software (content routing, URL routing, network routing). "Dispatcher" is more specific: picks which backend handles this request.

Alternatives considered:

| Name | Pro | Con |
|------|-----|-----|
| `router` | Familiar | Too generic; "routing" overloaded |
| `dispatcher` ⭐ | Clear "pick target per request" semantics | A little programmer-jargony |
| `tier` / `tiers` | Captures size-laddering | Presumes size is the only axis |
| `fit` / `fitter` | Short, clear for size case | Obscure for non-size rules |
| `selector` | Generic | Also used in k8s/CSS; overloaded |
| `picker` | Folksy, clear | Informal; unconventional |
| `bucket-router` | Explicit | Compound, awkward |

Going with `dispatcher` pending feedback.

## The `TokenEstimator` trait (shared)

All three primitives need to answer: *"does this request's input fit in model X's context window?"* That requires a token estimate. We want:

- A sane default that works out-of-the-box
- Configurable tuning for power users
- An extension point for real tokenizers when accuracy matters

### Trait

```rust
/// Estimate the token count of a prompt for the purpose of fit-checking.
///
/// Implementations SHOULD be conservative (over-estimate slightly) so that
/// fit-checks have headroom — under-estimation is a silent-truncation risk,
/// over-estimation at worst forces a fallthrough to a bigger model.
pub trait TokenEstimator: Send + Sync {
    /// Estimate tokens for a plain-text prompt (excludes tool definitions).
    fn estimate_text(&self, text: &str) -> usize;

    /// Estimate tokens for a chat request (messages + optional tool definitions).
    /// Default impl sums per-message + fixed overhead; override for accuracy.
    fn estimate_chat(&self, messages: &[Message], tools: &[ToolDef]) -> usize {
        // naive default: sum of text estimates + per-message framing overhead
        let msg_tokens: usize = messages.iter().map(|m| self.estimate_text(&m.content)).sum();
        let tool_tokens: usize = tools.iter().map(|t| self.estimate_text(&t.schema_json)).sum();
        let framing = messages.len() * 4;  // role markers, separators
        msg_tokens + tool_tokens + framing
    }
}
```

### Default implementation: `CharRatioEstimator`

```rust
pub struct CharRatioEstimator {
    pub chars_per_token: f32,  // default 3.5 (English-prose-biased)
    pub safety_margin: f32,    // default 1.10 (overstate by 10%)
}

impl Default for CharRatioEstimator {
    fn default() -> Self {
        Self { chars_per_token: 3.5, safety_margin: 1.10 }
    }
}

impl TokenEstimator for CharRatioEstimator {
    fn estimate_text(&self, text: &str) -> usize {
        let chars = text.chars().count() as f32;
        (chars / self.chars_per_token * self.safety_margin).ceil() as usize
    }
}
```

**Rationale for default values:**

- **3.5 chars/token** — common average for English prose with GPT-family tokenizers. Code and Chinese text are denser (~2.5 and ~1.5 respectively). Under-estimating for code/non-English is the *risk case*, hence the safety margin.
- **10% safety margin** — covers most under-estimates without wasting model context.

Both fields are configurable, see "Config surface" below.

### Pluggable implementations (future)

```rust
// Real tokenizer, exact count (via `tiktoken-rs` crate)
pub struct TiktokenEstimator {
    tokenizer: tiktoken_rs::CoreBPE,
}

// SentencePiece for Llama-family models
pub struct SentencePieceEstimator { /* ... */ }
```

Users opt in by configuring a non-default estimator. We ship `CharRatioEstimator` in the default build, others gated behind features (`features = ["tiktoken"]`) to keep build dependencies light.

### Per-model overrides

Different models have wildly different token/char ratios:

| Model family | Rough chars/token |
|---|---|
| GPT-4 (English prose) | ~4 |
| GPT-4 (code) | ~2.5 |
| Claude | ~3.7 |
| Llama/Qwen | ~3.0 |
| Chinese text | ~1.5 |

So `chars_per_token` should be overridable per-model:

```toml
[model_defaults]
chars_per_token = 3.5
safety_margin = 1.10

[[models]]
id = "qwen3.5-35b"
context_window = 32768
chars_per_token = 3.0      # Qwen tokenizer tends denser

[[models]]
id = "kimi-k2.6"
context_window = 262144
chars_per_token = 2.8      # Chinese-English mixed, code-heavy
```

### Config surface

**Global default** in top-level config:

```toml
[tokenizer]
kind = "char_ratio"           # "char_ratio" | "tiktoken" | "sentencepiece"
chars_per_token = 3.5
safety_margin = 1.10
```

**Per-primitive override** (in an alloy / cascade / dispatcher):

```toml
[[dispatchers]]
id = "smart"
[dispatchers.tokenizer]
kind = "tiktoken"
encoding = "cl100k_base"
```

**Per-model override**: as shown above. Takes precedence over per-primitive and global.

Resolution order: per-model > per-primitive > global > built-in default.

## Cross-primitive composition

The primitives are composable. Common patterns:

**Pattern 1: size-tier that blends within tiers**

```toml
[[alloys]]
id = "claude-gemini-200k"
min_context_window = 200000
# … 200K-safe blend

[[dispatchers]]
id = "smart"
[[dispatchers.rules]]
when.max_input_tokens = 30000
target = "local/qwen3.5-35b"
[[dispatchers.rules]]
when.max_input_tokens = 180000
target = "alloy/claude-gemini-200k"   # small-enough for our 200K blend
[[dispatchers.rules]]
when.max_input_tokens = 900000
target = "gemini-2.5-flash"
```

**Pattern 2: dispatcher in front of cascade**

```toml
[[cascades]]
id = "kimi-or-fallback"
# Assumes caller fits within narrowest member — paired with a dispatcher for safety
[[cascades.steps]]
model = "opencode-go/kimi-k2.6"
[[cascades.steps]]
model = "kimi-for-coding"

[[dispatchers]]
id = "with-safety"
[[dispatchers.rules]]
when.max_input_tokens = 125000          # narrowest cascade member is 128K Moonshot
target = "cascade/kimi-or-fallback"
[[dispatchers.rules]]
when.max_input_tokens = 250000
target = "opencode-go/kimi-k2.6"        # direct, Moonshot can't fit
```

**Rule**: cascades are *not* size-safe on their own — they must be used at a level where all members can serve the incoming request, OR wrapped in a dispatcher.

## Edge cases and open questions

### 1. Conversation-context growth

A dispatcher picks at request time. By message 20, the cumulative context may have grown past the initially-chosen model's ceiling.

**Options:**
- **(a) Sticky**: once picked, stay. Errors late if conversation grows past ceiling.
- **(b) Re-evaluate per turn**: dispatcher runs every turn. Session continuity breaks when the tier changes (different model, different "memory"). Bad UX.
- **(c) Always pick for session worst-case**: dispatcher uses an estimate of "this session's max context," picks a ceiling that covers it. Conservative, wastes smaller-model opportunities.

**Recommendation**: Configurable. Default **(a) sticky** with a clear error when the session outgrows its chosen tier, so the user knows to start a new session on a bigger tier. **(b)** as an opt-in for stateless contexts.

```toml
[[dispatchers]]
id = "smart"
re_evaluate = "sticky"   # "sticky" | "per_turn" | "worst_case"
```

### 2. Tool-use inflates context invisibly

Function definitions + tool results add tokens not present in the user's message. A `list_files` tool definition is ~100 tokens; a directory listing result can be thousands.

Addressed by the `estimate_chat` method taking tools explicitly, and by default safety margin. Power users with tool-heavy flows should bump `safety_margin` (0.15–0.20 reasonable).

### 3. Streaming output doesn't count against context

Output tokens don't count against the input-context budget. Our estimator is *input* estimator only.

### 4. Reasoning tokens (e.g., Kimi K2.6 reasoning mode)

Reasoning tokens add output cost but are usually *not* in the input context. Estimator doesn't need to account for them.

### 5. Context-window units

All sizes are in **tokens**. Not characters, not bytes. Config authors can use `K` suffix for readability:

```toml
context_window = 262144            # tokens
# equivalently:
context_window = "262K"            # parsed as 262 * 1024
```

### 6. What if model declares no context_window?

- **Alloy**: constituent can't be fit-checked; alloy's `min_context_window` is still honored if set, otherwise alloy has no enforced ceiling (runtime behavior: attempt and trust the model to error on its own).
- **Dispatcher**: rule targets are either sized (matchable) or "any" (catch-all, always matches last). Unsized rules only make sense as catch-alls.
- **Cascade**: unsized steps always considered eligible (no pre-skip).

Explicit `context_window = 0` means "unknown/unbounded" and skips size-checks for that member.

## Migration

**Alloys today** continue working. Adding `min_context_window` is optional but **auto-computed** as `min(constituent.context_window)` if not specified. Constituents without a declared `context_window` are treated as unknown (see previous section).

**Cascades today** are implicit inside alloy's `fallbacks` behavior. This RFC promotes them to a named `[[cascades]]` primitive. Transition: existing alloy-with-fallbacks behavior becomes sugar for `alloy-wrapped-in-cascade`; we keep the sugar so existing configs don't break.

**Dispatchers are new.** Opt-in.

**TokenEstimator is new.** Added as a global-defaults-with-overrides. If unconfigured, `CharRatioEstimator::default()` is used throughout.

## Scope boundaries of this RFC

**In-scope:**
- Design of the three primitives and their config shape
- TokenEstimator trait + default implementation
- Safety rules (alloy min_context_window, cascade skip-on-size, dispatcher exhaustion error)
- Composition rules
- Migration guarantees

**Out of scope (follow-up work):**
- Actual dispatcher implementation (this RFC approved → separate PR)
- Cascade-as-a-named-primitive implementation (separate PR; today's implicit fallback still works)
- Plugging `tiktoken-rs` or other real tokenizers (separate PR; trait is there, default is enough for now)
- Per-session routing memory (noted in edge case #1; follow-up if needed)
- Docs: this will be captured in README + a dedicated `docs/model-gateway.md` when implementation lands

## Open questions for reviewers

1. **Naming: `dispatcher` vs alternatives?** (See table above.)
2. **Cascade as a named primitive — worth it, or keep implicit inside alloy?** My lean: promote to named primitive because it simplifies composition stories.
3. **Safety margin default (10%) — too much, too little, or right?** Based on anecdotal reports; worth revisiting with real telemetry once we have token counts vs. char counts from a week of live traffic.
4. **Per-primitive tokenizer override — useful, or premature?** If most users will only ever use the default, we could defer the config surface for per-primitive overrides to v2.
5. **Re-evaluation strategy (sticky vs per_turn vs worst_case) default — which one?** I proposed sticky; alternative: worst_case for stateless API callers.

## Next steps if this RFC lands

1. Small safety PR — add `context_window` to `AlloyConstituentConfig`, `min_context_window` to `AlloyConfig`, validation in `AlloyProvider::new()`. No runtime behavior change beyond rejecting bad configs at startup. *(Already scoped as task #23.)*
2. Add `TokenEstimator` trait + `CharRatioEstimator` impl. Wire into alloy for runtime request-fit rejection.
3. Add `[[cascades]]` named primitive with same fit-check semantics.
4. Add `[[dispatchers]]` with `max_input_tokens` rules.
5. README updates + `docs/model-gateway.md` authoritative reference.

Each as a focused PR. RFC is the long-form design; PRs execute the plan.

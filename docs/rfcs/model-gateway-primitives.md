# RFC: Model-gateway primitives (Alloy + Cascade + Dispatcher)

Status: **implemented for the core runtime.** Alloy context-window
safety, named `[[cascades]]`, named `[[dispatchers]]`, and the shared
estimator trait are in code. Real tokenizer-backed estimators,
`capacity_fraction`, and per-model/per-primitive estimator overrides
remain future work.

## Decisions (from first review)

| # | Question | Resolution |
|---|----------|------------|
| 1 | Name for the size-routing primitive | **`dispatcher`** ("router" too generic) |
| 2 | Cascade as a named primitive | **Yes** — own `[[cascades]]` table |
| 3 | Safety margin default | **Two knobs** — estimator `safety_margin` (default 1.10) AND per-model `capacity_fraction` (default 1.0; users lower to e.g. 0.85 when a model degrades near its ceiling). Composition formula and rationale in the updated section below. |
| 4 | Per-primitive tokenizer override + second tokenizer impl | **Default implementation ships first.** `CharRatioEstimator` is wired into routing and `ByteRatioEstimator` exists for denser prompt families. `TiktokenEstimator`, `SentencePieceEstimator`, and per-primitive overrides are deferred behind future feature flags. |
| 5 | Re-evaluation default for dispatchers | **`per_turn`** (re-evaluate each message — never dies from size). `sticky` as opt-in for flows where model-voice continuity matters. `sticky_escalate` as a middle-ground convenience (sticky, permit one auto-promotion on ceiling, then sticky at the new tier). `worst_case` advanced opt-in with required growth prior. |
| 6 | Back-compat: allow missing `context_window` on alloy constituents | **No — required field.** Prototype phase, all installations owned in-house. Forcing size declaration at config load prevents silent truncation forever; trivial one-time config edit. |
| 7 | Dispatcher rule semantics + capacity_fraction interaction | **Default: "first target whose effective ceiling fits the request."** No `max_input_tokens` thresholds needed for the common case. `capacity_fraction` lives on each model individually and feeds the effective-ceiling computation. Explicit `when.max_input_tokens` rules remain available for non-size routing (cost tier, agent-id, etc.). |

## TL;DR

Today calciforge has one model-blending primitive — **alloy** — and an implicit on-error fallback behavior. That's not enough:

- Alloys assume constituents are **interchangeable** (any can serve any request). That assumption breaks when constituents have different context windows — e.g., local Qwen 3.5 (32K) + Kimi K2.6 (262K).
- Without size-awareness, a 100K-token request can be silently routed to a 32K model and truncated, losing data with no error.

This RFC proposes:

1. **Three clearly-scoped primitives**, each with a distinct purpose:
   - `[[alloys]]` — blend between **equivalent** models (implemented, with context-window safety)
   - `[[cascades]]` — try in order, fall through on error (implemented as a named primitive)
   - `[[dispatchers]]` — pick **by request shape** (implemented for size-first routing)
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
name = "Fast + Smart Blend"
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

**Validation:** at `AlloyProvider::from_config()`, error if any constituent's declared `context_window < min_context_window`. Catches the "I didn't mean to put a 32K and a 262K in the same alloy" footgun at config-load time.

**Runtime check:** when request arrives, estimate tokens (via `TokenEstimator`) and reject with clear error if `estimate > min_context_window`. Never truncate silently.

### 2. Cascade — ordered fallback on error (today, named)

**Purpose:** reliability. Try primary, on timeout/5xx/429 try secondary.

**Today's behavior:** there is no `fallbacks` field on `AlloyConfig`. Fallback
is implicit — `AlloyProvider::select_plan()` returns an `ordered_models: Vec<String>`
listing every constituent as a potential fallback, and the proxy iterates them
in `route_with_fallback()` until one succeeds. Order within that list is
**deterministic for `round_robin`** (rotating from the last selected index)
but **varies per request for `weighted`** (weighted sampling without
replacement). That "all constituents of the alloy are also fallbacks" pattern
is what the cascade primitive **promotes** to its own named construct, with
the important distinction that cascade ordering is *always* deterministic
(declaration order):

```toml
[[cascades]]
id = "kimi-with-fallback"
# First success wins.
#
# Cascade is TRIGGERED by errors (timeout, 5xx, 429) — it does not treat
# "request too large" as a retry condition. But before each step is
# attempted, the runtime pre-checks that the request fits that step's
# context_window and SKIPS unfit steps (with a warning log) rather than
# letting the model return an error. Think of it as: ineligibility is
# cheap to detect up front, so we do; actual errors are what cascade
# retries exist for.
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

**Default behavior:** ordered list of targets; first target whose *effective ceiling* can hold the request wins. No thresholds to maintain — the size check uses each target's own declared `context_window × capacity_fraction`.

```toml
[[dispatchers]]
id = "kimi-smart"
reevaluate = "per_turn"
# Try in order. First target whose effective ceiling fits the request wins.
# Error if no target fits.
targets = [
    "local/qwen3.5-35b",            # effective ≈  24,576
    "opencode-go/kimi-k2.6",        # effective ≈ 222,822
    "gemini-2.5-flash",             # effective ≈ 996,147
]
```

The effective ceiling is `context_window × capacity_fraction`, computed per-model. Adding or removing a model from the list doesn't require re-computing thresholds — the model's own declaration drives the fit check.

**Targets can be models OR other primitives:**

```toml
targets = [
    "local/qwen3.5-35b",
    "alloy/claude-gemini-200k",     # for requests that fit the alloy's effective ceiling
    "gemini-2.5-flash",
]
```

**Explicit rules for non-size decisions** (advanced — cost tier, agent id, request content, time of day):

```toml
[[dispatchers]]
id = "cost-aware"
# When explicit rules are present, they override the default fit-first-target behavior.
# Rules evaluated in declared order; first match wins.

[[dispatchers.rules]]
when.max_input_tokens = 10000           # hand-set floor, tighter than capacity_fraction would imply
target = "cheap-model"

[[dispatchers.rules]]
fits_target = true                      # fall back to implicit fit check against the target's effective ceiling
target = "expensive-model"
```

This composition is how the "kimi + local hybrid" goal is expressed safely:
- Most requests fit local (free, fast, ≈24K effective). Above, Kimi.
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
- **10% safety margin** — covers most under-estimates for prose and moderate code. **Not sufficient on its own for CJK-heavy prompts**, where actual tokens can run >2× a 3.5-chars/token estimate even after the margin. For those workloads, either (a) override `chars_per_token` to a denser ratio (e.g., 1.8), (b) raise `safety_margin` substantially (2.0+), or (c) use the Tiktoken estimator (feature flag) where an exact BPE count eliminates the guesswork. The CharRatio defaults are safe for the English-first deployments this RFC targets; anything heavier should tune.

Both fields are configurable, see "Config surface" below.

### Two distinct safety knobs

The original draft conflated two things. They're separate concerns with different defaults and scopes:

**Knob A — estimator `safety_margin` (multiplier on the estimate):**
"I might under-count tokens because my heuristic is approximate."
- Multiplicative factor applied to the estimate itself
- Default `1.10` for char-ratio; a real tokenizer like tiktoken can use `1.02` since it's accurate to ~1%
- Belongs to the estimator; each estimator implementation carries its own default
- Tighter → risks silent truncation; looser → wastes headroom

**Knob B — model `capacity_fraction` (multiplier on the declared window):**
"Even if I knew the exact count, some models degrade near their ceiling. Don't push them there."
- Multiplicative factor applied to the model's *declared* `context_window`
- Default `1.0` (use the full declared window). Users lower it per-model when they see quality drop-off.
- Per-model because behavior-near-ceiling varies (e.g., Claude reportedly holds up to ~95% of ceiling; some models noticeably degrade past ~70%)
- Users who say "I want a much higher buffer because I've noticed dumbing" set `capacity_fraction = 0.7` — clean separation from the estimator concern.

**Fit-check composition:**

Convention: `TokenEstimator::estimate_*` returns a **conservative** count
with `safety_margin` already applied (see the `CharRatioEstimator::estimate_text`
impl above — the `* self.safety_margin` happens inside). Callers never
multiply by `safety_margin` a second time. The per-model `capacity_fraction`
is applied once, on the *declared* `context_window`, to derive an "effective
ceiling". The fit check is then a direct comparison:

```
estimate  = TokenEstimator::estimate_*(...)     // already margin-applied
ceiling   = model.context_window * model.capacity_fraction

Rejected if:  estimate > ceiling
```

Worked example (prose so the formula stays the single source of truth):

- Raw input measured at roughly 180k tokens. Estimator applies its
  internal 10% margin, so the returned estimate is about 198k.
- Model has a declared context window of 262_144 tokens; the operator
  has set `capacity_fraction` to 0.85, giving an effective ceiling near
  222,822.
- 198k < 222k, so the request is accepted.
- If we had double-applied the margin (caller multiplying again after
  the estimator already did), we would compare roughly 217,800 against
  the same 222,822 — still under, but needlessly close.

Dispatcher rule language uses "effective ceiling" to mean
`context_window × capacity_fraction` consistently.

**Config:**

```toml
[tokenizer]
kind = "char_ratio"
chars_per_token = 3.5
safety_margin = 1.10        # estimator knob

[[models]]
id = "kimi-k2.6"
context_window = 262144
capacity_fraction = 0.85     # avoid top 15% where Kimi reportedly degrades

[[models]]
id = "claude-sonnet-4-6"
context_window = 200000
capacity_fraction = 0.95     # Claude holds up closer to ceiling

[[models]]
id = "local/qwen3.5-35b"
context_window = 32768
capacity_fraction = 0.75     # user has observed noticeable drop past 24K
```

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

> **Note:** the `[tokenizer]`, `[model_defaults]`, and `[[models]]` sections below are **proposed additions** to `PolyConfig`. They do not exist in the current schema and will be added as part of the implementation of this RFC. A schema version bump is expected; existing configs stay valid without them (resolution falls through to built-in defaults).

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

**Decision**: default to **`per_turn`**. Chat APIs are stateless; re-picking per message mechanically works. For task-completion flows (calciforge's main use case) the cost of an occasional model swap is lower than the cost of a session that dies at a ceiling.

```toml
[[dispatchers]]
id = "smart"
reevaluate = "per_turn"          # default — re-pick each message
# reevaluate = "sticky"          # pick once, error on ceiling (for voice-continuity flows)
# reevaluate = "sticky_escalate" # sticky, auto-promote once on ceiling, then sticky at new tier
# reevaluate = "worst_case"      # advanced — requires growth prior below
# assume_session_max_tokens = 100000
```

Pragmatic default for most flows: `sticky_escalate` is arguably the sweet spot (stability most of the session, graceful upgrade when needed). Listed as an opt-in for now since `per_turn` is simplest and always works.

### 2. Tool-use inflates context invisibly

Function definitions + tool results add tokens not present in the user's message. A `list_files` tool definition is ~100 tokens; a directory listing result can be thousands.

Addressed by the `estimate_chat` method taking tools explicitly, and by default safety margin. Power users with tool-heavy flows should bump `safety_margin` (a multiplier — try 1.15 or 1.20, i.e. +15–20%, rather than the default 1.10).

### 3. Output tokens DO count against the model's total context

Correction to an earlier draft: for most providers the context window bounds `input_tokens + output_tokens`, not just input. A request that fits on input can still overflow at generation time if `max_tokens` is large. Our `TokenEstimator` measures *input* only, so the fit-check must reserve headroom for the output budget. Implementation detail: primitive runtime compares `estimate(input) + max_tokens_for_request` against `effective_ceiling`, not just `estimate(input)`. Callers who don't set `max_tokens` explicitly must supply a default output budget (e.g., 4K) so the check isn't silently bypassed.

### 4. Reasoning tokens (e.g., Kimi K2.6 reasoning mode)

Reasoning tokens add output cost but are usually *not* in the input context. Estimator doesn't need to account for them.

### 5. Context-window units

All sizes are in **tokens**. Not characters, not bytes. Config authors can use `K` suffix for readability; `K` means `* 1024` (binary convention):

```toml
context_window = 262144            # tokens (2^18)
# equivalently:
context_window = "256K"            # parsed as 256 * 1024 = 262144
```

A literal "262K" would parse as 262 * 1024 = 268288, not 262144 — use `"256K"` if you want the `262144` value.

### 6. What if a model declares no context_window?

- **Alloy**: not applicable. Alloy constituents REQUIRE `context_window` (validated at `AlloyProvider::from_config`, and `0` is rejected explicitly). No silent-truncation path.
- **Dispatcher**: rule targets are either sized (matchable) or "any" (catch-all, always matches last). Unsized rules only make sense as catch-alls.
- **Cascade**: unsized steps are always considered eligible (no pre-skip); the step is attempted and errors surface as normal cascade failures.

## Migration

**Alloy constituents now REQUIRE `context_window`.** No back-compat for missing fields. Prototype phase, all installations owned in-house — fixing existing config files is a one-time edit; the upside (no silent truncation, ever) is worth breaking the schema. `min_context_window` on the alloy stays optional and auto-computes as `min(constituent.context_window)` when not specified.

**Existing config files needing updates:**
- `.210` `/etc/calciforge/config.toml`: `kimi-for-coding` alloy (Kimi + DeepSeek constituents)
- Anywhere else currently using `[[alloys]]`

Migration done in the same PR that introduces the required field.

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

## Open questions (resolved — see Decisions at top of doc)

All initial review questions have been resolved. Remaining open items surface during implementation:

1. **`capacity_fraction` defaults per known model family** — need empirical data. Initial defaults: `1.0` (no derate) until a model proves it needs less. Claude ~0.95, Kimi ~0.85 recommended starting points in docs, not hardcoded.
2. **Tiktoken encoding selection** — does the config need to specify `cl100k_base` vs `o200k_base` per model, or pick a sensible default? Open; likely per-model override.
3. **Dispatcher fit-check evaluation cost** — for very long histories, char-counting on every turn is cheap but non-zero. Measure once implementation lands; add a caching layer only if it shows up in profiles.

## Next steps if this RFC lands

1. Small safety PR — add `context_window` to `AlloyConstituentConfig`, `min_context_window` to `AlloyConfig`, validation in `AlloyProvider::new()`. No runtime behavior change beyond rejecting bad configs at startup. *(Already scoped as task #23.)*
2. Add `TokenEstimator` trait + `CharRatioEstimator` impl. Wire into alloy for runtime request-fit rejection.
3. Add `[[cascades]]` named primitive with same fit-check semantics.
4. Add `[[dispatchers]]` with `max_input_tokens` rules.
5. README updates + `docs/model-gateway.md` authoritative reference.

Each as a focused PR. RFC is the long-form design; PRs execute the plan.

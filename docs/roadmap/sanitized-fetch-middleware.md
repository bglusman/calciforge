# Sanitized Fetch Middleware

Calciforge currently focuses on flagging external content with `clean`,
`review`, or `unsafe` verdicts. A future fetch pipeline may also support
sanitizing content before it reaches a model.

## Why Consider It

Flagging preserves the original content and produces auditable policy
decisions. Sanitizing can reduce exposure by stripping hidden or irrelevant
content before model ingestion, but it may also remove legitimate context,
break pages, or make it harder to understand what was changed.

Sanitizing is also a token-budget feature. HTML-to-markdown conversion,
readability extraction, and boilerplate removal can reduce bandwidth, latency,
and model context pressure by removing navigation, scripts, styles, ads,
duplicated chrome, comments, and hidden content before the agent sees it.

Useful modes to evaluate:

- `flag`: current behavior; preserve content and attach/block on verdict.
- `annotate`: preserve content but wrap suspicious spans with markers.
- `redact`: replace suspicious spans while preserving surrounding text.
- `sanitize_html`: convert HTML to model-facing markdown after removing hidden
  DOM nodes, comments, off-screen text, suspicious attributes, scripts, styles,
  navigation, ads, and boilerplate.
- `metadata`: keep content unchanged but attach structured findings for the
  agent or caller.

## Design Questions

- Should sanitization happen before scanning, after scanning, or both?
- Should sanitized content be cached by digest separately from raw content?
- How do users inspect the raw content when a sanitizer changes behavior?
- Which content types should be supported first: HTML, Markdown, plain text,
  PDFs, emails, or chat transcripts?
- How should Calciforge report raw bytes, sanitized bytes, and estimated token
  savings so operators can see the security and cost impact?
- Should agents be able to request raw content explicitly, or only with
  approval?

## Implementation Paths

- Integrate an existing tool such as `scurl` as an optional subprocess or
  sidecar for HTML-to-markdown and prompt-defender behavior.
- Port a minimal subset to Rust: HTML parsing, readability extraction, hidden
  DOM stripping, metadata/alt/ARIA handling, and markdown rendering.
- Keep sanitizers as configured middleware stages so operators can choose
  between preserving, annotating, redacting, or transforming content.

The red-team fixture suite in `examples/red-team/` should grow sanitizer cases
alongside scanner cases before this becomes a default behavior.

# Calciforge Red-Team Fixtures

This directory contains regression fixtures for Calciforge's adversary scanner.
Run them with:

```sh
cargo run -p adversary-detector --example red-team
```

Each fixture has a name, URL, scan context, content, layer, local expectation,
and optional remote expectation. Add a fixture when you find a bypass, a false
positive, or a new attack family worth tracking. It is acceptable to document a
known local gap with `"expect_local": "clean"`; the hardening PR that closes
the gap should update that expectation.

Calciforge has two adversary-detector layers, and fixtures should make the
intended owner clear:

- Local Starlark candidates: encoded payloads, Unicode hiding, hidden DOM/text,
  and concrete tool-policy bypass strings.
- Remote LLM candidates: foreign-language attacks, foreign-language encoded
  attacks, poetry/style-shift attacks, fictional framing, coercion, multi-step
  decomposition, and ambiguous intent.
- Shared candidates: governance failures such as identity spoofing, false
  authority claims, cross-agent propagation, hidden task changes, and
  resource-exhaustion prompts.

Primary inspiration sources include GTFOBins/LOLBAS, the Agents of Chaos threat
taxonomy, adversarial-poetry jailbreak work, Agent Arena-style hidden web
content challenges, and scurl-style sanitized-fetch middleware. Keep fixtures
small and deterministic; larger corpora or live model-eval suites should live
beside this harness, not inside it.

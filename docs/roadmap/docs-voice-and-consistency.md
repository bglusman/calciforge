---
layout: default
title: Docs Voice And Consistency
---

# Docs Voice And Consistency

Status: Roadmap

Calciforge's docs should help a tired operator understand what is protected,
what is not protected yet, and what to do next. The current docs are useful but
too often sound like an architecture memo. They also repeat caveats in several
places, which makes drift more likely.

## Goals

- Lead with the user's problem: secret leaks, prompt injection, unsafe tool
  calls, broken model routing, and unclear agent coverage.
- Keep the first explanation short, then link to technical detail.
- Use one source of truth for boundary claims: model gateway, security proxy,
  tool policy, channel auth, and agent capabilities.
- Avoid polished filler phrases that make the docs sound machine-written.
- Keep examples generic. Use `owner`, `agent1`, or role names instead of local
  staging names.
- Add a small amount of warmth where it helps the reader keep going. Do not turn
  the docs into a joke delivery system.

## Style Checks

- Replace abstract opener phrases such as "serves as", "unlock", "ecosystem",
  "comprehensive", and "seamless" with concrete verbs.
- Avoid neat but hollow triplets unless the three items are a real product
  boundary.
- Prefer "what happens" and "what to run" over "why this is powerful".
- Say "this does not cover X" near any protection claim that could be read too
  broadly.
- Move long caveats into linked technical docs when the page is meant to onboard
  a new user.

## Follow-Up Passes

1. Rewrite the home page for a less defensive first read.
2. Deduplicate proxy/MITM caveats across the home page, security gateway docs,
   model gateway docs, and ADR 0001.
3. Audit public docs for local staging names and replace them with generic
   examples.
4. Add a docs consistency check for unsupported/stale terms such as old
   webhook-sidecar WhatsApp setup, removed gateway backends, and private local
   agent names.
5. Review roadmap pages separately. They can be rougher, but each should say
   whether it describes current behavior, proposed behavior, or abandoned work.

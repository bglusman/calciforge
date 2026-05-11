---
layout: default
title: "ADR 0002: Provider Adapter Boundary"
---

# ADR 0002: Provider Adapter Boundary

Status: Accepted

Date: 2026-05-11

## Context

Calciforge's model path was previously described as a singular "model gateway"
with a root backend. That wording made local Calciforge transport, hosted
gateway products, direct provider APIs, local model servers, and test mocks look
like the same thing.

They are not the same thing. Calciforge's durable product responsibility is the
model access boundary: authenticate the caller, identify the agent/user/channel,
apply model policy, resolve aliases and synthetic selectors, audit the request,
and then call a configured model provider boundary.

## Decision

Calciforge will use provider adapters as the primary model-call abstraction.

- `ProviderAdapter` is the runtime trait for a configured model boundary.
- `[[proxy.providers]]` is the preferred operational config surface.
- A deployment may configure multiple adapters: Ollama, OpenRouter, LiteLLM,
  Helicone, direct OpenAI-compatible HTTP, or future native/library adapters.
- Aliases and nested model resolution should be scoped to the selected provider
  where provider-owned routing exists.
- The legacy root `[proxy].backend_type` remains for compatibility, but it is
  not a product default.
- `mock` is test-only. Operational installs must choose at least one explicit
  provider adapter or explicitly disable the model proxy.

External gateways such as OpenRouter, LiteLLM, and Helicone are concrete
provider adapters, not special cases that Calciforge must install by default.
Installer scripts may offer convenience setup for them, but direct use of a
hosted or operator-managed gateway is also valid.

## Consequences

First-class Calciforge recipes should point agents at Calciforge-managed model
boundaries by default so Calciforge can apply policy, auth, audit, aliases, and
adversary hooks. Advanced users may point agents directly at an external
gateway, but docs and doctor checks should describe which Calciforge guarantees
are bypassed.

Provider-specific request fields, model-prefix translation, headers, and
credentials belong in adapter configuration. Provider adapter endpoint auth and
final model-provider credential ownership are separate concepts:

- `api_key` / `api_key_file` authenticate Calciforge to the configured
  `ProviderAdapter` endpoint when that endpoint requires a client credential.
- `model_api_key` / `model_api_key_file` are final upstream model-provider
  credentials, used only when Calciforge owns those credentials.
- `model_credential_owner = "calciforge"` means Calciforge owns final model
  credentials and must be explicitly configured with them.
- `model_credential_owner = "provider"` means the provider boundary owns final
  model credentials, or no final model credential is required, as with local
  unauthenticated endpoints.

The current built-in provider adapters expose one first-class bearer credential
slot per outgoing request. Config validation must reject provider routes that
claim Calciforge owns final model credentials while also configuring a separate
provider endpoint bearer credential, unless a future adapter explicitly
documents a second auth channel. This avoids implying that Calciforge can carry
two unrelated bearer tokens through a generic OpenAI-compatible request.

Model roles are named selectors, not a second routing system. They share the
model shortcut resolver and may point at concrete provider models, shortcuts,
or synthetic selectors. Internal features such as adversary-detector classifier
checks should ask for a role like `security.screening`; deployment config maps
that role to the provider/model/synthetic selector that should serve it.

Generic Calciforge wrappers should not grow provider-specific logic except
through explicit adapter configuration.

Security features that make LLM calls, including adversary-detector classifier
checks, should use the same provider-adapter boundary by default. A remote
scanner service remains supported for BYO policy, but Calciforge-shipped scanner
examples should default to the local Calciforge OpenAI-compatible endpoint, not
direct OpenAI or another public provider.

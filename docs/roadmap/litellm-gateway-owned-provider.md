---
layout: default
title: LiteLLM Gateway-Owned Provider Spike
---

# LiteLLM Gateway-Owned Provider Spike

Status: recipe/proof path.

Calciforge does not need a LiteLLM-specific adapter to exercise the external
gateway contract. LiteLLM's proxy is an OpenAI-compatible gateway process, so
Calciforge can route to it with the existing `backend_type = "http"` provider
path and mark the custody boundary with `credential_owner = "gateway"`.

In this shape, Calciforge owns channel identity, aliases, synthetic selectors,
access policy, command UX, and security scanning. LiteLLM owns upstream provider
keys, provider-specific model names, model groups, virtual-key access,
gateway-side aliases, retries, load balancing, and fallbacks.

```text
agent or channel
  -> Calciforge model gateway
     selector: managed/default
     auth: Calciforge client key
  -> LiteLLM proxy
     selector: default
     auth: LiteLLM virtual key
  -> upstream provider
     selector and provider key owned by LiteLLM
```

## LiteLLM Proxy Config

Example `litellm_config.yaml`:

```yaml
model_list:
  - model_name: default
    litellm_params:
      model: openai/gpt-4o-mini
      api_key: os.environ/OPENAI_API_KEY
  - model_name: coding
    litellm_params:
      model: anthropic/claude-sonnet-4-5
      api_key: os.environ/ANTHROPIC_API_KEY

litellm_settings:
  drop_params: true
  fallbacks:
    - default: ["coding"]

general_settings:
  master_key: os.environ/LITELLM_MASTER_KEY
```

The `model_name` values are the gateway-owned selectors Calciforge should send
after any Calciforge prefix rewrite. The `litellm_params.model` values and
provider API keys remain inside LiteLLM.

Run LiteLLM:

```bash
litellm --config /etc/litellm/config.yaml --host 127.0.0.1 --port 4000
```

Create scoped LiteLLM virtual keys when running with a database-backed LiteLLM
deployment. The LiteLLM key presented by Calciforge should only have access to
the model groups Calciforge is allowed to expose.

## Calciforge Config

Use a normal named provider route:

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"
api_key_file = "/etc/calciforge/secrets/model-gateway-client-key"
backend_type = "mock"
gateway_ui_url = "http://127.0.0.1:4000/ui"
timeout_seconds = 60

[[proxy.providers]]
id = "litellm-managed"
backend_type = "http"
url = "http://127.0.0.1:4000/v1"
credential_owner = "gateway"
api_key_file = "/etc/calciforge/secrets/litellm-virtual-key"
models = ["managed/*"]
strip_model_prefix = "managed/"
timeout_seconds = 60
```

With that config, a Calciforge request for `managed/default` is forwarded to
LiteLLM as `default`. Calciforge does not need to know whether LiteLLM maps that
to OpenAI, Anthropic, Azure, Bedrock, Ollama, a fallback chain, or a model group
with multiple deployments.

`gateway_ui_url` can point at LiteLLM's Admin UI, normally
`<liteLLM-proxy-base>/ui`. That lets Calciforge's `/gateway/ui` redirect and
the channel-side `!gateway` command expose the operator dashboard link through
the same mechanism used for Helicone. For a pure LiteLLM deployment, set it to
the LiteLLM Admin UI. For a mixed deployment with several named external
providers, this is currently one top-level operator link, not a per-provider UI
registry.

Use `models = ["*"]` only for a deliberately broad gateway tenancy. Prefer a
namespace such as `managed/*` for user-facing clarity and to avoid accidentally
colliding with Calciforge-local models, shortcuts, alloys, cascades, or
dispatchers.

## Operator UI

LiteLLM's Admin UI is available at `/ui` on the proxy base URL when the proxy is
started with a master key and database connection. LiteLLM documents it as the
place to create keys, track spend, add models without editing config/CRUD
endpoints, invite users, and manage models.

That UI is not the same product surface as Helicone's request observability
dashboard. The likely split is:

- LiteLLM Admin UI: model groups, model add/remove, virtual keys, users/teams,
  spend/budget controls, routing configuration, and operator administration.
- Helicone UI: request logs, traces, cost/latency analysis, prompt/response
  observability, and debugging provider traffic.

LiteLLM can also log to observability callbacks, including Helicone, but this
recipe treats LiteLLM as the gateway-owned model/key/routing process. If an
operator wants Helicone-grade request observability, use Helicone directly or
wire LiteLLM's logging callbacks as a separate observability concern.

## Smoke Test

The deterministic local proof is:

```bash
uv tool install 'litellm[proxy]'  # one-time, if `litellm` is not on PATH
python3 scripts/model-gateway-litellm-smoke.py
```

Without a persistent install, run it through `uvx`:

```bash
LITELLM_COMMAND="uvx --from litellm[proxy] litellm" \
  python3 scripts/model-gateway-litellm-smoke.py
```

The script starts a local mock OpenAI-compatible upstream, starts LiteLLM with a
temporary config pointing at that mock, starts Calciforge in `--proxy-only`
mode, then sends a request to Calciforge for `managed/default`.

It proves:

- Calciforge routes a namespaced model selector through the generic HTTP
  provider path.
- `credential_owner = "gateway"` can be used without a Helicone adapter.
- Calciforge authenticates to LiteLLM with a gateway key, while LiteLLM holds
  the upstream provider key and upstream model mapping.
- Calciforge-visible `managed/default` is not the upstream provider model.
- Calciforge's generic `/gateway/ui` redirect can point at LiteLLM's `/ui`
  operator dashboard.

It does not prove LiteLLM's production database-backed virtual key management,
dashboard, retries, fallbacks, or budget accounting. Those should be covered by
a separate operator recipe or integration test against a real LiteLLM
deployment.

## Candidate Notes

- LiteLLM is the best first proof because its proxy is OpenAI-compatible and
  directly exercises the key/model ownership split: model groups, virtual keys,
  model access, aliases, retries, load balancing, and fallbacks live in the
  external process.
- model-gateway-rs is a Rust library for model gateway abstractions, not
  primarily an external OpenAI-compatible gateway process. It may be useful for
  future Rust-side experiments, but it is weaker evidence for process-boundary
  key custody.
- Noveum ai-gateway is a Rust OpenAI-compatible gateway candidate, but its
  public examples pass provider choice and provider credentials in request
  headers. That is less aligned with gateway-owned upstream keys unless a
  deployment recipe moves those credentials behind the gateway.
- AISIX/APISIX AI Gateway is interesting for broader API gateway governance,
  per-key model access, moderation, PII redaction, and audit logging. It looks
  heavier than needed for this narrow contract proof.
- agentgateway is broader than a model proxy: LLM gateway, MCP gateway, A2A,
  RBAC, policy, telemetry, and Kubernetes/service-mesh integration. It is a
  plausible future adapter/recipe, but LiteLLM is the more direct low-friction
  proof for gateway-owned model and key registries.

Sources checked during this spike:

- [LiteLLM docs](https://docs.litellm.ai/)
- [LiteLLM virtual keys](https://docs.litellm.ai/docs/proxy/virtual_keys)
- [LiteLLM model access](https://docs.litellm.ai/docs/proxy/model_access_guide)
- [LiteLLM config.yaml](https://docs.litellm.ai/docs/proxy/configs)
- [model-gateway-rs docs.rs](https://docs.rs/crate/model-gateway-rs/)
- [Noveum ai-gateway](https://github.com/Noveum/ai-gateway)
- [AISIX AI Gateway](https://api7.ai/ai-gateway)
- [agentgateway](https://github.com/agentgateway/agentgateway)

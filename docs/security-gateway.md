---
layout: default
title: Security Gateway
---

# Security Gateway Architecture

The `security-gateway` checks agent tool and provider traffic that actually
enters Calciforge-controlled paths. It is not automatic coverage for every
process on the host.

Calciforge treats this as a support-tier question:

- **First-class agents** should have tested ingress and egress contracts. If a
  first-class adapter can receive user messages or send model/tool traffic
  around Calciforge in a protected profile, treat that as a Calciforge bug or
  an upstream limitation that needs a documented workaround.
- **Recipe, generic CLI, and generic ACP agents** are best effort unless the
  recipe documents a tested network boundary. Calciforge can give them safer
  defaults, wrapper scripts, CLI helpers, and proxy env, but it cannot prove an
  arbitrary agent runtime will not open another network path.
- **Hardened deployments** should be able to reject or disable adapters whose
  ingress, egress, or instruction path cannot be verified. That is the target
  shape for release-hardening work; for now, `calciforge doctor` reports the
  gaps it can see.

For stronger guarantees, route model calls through Calciforge's model gateway,
give agents explicit Calciforge fetch/tool wrappers, or run the agent under a
host/container boundary that prevents bypass.

## 🛡️ Traffic Flow

Outbound traffic from protected agents can be routed through the gateway by a
specific supported integration. Calciforge's own provider calls, health
checks, and LAN control-plane traffic should not use ambient
`HTTP_PROXY`/`HTTPS_PROXY`; proxying Calciforge itself can send model-gateway
requests and internal webhooks through the security proxy unnecessarily or
recursively.

**Outbound pipeline:**

1. **Manual credential check:** Before Calciforge substitutes any secrets,
   IronClaw checks the original agent-supplied URL and non-transport headers
   for raw credentials such as `api_key=sk-...`. Transport-auth headers such
   as `Authorization`, `Cookie`, and provider API-key headers are sanitized
   before this check; otherwise normal model/provider sessions and local
   gateways generate false positives. Exact
   proxy-managed placeholders such as
   `{% raw %}{{secret:NAME}}{% endraw %}` and
   `Bearer {% raw %}{{secret:NAME}}{% endraw %}` are safe control syntax;
   mixed manual-plus-placeholder values still remain visible to the check.
2. **Optional exfiltration scan:** When `scan_outbound = true`, outgoing request bodies are analyzed by the
   `adversary-detector` for exfiltration language, credential-harvest phrasing,
   and adversarial patterns. This is opt-in by default because provider/tool
   transcripts often include benign prompt-injection examples and opaque IDs.
3. **Secret substitution and credential injection:** When the request is
   visible to Calciforge, the gateway can substitute placeholders such as
   `{% raw %}{{secret:NAME}}{% endraw %}` in URLs, headers, and supported
   bodies, and inject provider `Authorization` headers from the vault.
4. **Control header strip and forwarding:** Calciforge strips
   `X-Calciforge-*` control headers, then forwards the request to the
   destination.

**Inbound pipeline:**

1. **Injection scan:** Incoming text-like response bodies are scanned for
   prompt injection or adversarial payloads. This remains default-on.
2. **Optional response secret-leak scan:** When `scan_response_secrets = true`,
   response bodies are also checked for high-entropy and secret-shaped values.
   This is opt-in by default because provider APIs commonly return opaque IDs
   and hashes as normal transport data.
3. **Enforcement:** If the response is deemed `unsafe`, the gateway blocks the
   content and returns `403 Forbidden` to the agent.

## 🚀 Deployment & Enforcement

The gateway has several enforcement modes. They are not interchangeable; pick
the strongest mode the target agent can actually run under, then verify that
the selected agent adapter actually uses it.

| Mode | Level | Status | Description |
|------|-------|--------|-------------|
| Model gateway | API | Working | Route OpenAI-compatible model calls through Calciforge's gateway. This is the most reliable path for providers and local dispatcher routes because Calciforge owns the HTTP request. |
| Explicit tools/fetch | App | Working/expanding | Give agents Calciforge-provided fetch, MCP, or recipe wrappers for network actions that need scanning or secret substitution. |
| Cooperative HTTP proxy | App | Limited | Set `HTTP_PROXY` only for agents and tools that have been tested with the proxy. This is useful for plaintext HTTP and simple HTTP clients. |
| HTTPS MITM | App/host trust | Experimental | Trust a Calciforge CA and terminate CONNECT traffic for clients that support custom trust stores. The hudsucker-backed prototype runs the existing scan/substitution pipeline over decrypted requests and responses. |
| OS redirect | Host | Roadmap | Use firewall rules such as Linux `iptables`/`nftables` or macOS `pf` to redirect outbound traffic from a controlled UID/process group to the gateway. |
| Container or VM isolation | Runtime | Roadmap | Run the agent in Docker, a Linux namespace, LXC, or a VM where egress is denied except through Calciforge-managed gateways. This is the likely path for agents that ignore proxy env or use complex transports. |
| Placeholder injection | Secret boundary | Roadmap | Give off-the-shelf agents fake env credentials and substitute real secrets only at the gateway. This keeps raw secrets out of agent memory but still needs a network enforcement path. |

The unified installer starts `security-proxy`, but it does not put
`HTTP_PROXY`/`HTTPS_PROXY` on the Calciforge service itself. Do not assume
CLI or exec-backed agents can be protected by generic proxy environment:
Codex, Claude, ACPX, npm-backed adapters, and streaming clients may use
CONNECT, WebSockets, or browser-backed authentication flows that the current
proxy cannot inspect and may break. Keep those agents unproxied unless you
have a tested wrapper for that specific runtime, and prefer OpenAI-compatible
gateway routes or explicit fetch/tool integrations for traffic that must be
scanned.

By default `security-proxy` binds to `127.0.0.1`. Keep that default for a
single-host install. For a trusted LAN deployment where other agent hosts must
use one shared proxy, set `SECURITY_PROXY_BIND=0.0.0.0` for the local installer
run, or add `"security_proxy_bind": "0.0.0.0"` to that host's node entry in
`deploy/nodes.json`. Pair a LAN bind with host firewall rules or equivalent
network restrictions when the LAN is not fully trusted.

Ambient `HTTPS_PROXY` is not a complete protection story unless it points at a
Calciforge MITM listener and the client trusts the Calciforge CA. Standard
HTTPS proxying uses CONNECT tunnels; without MITM, a proxy can only see the
destination host and encrypted bytes. Current `security-proxy` is MITM-only:
it uses hudsucker to terminate CONNECT traffic, mint per-host certificates from
the configured CA, and run the existing request/response substitution and
scanner pipeline over the decrypted HTTP messages. Prefer Calciforge-owned
model gateway routes, explicit fetch/tool integration, or audited recipe
wrappers for runtimes that cannot use the MITM trust setup.

Externally managed agent daemons are different. OpenClaw, ZeroClaw, Claude
Code, opencode, Dirac, or any custom process started by a separate service
manager must be launched with a tested proxy configuration in that service
manager, or enforced with an OS/network tier. Registering Calciforge webhooks
lets those agents talk back to Calciforge, but it does not by itself prove
their outbound HTTP is going through `security-proxy`.

For a manually started daemon that uses plaintext HTTP:

```sh
export HTTP_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

Use service-manager environment blocks for persistent daemons, and validate by
checking `security-proxy` logs while the agent makes a known outbound request.
`calciforge doctor` warns if the Calciforge daemon itself has ambient proxy
environment, flags explicit subprocess proxy env for verification, and warns
when configured HTTP/native agent daemons need separate validation.

### What Happened To `HTTP(S)_PROXY`

Calciforge did not remove proxy support; it narrowed where proxy env is treated
as a reliable security mechanism.

- `HTTP_PROXY` remains useful for tested plaintext HTTP clients. The
  OpenClaw installer path can write service proxy env via `proxy_endpoint`,
  after checking that the configured `security-proxy` is reachable from the
  OpenClaw host.
- `HTTPS_PROXY` should only be set for agent runtimes that have been tested
  with Calciforge's MITM mode and trust the configured CA. Setting it globally
  can break streaming clients, WebSockets, browser/OAuth flows, and npm-backed
  adapters.
- Browser-backed tools usually need runtime-specific wiring. Managed OpenClaw
  gets `browser.extraArgs = ["--proxy-server=..."]`; relying on ambient env is
  not enough because OpenClaw strips Chrome proxy env and otherwise starts
  Chrome with `--no-proxy-server`.
- Ambient proxy env on the Calciforge daemon itself is avoided because it can
  route Calciforge provider calls, channel callbacks, health checks, and local
  control-plane traffic through its own proxy boundary.
- Secret injection works when the request reaches Calciforge in a visible form:
  model-gateway/provider routes, explicit fetch/MCP/tool wrappers, audited
  recipes, plaintext HTTP intercept mode, or HTTPS MITM mode. It does not
  happen for an external daemon's direct HTTPS egress unless that daemon is
  configured to use Calciforge's MITM listener or another Calciforge-owned tool
  path.

### HTTPS MITM Prototype

The installer now starts `security-proxy` with the hudsucker-backed MITM
listener enabled by default and generates a persistent local CA if one does not
already exist. On macOS, the installer explains why the trust step is needed
before it asks the system to add that CA to the login keychain. This is required
for any tested browser, tool, or agent runtime that sends HTTPS traffic through
`security-proxy` and expects inspected pages without certificate errors. Set
`SECURITY_PROXY_TRUST_MITM_CA=false` to skip the keychain prompt. That makes
inspected HTTPS the default available proxy mode, but it does not automatically
make every runtime trust that CA.

To run the binary manually, use:

```sh
SECURITY_PROXY_CA_CERT=/etc/calciforge/mitm-ca.pem \
SECURITY_PROXY_CA_KEY=/etc/calciforge/mitm-ca-key.pem \
SECURITY_PROXY_PORT=8888 \
security-proxy
```

Then configure the target agent process, not the Calciforge daemon itself:

```sh
export HTTP_PROXY=http://127.0.0.1:8888
export HTTPS_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

The agent runtime must trust `mitm-ca.pem`. Depending on the runtime that can
mean the system trust store, `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`,
`NODE_EXTRA_CA_CERTS`, browser trust settings, or tool-specific configuration.
The current prototype covers explicit proxy mode; OS-level transparent
redirects and installer-managed per-runtime trust setup are next.

Practical tiers:

- Direct Mac Mini/Studio OpenClaw: use the Calciforge bridge plugin for inbound chat,
  point provider/model calls at Calciforge's model gateway where possible, and
  use `proxy_endpoint` plus MITM CA trust for tested HTTP/HTTPS egress. This is
  convenient but cooperative; OpenClaw can still bypass Calciforge if it opens
  its own direct connections outside the configured proxy environment.
- Linux service host: add systemd drop-ins, dedicated service users, and later
  `iptables`/`nftables` rules so the agent process has fewer unmanaged egress
  paths.
- Container, LXC, or VM: deny external egress except to Calciforge services.
  This is the likely preferred profile for agents that use complex transports
  or ignore proxy environment.

### Choosing A Boundary

For agents Calciforge launches as subprocesses, start with direct channel
routing plus conservative CLI flags. Add gateway coverage only through a path
that has been tested for that specific runtime:

- use `kind = "openai-compat"` or the model gateway when the work is really a
  model call;
- use artifact or recipe wrappers when the network action is a known command
  Calciforge can run and audit;
- use MCP/fetch tools when the agent can delegate web access to Calciforge;
- use container or VM isolation when the agent has broad network behavior that
  cannot be reliably proxied.

For externally managed daemons, Calciforge can authenticate inbound callbacks
and gate channel access, but it cannot prove outbound network policy unless the
daemon is launched in a controlled environment. The practical future path is a
local-lab profile that can run selected agents inside a container or VM with
egress limited to Calciforge services.

## ⚙️ Configuration

The gateway is configured via `GatewayConfig`:
- `scan_outbound`: Toggle outbound adversary/exfiltration detection. Defaults
  off while this policy matures; enable only for deployments that have tuned
  false positives on provider/tool transcripts.
- `scan_inbound`: Toggle injection detection.
- `scan_response_secrets`: Toggle high-entropy/secret-pattern response leak
  detection. Defaults off independently from prompt-injection scanning.
- `inject_credentials`: Toggle automatic API key injection.
- `manual_credential_override_requires_operator_approval`: Require an operator token for `ironclaw.manual_credential` override headers. Default: `true`.
- `bypass_domains`: List of domains that skip scanning (e.g., internal services).
- `scanner_checks`: Ordered adversary-detector checks. Empty means the built-in
  default Starlark scanner policy.

Manual credential blocks return an agent-readable explanation plus structured
headers:

- `X-Calciforge-Policy: ironclaw.manual_credential`
- `X-Calciforge-Operator-Approval: required`
- `X-Calciforge-Override-Supported: operator_scoped`
- `X-Calciforge-Override-Header: X-Calciforge-Override`

The operator override header is request-side control metadata, not upstream
API input:

```http
X-Calciforge-Override: ironclaw.manual_credential:<token>
```

With the default configuration, `<token>` must match
`SECURITY_PROXY_MANUAL_CREDENTIAL_OVERRIDE_TOKEN`. Operators can explicitly
allow self-asserted overrides by setting
`manual_credential_override_requires_operator_approval = false` in
`security-proxy.toml`, or
`SECURITY_PROXY_MANUAL_CREDENTIAL_OVERRIDE_REQUIRES_OPERATOR_APPROVAL=false`
in the service environment. Calciforge strips `X-Calciforge-*` headers before
forwarding, so override metadata is never sent to the upstream server.

## Scanner Extension Points

Calciforge's security checks are an ordered pipeline:

1. Built-in default Starlark policy — runs when `scanner_checks` is empty.
   It implements the default hidden-payload, prompt-injection, PII-harvest,
   and exfiltration checks in editable policy code.
2. `starlark` — in-process operator policy. This is the low-latency path for
   site-specific rules that do not need network calls. Policies can call
   `regex_match(pattern, content)` and
   `base64_decoded_regex_match(pattern, content)` for bounded Rust-backed
   matching.
3. `remote_http` — optional custom policy service. This is where operators can
   add an LLM classifier, heavyweight DLP checks, or organization-specific
   threat modeling that belongs outside the proxy process.

### Override and Approval Matrix

Not every gateway denial should be equally overrideable. Recommended defaults:

| Policy / block class | Configurable? | Overrideable? | Default approval |
|---|---:|---:|---|
| `ironclaw.manual_credential` — raw credential supplied by the agent | Yes | Yes, scoped header | Operator required |
| Secret substitution destination denied by `secret_destination_allowlist` or dynamic `allowed_destinations` metadata | Yes, via operator config or secret metadata | Not by agent header | Operator config/metadata change required |
| Malformed or unresolved `{% raw %}{{secret:NAME}}{% endraw %}` | No | No | Fix request or secret store |
| `agent_web.forbid_search_engines` | Yes | Prefer config only | Operator config change required |
| `agent_web.preflight_message_urls` destination denial | Yes | Prefer config only | Operator config change required |
| `agent_web.scan_search_responses` blocked result | Yes | Prefer config only | Operator config change required |
| Provider-side browsing tool stripped/blocked | Yes | Prefer config only | Operator config change required |
| Inbound prompt-injection / unsafe response scan | Yes, scanner policy | Not by agent header | Operator policy/config change required |
| Outbound exfiltration scan | Yes, scanner policy; default off | Not by agent header | Operator policy/config change required |
| Response secret-leak scan | Yes; default off | Not by agent header | Operator policy/config change required |

The reason for the split is blast radius. Manual-credential detection can be a
false positive for legacy APIs that use unfortunate parameter names, so a
scoped override is useful. Transport authentication is not governed by a
provider-host whitelist; known auth headers are sanitized before the
manual-credential scanner, and real secret movement is governed by placeholder
resolution plus destination allowlists. Destination allowlists, prompt-injection
blocks, and opt-in exfiltration/secret-leak blocks are higher-risk policy
boundaries; an agent should receive a clear explanation and ask for operator
help rather than self-override.

Calciforge can still make these policies configurable for operators. The key
rule is that configuration changes should happen in `security-proxy.toml`,
service environment, or policy files, while request-carried override metadata
stays narrowly scoped and is stripped before forwarding upstream.

Calciforge intentionally has both local and remote adversary detectors. The
local Starlark policy is for deterministic prefiltering: hidden DOM/text,
encoding, obvious exfiltration language, and concrete tool-policy bypass
patterns. The remote HTTP/LLM check is for semantic adjudication: foreign
language, poetry or other style-shift attacks, fictional framing, coercion,
multi-step decomposition, and intent that would be brittle or overbroad as
regex. The remote pass adds latency and still asks one model to defend another
model, so Calciforge keeps Starlark as the default and makes the LLM pass
explicitly configurable.

No remote service is required for the default gateway. The localhost HTTP hop is
small, but an LLM classifier call is not; enable it only when the extra security
pass is worth the added latency.

On a local release build, the built-in Starlark default scanner measured about
`299µs` per warm scan for ordinary small content. Treat that as a sanity check,
not a universal latency guarantee: large bodies, cold starts, extra configured
policies, proxy I/O, and remote LLM checks dominate real end-to-end latency.

The example prompt covers more than classic prompt injection: credential
exfiltration, malicious tool-use instructions, false authority claims, identity
spoofing, cross-agent propagation, denial-of-service attempts, destructive
cleanup, unbounded resource use, and other governance failures described by
agent red-team work such as
[`Agents of Chaos`](https://arxiv.org/abs/2602.20021).

For the standalone `security-proxy` binary, the fastest way to add a custom
remote check is:

```sh
SECURITY_PROXY_REMOTE_SCANNER_URL=http://127.0.0.1:9801 \
SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED=true \
security-proxy
```

For Calciforge channel-message scanning, use:

```sh
CALCIFORGE_REMOTE_SCANNER_URL=http://127.0.0.1:9801 \
CALCIFORGE_REMOTE_SCANNER_FAIL_CLOSED=true \
calciforge
```

The unified installer can also host the example scanner as a managed local
service:

```sh
CALCIFORGE_REMOTE_SCANNER_ENABLED=1 \
REMOTE_SCANNER_API_KEY_FILE=~/.config/calciforge/secrets/remote-scanner-api-key \
REMOTE_SCANNER_PROMPT_FILE=~/.config/calciforge/remote-llm-scanner-prompt.txt \
bash scripts/install.sh
```

When enabled, the installer starts `remote-llm-scanner` on
`127.0.0.1:9801` and sets `SECURITY_PROXY_REMOTE_SCANNER_URL` plus
`CALCIFORGE_REMOTE_SCANNER_URL` for the managed services. The API key can be
provided through `REMOTE_SCANNER_API_KEY_FILE` or `REMOTE_SCANNER_API_KEY`; the
file path is preferred so service definitions do not contain the key. The
classifier prompt is also editable: set `REMOTE_SCANNER_PROMPT_FILE` to a text
file or `REMOTE_SCANNER_PROMPT` to an inline override. The installer seeds a
default prompt file when it manages the example service.

Or configure checks directly in `config.toml`:

```toml
[security]
profile = "balanced"
scan_outbound = false
scan_response_secrets = false

# Empty scanner_checks uses the built-in Starlark default:
# builtin:calciforge/default-scanner.star
#
# To customize it, copy
# crates/adversary-detector/policies/default-scanner.star to
# /etc/calciforge/scanner-policies/default-scanner.star, edit it, then
# configure it explicitly:
#
[[security.scanner_checks]]
kind = "starlark"
path = "/etc/calciforge/scanner-policies/default-scanner.star"
fail_closed = true
max_callstack = 64

[[security.scanner_checks]]
kind = "starlark"
path = "/etc/calciforge/scanner.star"
fail_closed = true
max_callstack = 64

[[security.scanner_checks]]
kind = "remote_http"
url = "http://127.0.0.1:9801"
fail_closed = true
```

Checks are evaluated in order. A `clean` result continues to the next check.
A `review` result is retained while later checks continue, so a later
`unsafe` result can still block; `unsafe` stops the pipeline immediately.
`fail_closed` controls scanner errors or outages only: with `false`, an
unavailable optional check is skipped; successful `review` or `unsafe`
verdicts still enforce.

Starlark checks run in-process with `load()` disabled and a bounded call stack.
The policy file must define `scan(input)` and return `"clean"`, `"review"`,
`"unsafe"`, or a dict with `verdict` and optional `reason`:

```python
def scan(input):
    content = input["content"].lower()

    if input["context"] == "api" and "wire money" in content:
        return {
            "verdict": "unsafe",
            "reason": "operator policy blocks wire-transfer instructions",
        }

    return "clean"
```

Starlark policies receive `url`, `content`, `context`,
`discussion_ratio_threshold`, and `min_signals_for_ratio`. They also have
helpers backed by Rust's `regex` crate with compiled-pattern caching:
`regex_match(pattern, content)` for direct matching and
`base64_decoded_regex_match(pattern, content)` for bounded inspection of
base64-encoded text tokens. See
`crates/adversary-detector/policies/default-scanner.star` for the default
policy, `examples/security-scanner.star` for a minimal starter policy, and
`examples/scanner-policies/` for reusable examples covering destination
allowlists, destructive command patterns, and credential-language review.
`calciforge doctor --no-network` validates Starlark policy files and remote
scanner URL syntax without calling remote scanner services.

Remote checks receive the same content that would otherwise be allowed or
blocked by the local scanner:

```http
POST /scan
Content-Type: application/json

{"url":"https://api.example.com","content":"...","context":"api"}
```

They return:

```json
{"verdict":"clean|review|unsafe","reason":"short reason"}
```

`scripts/remote-llm-scanner.py` is a built-in example. It exposes `/scan` and
uses the local Calciforge model boundary by default with a strict
security-classifier prompt:

```sh
REMOTE_SCANNER_API_KEY_FILE=~/.config/calciforge/secrets/model-gateway-client-key \
REMOTE_SCANNER_API_BASE=http://127.0.0.1:18083/v1 \
REMOTE_SCANNER_MODEL=adversary/default \
REMOTE_SCANNER_PROMPT_FILE=./scripts/remote-llm-scanner-prompt.txt \
./scripts/remote-llm-scanner.py
```

Use `fail_closed = true` when the remote check is part of your enforcement
boundary. Use `fail_closed = false` for advisory classifiers where local checks
must continue to work if the remote service is unavailable.

## Custom Policy Code

There are three extension paths today:

- Rust integrations that embed `adversary-detector` can implement the
  `ScannerCheck` trait and compose their own in-process pipeline.
- Deployed Calciforge and `security-proxy` instances can load Starlark policy
  files for low-latency operator-owned logic without a sidecar service.
- Deployed Calciforge and `security-proxy` instances load arbitrary custom
  logic through the `remote_http` contract above. That keeps heavyweight code
  outside the trusted proxy process and lets users write checks in Python, Rust,
  Go, Lua, shell, or any other runtime.

Scanner code is operator-owned configuration-layer policy, so the sandbox is
not about treating the operator as hostile. It is about reliability and
blast-radius reduction: accidental recursion, dependency behavior, or unexpected
file and network access should not weaken the gateway. Starlark is the default
in-process scanner layer because it is already used by Calciforge policy code,
has no ambient filesystem or network access in this integration, supports
editable branching logic, and can use cached Rust regexes through
`regex_match()`. WebAssembly remains a possible future plugin layer when
stronger fuel and memory controls are needed. Use Starlark for local rules,
including regexes, keyword lists, size limits, allowed-language checks, or
context-specific branching; use `remote_http` when the rule needs networked
services or heavyweight dependencies.

Starter Starlark policies live under `examples/scanner-policies/`:

| Policy | Purpose |
|--------|---------|
| `allowed-destinations.star` | Review or block credential-shaped content sent outside an allowed destination list. |
| `command-denylist.star` | Block destructive shell-command patterns and review network download commands. |
| `credential-language.star` | Review or block credential disclosure, forwarding, and exfiltration language. |

Copy these into `/etc/calciforge/scanner-policies/`, edit the constants at the
top of each file, then add one or more `starlark` checks to `config.toml`.

## Testing

Integration tests are located in `crates/security-proxy/tests/`. They verify:
- Interception of adversarial content.
- Blocking of unsafe responses.
- Successful credential injection for known providers.

The scanner also has a contributor-friendly red-team fixture suite:

```sh
cargo run -p adversary-detector --example red-team
```

Fixtures live in `examples/red-team/adversary-fixtures.json`. Add cases there
when you find a bypass or false positive. Useful categories include encoded
payloads, foreign-language prompt injection, Unicode obfuscation, benign
security research, and GTFOBins/LOLBins-style instructions where a legitimate
tool is used to bypass a higher-level policy. Some fixtures can intentionally
document current gaps by expecting `clean`; hardening work should update the
fixture expectation in the same PR that improves the policy.

Good sources for new fixture families include:

- [GTFOBins](https://gtfobins.org/) and LOLBAS-style tool-policy bypasses.
- Agent-governance threat taxonomies such as `Agents of Chaos`.
- Adversarial-poetry and other style-shift jailbreak research.
- [Agent Arena](https://wiz.jock.pl/experiments/agent-arena/) hidden web-content
  cases: comments, hidden DOM nodes, microtext, ARIA, data attributes, alt text,
  off-screen content, and zero-width text.
- scurl-style sanitized-fetch middleware; see the
  [sanitized fetch roadmap](roadmap/sanitized-fetch-middleware.html).

## `[security.secret_access]` — identity-scoped secret ACLs

`[security.secret_access]` gates which secret names an identified agent,
user, or channel may discover, reference, and substitute. This is an
identity gate; `secret_destination_allowlist` and dynamic
`allowed_destinations` metadata still apply independently as destination
gates.

```toml
[security.secret_access]
[[security.secret_access.rules]]
agents = ["research-*"]
users = ["owner"]
channels = ["signal"]
secrets = ["BRAVE_*", "SEARCH_*"]
```

Rule selectors are conjunctive. Empty `agents`, `users`, or `channels`
lists are wildcards for that selector type; configured selectors must
match the active identity. `secrets` must be non-empty and supports `*`
wildcards.

Identity sources:

- MCP and `calciforge-secrets`: `CALCIFORGE_AGENT_ID`,
  `CALCIFORGE_USER_ID`, `CALCIFORGE_CHANNEL_ID`, or
  `CALCIFORGE_CHANNEL`.
- API-backed `calciforge-secrets` wrappers forward those identities to the
  central secret-control API; managed installs set `CALCIFORGE_AGENT_ID` to
  the claw name in the generated wrapper.
- security proxy: `x-calciforge-agent-id`, legacy `x-agent-id`,
  `x-calciforge-user-id`, `x-calciforge-channel-id`, or
  `x-calciforge-channel`.

Known identities fail closed: if no rule allows a secret, `list_secrets`
and `calciforge-secrets list` hide it, reference creation rejects it, and
security-proxy substitution refuses to resolve it. Unknown identities
preserve process-scoped compatibility for existing deployments. The proxy
strips Calciforge identity headers, including legacy `x-agent-id`, before
forwarding upstream.

This ACL is a read/use policy. The central `POST /control/secrets/set`
helper remains a privileged operator path guarded by the
`secret_control_api_key` and, when `allowed_destinations` are supplied,
refuses to store the secret value unless destination metadata is stored
first. It does not currently grant per-identity write permissions; treat
that as separate secret-integrity hardening before exposing write-capable
helpers broadly.

## `[security.agent_web]` — agent-web-content defenses

Calciforge's MITM gateway already scans every outbound HTTPS, but the highest-likelihood leak path for blocked content is *not* a direct egress to a denied host — it's the **search-API response** that contains pre-indexed snippets of the same denied host, or a **provider-side browsing tool** that the model invokes from inside an allowed `api.openai.com` session.

`[security.agent_web]` adds four configurable defenses against this class of leak. All default to safe values; operators opt into stricter modes.

This complements but does **not** replace `secret_destination_allowlist` or dynamic `allowed_destinations` secret metadata — those allowlists gate *secrets-into-hosts*, while `agent_web` gates *content* (search snippets, provider browsing tool defs, URLs in LLM message bodies). Static TOML policy and dynamic metadata are intersected; metadata read failures fail closed when substitution needs a destination policy decision.

### (A) `forbid_search_engines`

Block all egress to known search APIs entirely. When `true`, requests to any host matching `search_engine_patterns` are denied.

```toml
[security.agent_web]
forbid_search_engines = true
# Override the default curated list (api.search.brave.com, duckduckgo.com,
# api.tavily.com, serpapi.com, serper.dev, api.firecrawl.dev, api.you.com,
# api.exa.ai, api.kimi.com, api.minimax.com).
search_engine_patterns = ["api.search.brave.com", "api.tavily.com"]
```

### (B) `scan_search_responses`

Scan responses from search APIs for prompt-injection AND for URLs that fail the `url_destination_denylist`.

- `search_response_strategy = "block"` (default) — replace the entire response with the standard block page.
- `search_response_strategy = "strip"` — parse the JSON and drop only the offending result entries; falls back to "block" if the JSON can't be parsed.

```toml
[security.agent_web]
forbid_search_engines = false
scan_search_responses = true
search_response_strategy = "strip"
url_destination_denylist = ["leaked-corp-docs.example.com", "intranet.acme.local"]
```

### (C) `forbid_provider_browsing`

Inspect outbound LLM API request bodies and either strip or block known provider-side browsing tools (`web_search`, `web_search_preview`, `web_search_20250305`, `computer_use_*`, `google_search`, `google_search_retrieval`, `browser`, `browser_use`, …).

Always-search models (`gpt-4o-search-preview*`) cannot be stripped — they're always blocked when this is on.

- `provider_browsing_strategy = "strip"` (default) — rewrite request body to drop the tool defs.
- `provider_browsing_strategy = "block"` — refuse the request entirely.

```toml
[security.agent_web]
forbid_provider_browsing = true
provider_browsing_strategy = "strip"
# Override the curated tool / model lists if needed.
forbidden_browsing_tools = ["web_search", "web_search_20250305", "google_search"]
forbidden_browsing_models = ["gpt-4o-search-preview"]
known_llm_apis = [
    "api.openai.com",
    "chatgpt.com",
    "chat.openai.com",
    "api.anthropic.com",
    "openrouter.ai",
    "generativelanguage.googleapis.com",
    "api.groq.com",
]
```

### (D) `preflight_message_urls`

Extract `https?://...` URLs from outbound LLM request bodies for hosts in `known_llm_apis`; test each against `url_destination_denylist`. The scanner covers common shapes such as `messages[].content`, Anthropic content arrays, OpenAI Responses `input`, provider-specific nested JSON envelopes, and `tools[].description` when `preflight_tool_descriptions = true`.

If any URL would be blocked at fetch time, the LLM request is refused before forwarding to the provider. This is separate from content scanning: response scanners still inspect raw content that crosses the gateway, while URL preflight prevents opaque provider-side browsing from fetching denied origins where the gateway would otherwise only see a synthesized model summary.

```toml
[security.agent_web]
preflight_message_urls = true
preflight_tool_descriptions = true
url_destination_denylist = ["leaked-corp-docs.example.com", "ref.jock.pl"]
```

### Audit

Each policy hit emits a `tracing` INFO event with structured fields (`policy = "agent_web.<feature>"`, `dest_host`, `decision`, plus `tool`/`model`/`denied_host` when relevant) — these flow into the existing Calciforge audit pipeline.

# Security Gateway Architecture

The `security-gateway` is the mandatory network enforcement point for all Calciforge agent traffic. It replaces opt-in sidecar scanning with a fail-closed transparent proxy.

## 🛡️ Traffic Flow

All outbound HTTP/HTTPS traffic from an agent is routed through the gateway.

**Outbound Pipeline:**
1. **Exfiltration Scan**: Outgoing request bodies are analyzed by the `adversary-detector` for secrets, PII, or adversarial patterns.
2. **Credential Injection**: The gateway detects the target API (e.g., OpenAI, Anthropic) and injects the required `Authorization` headers from the vault.
3. **Forwarding**: The request is forwarded to the destination.

**Inbound Pipeline:**
1. **Injection Scan**: Incoming response bodies are scanned for prompt injection or adversarial payloads.
2. **Enforcement**: If the response is deemed `unsafe`, the gateway blocks the content and returns a `403 Forbidden` to the agent.

## 🚀 Deployment & Enforcement

The gateway can be enforced at three tiers:

| Tier | Method | Level | Description |
|------|---------|--------|-------------|
| 1 | **Cooperative** | App | Set `HTTP_PROXY` / `HTTPS_PROXY` env vars. |
| 2 | **Enforced** | OS | `iptables` redirect of ports 80/443 to gateway. |
| 3 | **Isolated** | Net | Network namespaces restricting all traffic to the gateway. |

The unified installer configures the Calciforge service with
`HTTP_PROXY`/`HTTPS_PROXY` pointing at `security-proxy`. CLI and exec-backed
agents launched as Calciforge subprocesses inherit that environment.

Externally managed agent daemons are different. OpenClaw, ZeroClaw, Claude
Code, opencode, Dirac, or any custom process started by a separate service
manager must also be launched with the same proxy environment, or enforced with
an OS/network tier. Registering Calciforge webhooks lets those agents talk back
to Calciforge, but it does not by itself prove their outbound HTTP is going
through `security-proxy`.

For a manually started daemon:

```sh
export HTTP_PROXY=http://127.0.0.1:8888
export HTTPS_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

Use service-manager environment blocks for persistent daemons, and validate by
checking `security-proxy` logs while the agent makes a known outbound request.
`calciforge doctor` reports whether Calciforge itself has a complete proxy
environment for subprocess agents and warns when configured HTTP/native agent
daemons need separate validation.

## ⚙️ Configuration

The gateway is configured via `GatewayConfig`:
- `scan_outbound`: Toggle exfiltration detection.
- `scan_inbound`: Toggle injection detection.
- `inject_credentials`: Toggle automatic API key injection.
- `bypass_domains`: List of domains that skip scanning (e.g., internal services).
- `scanner_checks`: Ordered adversary-detector checks. Empty means the built-in
  default Starlark scanner policy.

## Scanner Extension Points

Calciforge's security checks are an ordered pipeline:

1. Built-in default Starlark policy — runs when `scanner_checks` is empty.
   It implements the default hidden-payload, prompt-injection, PII-harvest,
   and exfiltration checks in editable policy code.
2. `starlark` — in-process operator policy. This is the low-latency path for
   site-specific rules that do not need network calls. Policies can call
   `regex_match(pattern, content)` for Rust-regex-backed matching.
3. `regex`, `keywords`, and `max_size` — declarative low-latency checks for
   common operator rules that should not require custom code.
4. `structural` and `semantic` — explicit Rust built-ins retained for
   compatibility and for operators who want the older split pipeline.
5. `remote_http` — optional custom policy service. This is where operators can
   add an LLM classifier, heavyweight DLP checks, or organization-specific
   threat modeling that belongs outside the proxy process.

The remote LLM check is best treated as defense in depth: a focused classifier
prompt with binary-ish `clean/review/unsafe` output can catch manipulation
patterns that simple regexes miss, but it adds latency and still asks one model
to defend another model. For that reason, Calciforge keeps the local Starlark
policy as the default, makes the LLM pass explicit, and lets operators choose
whether remote scanner outages fail open or fail closed.

No remote service is required for the default gateway. The localhost HTTP hop is
small, but an LLM classifier call is not; enable it only when the extra security
pass is worth the added latency.

On a local release build, the built-in Starlark default scanner measured about
`150µs` per warm scan for ordinary small content. Treat that as a sanity check,
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
REMOTE_SCANNER_API_KEY_FILE=~/.calciforge/secrets/remote-scanner-api-key \
bash scripts/install.sh
```

When enabled, the installer starts `remote-llm-scanner` on
`127.0.0.1:9801` and sets `SECURITY_PROXY_REMOTE_SCANNER_URL` plus
`CALCIFORGE_REMOTE_SCANNER_URL` for the managed services. The API key can be
provided through `REMOTE_SCANNER_API_KEY_FILE` or `REMOTE_SCANNER_API_KEY`; the
file path is preferred so service definitions do not contain the key.

Or configure checks directly in `config.toml`:

```toml
[security]
profile = "balanced"
scan_outbound = true

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
kind = "keywords"
terms = ["wire", "urgent"]
match_all = true
verdict = "review"
reason = "review urgent wire language"

[[security.scanner_checks]]
kind = "regex"
pattern = "\\bcredential dump\\b"
case_insensitive = true
verdict = "unsafe"
reason = "credential-dump language blocked"

[[security.scanner_checks]]
kind = "max_size"
bytes = 1048576
verdict = "review"
reason = "review unusually large content"

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

Declarative checks are evaluated in order with the rest of the scanner
pipeline. `verdict` accepts `clean`, `review`, or `unsafe`; omitted verdicts
default to `unsafe`.

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
`discussion_ratio_threshold`, and `min_signals_for_ratio`. They also have a
`regex_match(pattern, content)` helper backed by Rust's `regex` crate with
compiled-pattern caching. See
`crates/adversary-detector/policies/default-scanner.star` for the default
policy, `examples/security-scanner.star` for a minimal starter policy, and
`examples/scanner-policies/` for reusable examples covering destination
allowlists, destructive command patterns, and credential-language review.
`calciforge doctor --no-network` validates Starlark policy files, regex
syntax, keyword/max-size rule shape, and remote scanner URL syntax without
calling remote scanner services.

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
uses an OpenAI-compatible model with a strict security-classifier prompt:

```sh
REMOTE_SCANNER_API_KEY=... \
REMOTE_SCANNER_API_BASE=https://api.openai.com/v1 \
REMOTE_SCANNER_MODEL=gpt-5.4-mini \
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
stronger fuel and memory controls are needed. Declarative checks such as
regexes, keyword lists, and size limits are still available for very simple
rules. Use Starlark when a rule needs conditionals or context-specific
branching, and `remote_http` when the rule needs networked services or
heavyweight dependencies.

Starter Starlark policies live under `examples/scanner-policies/`:

| Policy | Purpose |
|--------|---------|
| `allowed-destinations.star` | Review or block credential-shaped content sent outside an allowed destination list. |
| `command-denylist.star` | Block destructive shell-command patterns and review network download commands. |
| `credential-language.star` | Review or block credential disclosure, forwarding, and exfiltration language. |

Copy these into `/etc/calciforge/scanner-policies/`, edit the constants at the
top of each file, then add one or more `starlark` checks to `config.toml`.

## 🧪 Testing

Integration tests are located in `crates/security-proxy/tests/`. They verify:
- Interception of adversarial content.
- Blocking of unsafe responses.
- Successful credential injection for known providers.

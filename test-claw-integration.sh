#!/bin/bash
# ZeroClawed Claw Integration Test
# Tests end-to-end routing: ZeroClawed → OpenClaw proxy → Kimi/Gemini/OpenRouter
#
# Usage: ./test-claw-integration.sh [host]
# Default host: 192.168.1.210

set -uo pipefail

HOST="${1:-192.168.1.210}"
SSH="ssh -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no -o ConnectTimeout=5"
OPENCLAW_PROXY="http://192.168.1.229:18789"
OPENCLAW_TOKEN="Cryptonomicon381!"

PASS=0
FAIL=0
WARN=0

pass() { echo "  ✅ $1"; ((PASS++)); }
fail() { echo "  ❌ $1"; ((FAIL++)); }
warn() { echo "  ⚠️  $1"; ((WARN++)); }

echo "╔══════════════════════════════════════════════════╗"
echo "║     ZeroClawed Claw Integration Test             ║"
echo "╚══════════════════════════════════════════════════╝"
echo "Target: $HOST"
echo "Proxy:  $OPENCLAW_PROXY"
echo ""

# ─────────────────────────────────────────────────────
# Phase 1: Connectivity
# ─────────────────────────────────────────────────────
echo "═══ Phase 1: Connectivity ═══"

if $SSH root@$HOST "echo ok" >/dev/null 2>&1; then
    pass "SSH connection to $HOST"
else
    fail "Cannot connect to $HOST"
    echo "Cannot continue without SSH. Aborting."
    exit 1
fi

if curl -sf --max-time 5 "$OPENCLAW_PROXY/v1/models" -H "Authorization: Bearer $OPENCLAW_TOKEN" > /dev/null; then
    pass "OpenClaw proxy reachable ($OPENCLAW_PROXY)"
else
    warn "OpenClaw proxy not reachable (claws may still work via fallback)"
fi

# ─────────────────────────────────────────────────────
# Phase 2: Binary Presence & Versions
# ─────────────────────────────────────────────────────
echo ""
echo "═══ Phase 2: Binary Presence ═══"

$SSH root@$HOST '
for cmd in zeptoclaw ironclaw zeroclawed; do
    if command -v $cmd >/dev/null 2>&1; then
        version=$($cmd --version 2>&1 | head -1 || echo "unknown")
        echo "OK|$cmd|$version"
    else
        echo "FAIL|$cmd|NOT FOUND"
    fi
done
' | while IFS='|' read -r status name version; do
    if [ "$status" = "OK" ]; then
        pass "$name: $version"
    else
        fail "$name: not found"
    fi
done

# ─────────────────────────────────────────────────────
# Phase 3: Config Verification
# ─────────────────────────────────────────────────────
echo ""
echo "═══ Phase 3: Configuration ═══"

# ZeptoClaw config
ZC_URL=$($SSH root@$HOST 'python3 -c "import json; c=json.load(open(\"/root/.zeptoclaw/config.json\")); print(c.get(\"providers\",{}).get(\"openai\",{}).get(\"api_base\",\"MISSING\"))" 2>/dev/null || echo "MISSING"')
if echo "$ZC_URL" | grep -q "18789"; then
    pass "ZeptoClaw → OpenClaw proxy ($ZC_URL)"
else
    fail "ZeptoClaw not using OpenClaw proxy (got: $ZC_URL)"
fi

ZC_MODEL=$($SSH root@$HOST 'python3 -c "import json; c=json.load(open(\"/root/.zeptoclaw/config.json\")); print(c.get(\"agents\",{}).get(\"defaults\",{}).get(\"model\",\"MISSING\"))" 2>/dev/null || echo "MISSING"')
if [ "$ZC_MODEL" = "openclaw:main" ]; then
    pass "ZeptoClaw model = openclaw:main"
else
    warn "ZeptoClaw model = $ZC_MODEL (expected openclaw:main)"
fi

# IronClaw config
IC_URL=$($SSH root@$HOST 'ironclaw config get openai_compatible_base_url 2>/dev/null || echo "MISSING"')
if echo "$IC_URL" | grep -q "18789"; then
    pass "IronClaw → OpenClaw proxy ($IC_URL)"
else
    fail "IronClaw not using OpenClaw proxy (got: $IC_URL)"
fi

IC_MODEL=$($SSH root@$HOST 'ironclaw config get selected_model 2>/dev/null || echo "MISSING"')
if echo "$IC_MODEL" | grep -q "openclaw"; then
    pass "IronClaw model = $IC_MODEL"
else
    warn "IronClaw model = $IC_MODEL (expected openclaw:main)"
fi

IC_BACKEND=$($SSH root@$HOST 'ironclaw config get llm_backend 2>/dev/null || echo "MISSING"')
if echo "$IC_BACKEND" | grep -q "openai_compatible"; then
    pass "IronClaw backend = openai_compatible"
else
    fail "IronClaw backend = $IC_BACKEND (expected openai_compatible)"
fi

# IronClaw API key
if $SSH root@$HOST 'grep -q "OPENAI_API_KEY.*Cryptonomicon" /root/.ironclaw/.env 2>/dev/null'; then
    pass "IronClaw API key configured"
else
    warn "IronClaw API key not in .env (may use DB settings)"
fi

# IronClaw webhook port free
if ! $SSH root@$HOST 'ss -tlnp | grep -q 8080 | grep ironclaw' 2>/dev/null; then
    pass "IronClaw webhook port 8080 available"
else
    warn "IronClaw webhook port 8080 conflict (ironclaw or other service)"
fi

# ZeroClawed config
if $SSH root@$HOST 'grep -q "id = \"zeptoclaw\"" /etc/zeroclawed/config.toml 2>/dev/null'; then
    pass "ZeroClawed has zeptoclaw agent configured"
else
    fail "ZeroClawed missing zeptoclaw agent"
fi

if $SSH root@$HOST 'grep -q "id = \"ironclaw\"" /etc/zeroclawed/config.toml 2>/dev/null'; then
    pass "ZeroClawed has ironclaw agent configured"
else
    fail "ZeroClawed missing ironclaw agent"
fi

if $SSH root@$HOST 'grep -A5 "identity = \"brian\"" /etc/zeroclawed/config.toml | grep -q "zeptoclaw"' 2>/dev/null; then
    pass "Brian's routing includes zeptoclaw"
else
    fail "Brian's routing missing zeptoclaw"
fi

if $SSH root@$HOST 'grep -A5 "identity = \"brian\"" /etc/zeroclawed/config.toml | grep -q "ironclaw"' 2>/dev/null; then
    pass "Brian's routing includes ironclaw"
else
    fail "Brian's routing missing ironclaw"
fi

# Wrapper scripts
for wrapper in zeptoclaw-openclaw ironclaw-openclaw; do
    if $SSH root@$HOST "test -x /usr/local/bin/$wrapper" 2>/dev/null; then
        pass "Wrapper /usr/local/bin/$wrapper exists and is executable"
    else
        fail "Wrapper /usr/local/bin/$wrapper missing or not executable"
    fi
done

# ─────────────────────────────────────────────────────
# Phase 4: Direct Claw Tests
# ─────────────────────────────────────────────────────
echo ""
echo "═══ Phase 4: Direct Claw Invocation ═══"

echo -n "  Testing ZeptoClaw... "
ZC_RESULT=$($SSH root@$HOST 'timeout 30 zeptoclaw agent -m "Reply with exactly: DIRECT_ZC_OK" 2>&1' 2>&1)
if echo "$ZC_RESULT" | grep -q "DIRECT_ZC_OK"; then
    pass "ZeptoClaw direct invocation works"
else
    fail "ZeptoClaw direct invocation failed"
    echo "    Error: $(echo "$ZC_RESULT" | tail -3)"
fi

echo -n "  Testing IronClaw... "
IC_RESULT=$($SSH root@$HOST 'timeout 30 ironclaw --cli-only run -m "Reply with exactly: DIRECT_IC_OK" 2>&1' 2>&1)
if echo "$IC_RESULT" | grep -q "DIRECT_IC_OK"; then
    pass "IronClaw direct invocation works"
else
    fail "IronClaw direct invocation failed"
    echo "    Error: $(echo "$IC_RESULT" | tail -3)"
fi

# ─────────────────────────────────────────────────────
# Phase 5: OpenClaw Proxy Model Routing
# ─────────────────────────────────────────────────────
echo ""
echo "═══ Phase 5: OpenClaw Proxy Model Routing ═══"

MODELS=$(curl -sf --max-time 5 "$OPENCLAW_PROXY/v1/models" -H "Authorization: Bearer $OPENCLAW_TOKEN" 2>/dev/null | python3 -c "import sys,json; [print(m['id']) for m in json.load(sys.stdin).get('data',[])]" 2>/dev/null || echo "")
if [ -n "$MODELS" ]; then
    pass "OpenClaw proxy serves models: $(echo $MODELS | tr '\n' ', ')"
else
    warn "Could not list models from OpenClaw proxy"
fi

# ─────────────────────────────────────────────────────
# Phase 6: OneCLI Integration (if available)
# ─────────────────────────────────────────────────────
echo ""
echo "═══ Phase 6: OneCLI Integration ═══"

if $SSH root@$HOST 'command -v onecli >/dev/null 2>&1' 2>/dev/null; then
    pass "OneCLI binary found"
    ONECLI_STATUS=$($SSH root@$HOST 'curl -sf --max-time 3 http://127.0.0.1:8081/health 2>/dev/null' || echo "")
    if [ -n "$ONECLI_STATUS" ]; then
        pass "OneCLI service healthy on :8081"
    else
        warn "OneCLI installed but service not running on :8081"
    fi
else
    warn "OneCLI not installed (optional — claws use direct OpenClaw proxy)"
fi

# ─────────────────────────────────────────────────────
# Summary
# ─────────────────────────────────────────────────────
echo ""
echo "╔══════════════════════════════════════════════════╗"
echo "║     Test Summary                                 ║"
echo "╠══════════════════════════════════════════════════╣"
echo "║  ✅ Passed:  $PASS                                     ║"
echo "║  ❌ Failed:  $FAIL                                     ║"
echo "║  ⚠️  Warnings: $WARN                                    ║"
echo "╚══════════════════════════════════════════════════╝"

if [ $FAIL -gt 0 ]; then
    exit 1
fi
exit 0

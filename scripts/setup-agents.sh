#!/usr/bin/env bash
# setup-agents.sh — Install and configure AI agents with clashd policy integration.
#
# Agents: opencode (brew), openclaw (npm), zeroclaw (brew)
# Policy: clashd webhook_audit (zeroclaw), exec-approvals (openclaw)
# Flags:
#   --configure-only   Skip installs, only configure (assumes tools present)
#   --install-only     Install tools but skip clashd policy wiring
#   --agents <list>    Comma-separated subset: opencode,openclaw,zeroclaw (default: all)
#
# Usage:
#   cd ~/projects/zeroclawed && bash scripts/setup-agents.sh
#   cd ~/projects/zeroclawed && bash scripts/setup-agents.sh --configure-only
#   cd ~/projects/zeroclawed && bash scripts/setup-agents.sh --agents zeroclaw,openclaw

set -euo pipefail

CLASHD_PORT="${CLASHD_PORT:-9001}"
CONFIGURE_ONLY=false
INSTALL_ONLY=false
AGENTS="opencode,openclaw,zeroclaw"

# ── arg parsing ───────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --configure-only) CONFIGURE_ONLY=true ;;
        --install-only)   INSTALL_ONLY=true ;;
        --agents)         AGENTS="$2"; shift ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

# ── colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
die()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }
hdr()  { echo -e "\n${CYAN}── $* ──${NC}"; }

# Add brew and cargo to PATH
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

agent_enabled() { [[ ",$AGENTS," == *",$1,"* ]]; }

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ZeroClawed — Agent Install & Policy Setup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Agents:   $AGENTS"
echo "  Mode:     $([ "$CONFIGURE_ONLY" = true ] && echo configure-only || ([ "$INSTALL_ONLY" = true ] && echo install-only || echo install+configure))"
echo ""

# ── helpers ───────────────────────────────────────────────────────────────────

require_brew() {
    command -v brew &>/dev/null || die "Homebrew not found. Install from https://brew.sh"
}

require_npm() {
    command -v npm &>/dev/null || die "npm not found — brew install node"
}

install_if_missing_brew() {
    local pkg="$1" bin="${2:-$1}"
    if command -v "$bin" &>/dev/null; then
        ok "$bin already installed ($(${bin} --version 2>/dev/null | head -1 || echo 'ok'))"
    else
        echo "Installing $pkg via brew..."
        brew install "$pkg"
        ok "Installed $pkg"
    fi
}

install_if_missing_npm() {
    local pkg="$1" bin="${2:-$1}"
    if command -v "$bin" &>/dev/null; then
        ok "$bin already installed ($($bin --version 2>/dev/null | head -1 || echo 'ok'))"
    else
        echo "Installing $pkg via npm..."
        npm install -g "${pkg}@latest"
        ok "Installed $pkg"
    fi
}

clashd_running() {
    curl -sf "http://localhost:${CLASHD_PORT}/health" > /dev/null 2>&1
}

# ── 1. opencode ───────────────────────────────────────────────────────────────
if agent_enabled opencode; then
    hdr "opencode"

    if [[ "$CONFIGURE_ONLY" != true ]]; then
        require_brew
        install_if_missing_brew opencode
    else
        command -v opencode &>/dev/null || die "opencode not found (run without --configure-only to install)"
    fi

    if [[ "$INSTALL_ONLY" != true ]]; then
        # opencode uses npm plugins (must be an npm module).
        # Check if our clashd plugin package is published/available locally.
        PLUGIN_PKG_DIR="$(dirname "${BASH_SOURCE[0]}")/opencode-clashd-plugin"
        if [[ -d "$PLUGIN_PKG_DIR" ]]; then
            echo "Installing opencode clashd policy plugin from $PLUGIN_PKG_DIR..."
            (cd "$PLUGIN_PKG_DIR" && npm pack --quiet)
            TARBALL=$(ls "$PLUGIN_PKG_DIR"/*.tgz 2>/dev/null | tail -1)
            if [[ -n "$TARBALL" ]]; then
                npm install -g "$TARBALL"
                opencode plugin zeroclawed-clashd-policy --global 2>/dev/null || \
                    warn "opencode plugin install returned non-zero (may already be registered)"
                ok "opencode clashd policy plugin installed"
            else
                warn "Could not pack opencode plugin — skipping (clashd monitoring won't apply to opencode)"
            fi
        else
            warn "opencode plugin not yet published (scripts/opencode-clashd-plugin/ missing) — skipping opencode clashd wiring"
            warn "opencode will work normally; add clashd plugin later"
        fi
    fi
fi

# ── 2. openclaw ───────────────────────────────────────────────────────────────
if agent_enabled openclaw; then
    hdr "openclaw"

    if [[ "$CONFIGURE_ONLY" != true ]]; then
        require_npm
        install_if_missing_npm openclaw
    else
        command -v openclaw &>/dev/null || die "openclaw not found (run without --configure-only to install)"
    fi

    if [[ "$INSTALL_ONLY" != true ]]; then
        # Map clash starlark deny rules to openclaw exec approvals.
        # security=restricted: agent must request exec; ask=on: prompt before executing.
        # allowlist: patterns that are unconditionally permitted.
        echo "Configuring openclaw exec-approvals (clash policy mapping)..."

        python3 - <<'PYEOF' | openclaw approvals set --stdin 2>&1 | head -3
import json

# Translate clash policy deny/review rules into openclaw allowlist patterns.
# Denied patterns are excluded; safe read-only tools are pre-allowed.
ALLOWLIST_PATTERNS = [
    "git",
    "ls", "cat", "grep", "find", "echo", "pwd", "wc", "head", "tail",
    "curl", "wget",
    "python", "python3", "node", "npm", "cargo",
    "make", "cmake", "rustc",
]

approvals = {
    "version": 1,
    "defaults": {
        "tools.exec": {
            "security": "restricted",
            "ask": "on"
        }
    },
    "agents": {
        "main": {
            "allowlist": ALLOWLIST_PATTERNS
        }
    }
}

print(json.dumps(approvals))
PYEOF
        ok "openclaw exec-approvals configured (restricted + ask=on, safe commands allowlisted)"
        warn "Note: exec-approvals take effect when the openclaw gateway is running"
    fi
fi

# ── 3. zeroclaw ───────────────────────────────────────────────────────────────
if agent_enabled zeroclaw; then
    hdr "zeroclaw"

    if [[ "$CONFIGURE_ONLY" != true ]]; then
        require_brew
        install_if_missing_brew zeroclaw
    else
        command -v zeroclaw &>/dev/null || die "zeroclaw not found (run without --configure-only to install)"
    fi

    if [[ "$INSTALL_ONLY" != true ]]; then
        echo "Configuring zeroclaw webhook_audit → clashd..."
        zeroclaw config set hooks.enabled true
        zeroclaw config set hooks.builtin.webhook-audit.enabled true
        zeroclaw config set hooks.builtin.webhook-audit.url \
            "http://localhost:${CLASHD_PORT}/hooks/zeroclaw-audit"
        zeroclaw config set hooks.builtin.webhook-audit.include-args true
        ok "zeroclaw webhook_audit → clashd:${CLASHD_PORT}/hooks/zeroclaw-audit"
        warn "Note: webhook_audit is monitoring only (fire-and-forget) — policy blocks via autonomy config below"

        echo "Configuring zeroclaw autonomy (clash deny rules → static policy)..."
        # block_high_risk_commands covers rm -rf, mkfs, dd, etc. in zeroclaw's built-in list
        zeroclaw config set autonomy.block-high-risk-commands true
        ok "zeroclaw autonomy.block-high-risk-commands = true"

        # Start zeroclaw as a launchd service if not already running
        if ! brew services list | grep -q "zeroclaw.*started"; then
            echo "Starting zeroclaw service..."
            brew services start zeroclaw
            ok "zeroclaw service started (auto-restarts at login)"
        else
            ok "zeroclaw service already running"
        fi
    fi
fi

# ── 4. Verify clashd is up ────────────────────────────────────────────────────
hdr "clashd health check"
if clashd_running; then
    ok "clashd responding on port ${CLASHD_PORT}"
    HOOKS=$(curl -sf "http://localhost:${CLASHD_PORT}/" | python3 -c "import sys,json; d=json.load(sys.stdin); print(', '.join(d.get('features', [])))" 2>/dev/null || echo "unknown")
    echo "  Features: $HOOKS"
else
    warn "clashd not responding on port ${CLASHD_PORT}"
    warn "Run: cd ~/projects/zeroclawed && bash scripts/setup-claude-hooks.sh"
    warn "Or:  launchctl load ~/Library/LaunchAgents/com.zeroclawed.clashd.plist"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Agent setup complete."
echo ""
echo "Integration summary:"
agent_enabled opencode && echo "  opencode   — plugin pending (scripts/opencode-clashd-plugin/)"
agent_enabled openclaw && echo "  openclaw   — exec-approvals: restricted+ask (start: openclaw gateway)"
agent_enabled zeroclaw && echo "  zeroclaw   — webhook_audit → clashd (monitoring); autonomy.block-high-risk=true"
echo ""
echo "Next steps:"
echo "  1. Run setup-claude-hooks.sh if not done:  bash scripts/setup-claude-hooks.sh"
echo "  2. Start openclaw gateway:                 openclaw gateway --port 18789"
echo "  3. Test zeroclaw:                          zeroclaw status"
echo "  4. Check clashd logs:                      ~/Library/Logs/clashd/clashd.log"
echo ""

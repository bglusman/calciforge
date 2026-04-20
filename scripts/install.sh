#!/usr/bin/env bash
# install.sh — ZeroClawed unified installer.
#
# Builds zeroclawed + clashd + security-proxy, installs all AI agents,
# wires clashd as the shared policy engine, and starts all services.
#
# Flags:
#   --yes              Non-interactive: install all missing tools automatically
#   --configure-only   Skip installs; only configure (assumes everything present)
#   --agents <list>    Comma-separated subset to include (default: all)
#                      Valid values: claude,opencode,openclaw,zeroclaw
#
# Usage:
#   cd ~/projects/zeroclawed && bash scripts/install.sh
#   cd ~/projects/zeroclawed && bash scripts/install.sh --yes
#   cd ~/projects/zeroclawed && bash scripts/install.sh --agents claude,opencode

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
CLASH_DIR="$HOME/.clash"
CLAUDE_DIR="$HOME/.claude"
CLASHD_PORT="${CLASHD_PORT:-9001}"
SECURITY_PROXY_PORT="${SECURITY_PROXY_PORT:-8888}"
PLIST_DIR="$HOME/Library/LaunchAgents"
LOG_DIR="$HOME/Library/Logs/clashd"

YES=false
CONFIGURE_ONLY=false
AGENTS="claude,opencode,openclaw,zeroclaw"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --yes)             YES=true ;;
        --configure-only)  CONFIGURE_ONLY=true ;;
        --agents)          AGENTS="$2"; shift ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$HOME/.cargo/bin:$BIN_DIR:$PATH"

# ── colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
die()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }
hdr()  { echo -e "\n${CYAN}━━ $* ━━${NC}"; }

agent_enabled() { [[ ",$AGENTS," == *",$1,"* ]]; }

# ── ask helper ────────────────────────────────────────────────────────────────
# ask_install <name> <what>: returns 0 (yes) or 1 (no)
ask_install() {
    local name="$1" what="$2"
    if [[ "$YES" == true ]]; then return 0; fi
    read -r -p "  $name not found. Install $what? [Y/n] " ans
    [[ "${ans:-Y}" =~ ^[Yy] ]]
}

# ── install helpers ───────────────────────────────────────────────────────────
require_brew() { command -v brew &>/dev/null || die "Homebrew not found — install from https://brew.sh"; }
require_npm()  { command -v npm  &>/dev/null || die "npm not found — brew install node"; }

ensure_brew() {
    local pkg="$1" bin="${2:-$1}"
    if command -v "$bin" &>/dev/null; then
        ok "$bin $(${bin} --version 2>/dev/null | head -1 || echo '(installed)')"
    elif [[ "$CONFIGURE_ONLY" == true ]]; then
        die "$bin not found — run without --configure-only to install"
    elif ask_install "$bin" "via brew install $pkg"; then
        require_brew
        echo "  Installing $pkg..."
        brew install "$pkg" 2>&1 | tail -3
        ok "$pkg installed"
    else
        warn "Skipping $bin — some features won't work"
        return 1
    fi
}

ensure_npm() {
    local pkg="$1" bin="${2:-$1}"
    if command -v "$bin" &>/dev/null; then
        ok "$bin $($bin --version 2>/dev/null | head -1 || echo '(installed)')"
    elif [[ "$CONFIGURE_ONLY" == true ]]; then
        die "$bin not found — run without --configure-only to install"
    elif ask_install "$bin" "via npm install -g $pkg"; then
        require_npm
        echo "  Installing $pkg..."
        npm install -g "${pkg}@latest" 2>&1 | grep -E "^added|^npm warn deprecated" | tail -3
        ok "$pkg installed"
    else
        warn "Skipping $bin — some features won't work"
        return 1
    fi
}

# ── banner ────────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ZeroClawed — Unified Installer"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Agents:  $AGENTS"
echo "  Mode:    $([ "$CONFIGURE_ONLY" = true ] && echo configure-only || echo install+configure)"
echo "  Yes:     $YES"
echo ""

# ══════════════════════════════════════════════════════════════════════════════
# 1. Build + install zeroclawed, clashd, security-proxy
# ══════════════════════════════════════════════════════════════════════════════
if [[ "$CONFIGURE_ONLY" != true ]]; then
    hdr "Building ZeroClawed binaries"
    CARGO="$HOME/.cargo/bin/cargo"
    [[ -x "$CARGO" ]] || die "cargo not found — install Rust from https://rustup.rs"

    "$CARGO" build --release -p clashd -p zeroclawed -p security-proxy 2>&1 \
        | grep -E "^error|Compiling (clashd|zeroclawed|security.proxy)|Finished" || true

    mkdir -p "$BIN_DIR"
    for bin in clashd zeroclawed security-proxy; do
        src="$REPO_ROOT/target/release/$bin"
        [[ -f "$src" ]] || { warn "Binary not found: $src (build may have failed)"; continue; }
        cp "$src" "$BIN_DIR/$bin"
        chmod +x "$BIN_DIR/$bin"
        ok "Installed $bin → $BIN_DIR/$bin"
    done

    [[ ":$PATH:" != *":$BIN_DIR:"* ]] && \
        warn "$BIN_DIR not in PATH — add: export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

# ══════════════════════════════════════════════════════════════════════════════
# 2. clashd — policy engine (launchd service)
# ══════════════════════════════════════════════════════════════════════════════
hdr "clashd policy engine"

mkdir -p "$CLASH_DIR" "$LOG_DIR" "$PLIST_DIR"

CLASHD_POLICY="${CLASHD_POLICY:-$CLASH_DIR/policy.star}"
AGENTS_JSON="$CLASH_DIR/agents.json"

if [[ ! -f "$CLASHD_POLICY" ]]; then
    cp "$REPO_ROOT/crates/clashd/config/claude-code-policy.star" "$CLASHD_POLICY"
    ok "Policy installed → $CLASHD_POLICY"
else
    ok "Policy already present → $CLASHD_POLICY"
fi

[[ -f "$AGENTS_JSON" ]] || \
    { cp "$REPO_ROOT/crates/clashd/config/agents.example.json" "$AGENTS_JSON" 2>/dev/null || \
      echo '{"agents":[]}' > "$AGENTS_JSON"; ok "Agent config → $AGENTS_JSON"; }

CLASHD_PLIST="$PLIST_DIR/com.zeroclawed.clashd.plist"
cat > "$CLASHD_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.zeroclawed.clashd</string>
    <key>ProgramArguments</key><array><string>${BIN_DIR}/clashd</string></array>
    <key>EnvironmentVariables</key><dict>
        <key>CLASHD_PORT</key><string>${CLASHD_PORT}</string>
        <key>CLASHD_POLICY</key><string>${CLASHD_POLICY}</string>
        <key>CLASHD_AGENTS</key><string>${AGENTS_JSON}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${LOG_DIR}/clashd.log</string>
    <key>StandardErrorPath</key><string>${LOG_DIR}/clashd.err</string>
</dict></plist>
EOF

launchctl unload "$CLASHD_PLIST" 2>/dev/null || true
launchctl load "$CLASHD_PLIST"

sleep 1
curl -sf "http://localhost:${CLASHD_PORT}/health" > /dev/null \
    && ok "clashd running on :${CLASHD_PORT}" \
    || warn "clashd not yet responding — check $LOG_DIR/clashd.err"

# ══════════════════════════════════════════════════════════════════════════════
# 3. security-proxy (launchd service)
# ══════════════════════════════════════════════════════════════════════════════
hdr "security-proxy"

SEC_PLIST="$PLIST_DIR/com.zeroclawed.security-proxy.plist"
SEC_LOG_DIR="$HOME/Library/Logs/security-proxy"
mkdir -p "$SEC_LOG_DIR"

cat > "$SEC_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.zeroclawed.security-proxy</string>
    <key>ProgramArguments</key><array><string>${BIN_DIR}/security-proxy</string></array>
    <key>EnvironmentVariables</key><dict>
        <key>SECURITY_PROXY_PORT</key><string>${SECURITY_PROXY_PORT}</string>
        <key>AGENT_CONFIG</key><string>${AGENTS_JSON}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${SEC_LOG_DIR}/security-proxy.log</string>
    <key>StandardErrorPath</key><string>${SEC_LOG_DIR}/security-proxy.err</string>
</dict></plist>
EOF

launchctl unload "$SEC_PLIST" 2>/dev/null || true
launchctl load "$SEC_PLIST"

sleep 1
curl -sf "http://localhost:${SECURITY_PROXY_PORT}/health" > /dev/null \
    && ok "security-proxy running on :${SECURITY_PROXY_PORT}" \
    || warn "security-proxy not yet responding — check $SEC_LOG_DIR/security-proxy.err"

# ══════════════════════════════════════════════════════════════════════════════
# 4. Claude Code hook
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled claude; then
    hdr "Claude Code"

    if ! command -v claude &>/dev/null; then
        if ask_install "claude" "Claude Code CLI (npm install -g @anthropic-ai/claude-code)"; then
            require_npm
            npm install -g @anthropic-ai/claude-code
            ok "Claude Code installed"
        else
            warn "Skipping Claude Code hook wiring"
        fi
    else
        ok "claude $(claude --version 2>/dev/null | head -1 || echo '(installed)')"
    fi

    if command -v claude &>/dev/null; then
        SETTINGS="$CLAUDE_DIR/settings.json"
        if [[ ! -f "$SETTINGS" ]]; then
            warn "Claude settings not found at $SETTINGS — start Claude Code once first"
        else
            python3 - "$SETTINGS" "$CLASHD_PORT" <<'PYEOF'
import json, sys
path, port = sys.argv[1], sys.argv[2]
with open(path) as f: s = json.load(f)
entry = {"matcher": "", "hooks": [{"type": "command",
    "command": f"curl -sf -X POST http://localhost:{port}/hooks/claude-code "
               f"-H 'Content-Type: application/json' -d @-"}]}
hooks = s.setdefault("hooks", {})
pre = hooks.setdefault("PreToolUse", [])
pre[:] = [h for h in pre if not any("hooks/claude-code" in str(x.get("command",""))
          for x in h.get("hooks", []))]
pre.insert(0, entry)
with open(path, "w") as f: json.dump(s, f, indent=2); f.write("\n")
print("settings.json updated")
PYEOF
            ok "Claude Code PreToolUse hook → clashd:${CLASHD_PORT}"
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 5. opencode
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled opencode; then
    hdr "opencode"
    ensure_brew opencode || true

    PLUGIN_DIR="$REPO_ROOT/scripts/opencode-clashd-plugin"
    if [[ -d "$PLUGIN_DIR" ]]; then
        (cd "$PLUGIN_DIR" && npm pack --quiet 2>/dev/null)
        TARBALL=$(ls "$PLUGIN_DIR"/*.tgz 2>/dev/null | tail -1)
        [[ -n "$TARBALL" ]] && opencode plugin zeroclawed-clashd-policy --global 2>/dev/null \
            && ok "opencode clashd plugin registered" \
            || warn "opencode plugin not registered (run opencode plugin zeroclawed-clashd-policy manually)"
    else
        warn "opencode clashd plugin not yet built (scripts/opencode-clashd-plugin/ pending)"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 6. openclaw
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled openclaw; then
    hdr "openclaw"
    ensure_npm openclaw || true

    if command -v openclaw &>/dev/null; then
        python3 - <<'PYEOF' | openclaw approvals set --stdin 2>&1 | head -2
import json
print(json.dumps({"version":1,"defaults":{"tools.exec":{"security":"restricted","ask":"on"}},
    "agents":{"main":{"allowlist":["git","ls","cat","grep","find","echo","pwd",
        "wc","head","tail","curl","wget","python","python3","node","npm","cargo",
        "make","cmake","rustc"]}}}))
PYEOF
        ok "openclaw exec-approvals configured (restricted+ask, common tools allowlisted)"
        warn "Start openclaw gateway: openclaw gateway --port 18789"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 7. zeroclaw
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled zeroclaw; then
    hdr "zeroclaw"
    ensure_brew zeroclaw || true

    if command -v zeroclaw &>/dev/null; then
        zeroclaw config set hooks.enabled true 2>/dev/null
        zeroclaw config set hooks.builtin.webhook-audit.enabled true 2>/dev/null
        zeroclaw config set hooks.builtin.webhook-audit.url \
            "http://localhost:${CLASHD_PORT}/hooks/zeroclaw-audit" 2>/dev/null
        zeroclaw config set hooks.builtin.webhook-audit.include-args true 2>/dev/null
        zeroclaw config set autonomy.block-high-risk-commands true 2>/dev/null
        ok "zeroclaw webhook_audit → clashd:${CLASHD_PORT}"

        if zeroclaw doctor 2>/dev/null | grep -q "no default_provider"; then
            warn "zeroclaw needs a provider configured before starting"
            warn "Run: zeroclaw onboard"
        elif ! brew services list | grep -q "zeroclaw.*started"; then
            brew services start zeroclaw 2>/dev/null \
                && ok "zeroclaw service started" \
                || warn "Could not start zeroclaw service — run: zeroclaw daemon"
        else
            ok "zeroclaw service already running"
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# Summary
# ══════════════════════════════════════════════════════════════════════════════
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Installation complete. Service status:"
echo ""
curl -sf "http://localhost:${CLASHD_PORT}/health"      > /dev/null 2>&1 && echo "  ✓ clashd          :${CLASHD_PORT}" || echo "  ✗ clashd          :${CLASHD_PORT}  (check $LOG_DIR/clashd.err)"
curl -sf "http://localhost:${SECURITY_PROXY_PORT}/health" > /dev/null 2>&1 && echo "  ✓ security-proxy  :${SECURITY_PROXY_PORT}" || echo "  ✗ security-proxy  :${SECURITY_PROXY_PORT}  (check $SEC_LOG_DIR/security-proxy.err)"
agent_enabled zeroclaw && (zeroclaw status 2>/dev/null | grep -q "running" \
    && echo "  ✓ zeroclaw" || echo "  ✗ zeroclaw  (run: zeroclaw onboard, then: zeroclaw daemon)")
echo ""
echo "To route Claude Code web requests through security-proxy, add to ~/.zshrc:"
echo "  export HTTP_PROXY=http://localhost:${SECURITY_PROXY_PORT}"
echo "  export HTTPS_PROXY=http://localhost:${SECURITY_PROXY_PORT}"
echo ""
echo "Logs:"
echo "  clashd:         $LOG_DIR/"
echo "  security-proxy: $SEC_LOG_DIR/"
echo "  Policy:         $CLASHD_POLICY"
echo ""

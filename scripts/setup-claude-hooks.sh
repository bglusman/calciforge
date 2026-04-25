#!/usr/bin/env bash
# setup-claude-hooks.sh — Wire clashd as a Claude Code PreToolUse policy engine.
#
# What this does:
#   1. Builds clashd and calciforge (release profile)
#   2. Installs binaries to ~/.local/bin/
#   3. Creates ~/.clash/ with a Claude Code-specific policy
#   4. Installs a launchd service so clashd starts at login (macOS)
#   5. Updates ~/.claude/settings.json with the PreToolUse hook
#
# Usage:
#   cd ~/projects/calciforge && bash scripts/setup-claude-hooks.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
CLASH_DIR="$HOME/.clash"
CLAUDE_DIR="$HOME/.claude"
CLASHD_PORT="${CLASHD_PORT:-9001}"
CLASHD_POLICY="${CLASHD_POLICY:-$CLASH_DIR/policy.star}"
PLIST_LABEL="com.calciforge.clashd"
PLIST_PATH="$HOME/Library/LaunchAgents/$PLIST_LABEL.plist"

# ── colours ──────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
die()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Calciforge × Claude Code — policy hook setup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── 1. Build ──────────────────────────────────────────────────────────────────
echo "Building clashd and calciforge (release)..."
CARGO="${HOME}/.cargo/bin/cargo"
[[ -x "$CARGO" ]] || die "cargo not found at $CARGO"

if ! "$CARGO" build --release -p clashd -p calciforge > /tmp/calciforge-build.log 2>&1; then
    grep "^error" /tmp/calciforge-build.log || true
    die "cargo build failed — full log at /tmp/calciforge-build.log"
fi
grep -E "Compiling|Finished" /tmp/calciforge-build.log || true
ok "Build complete"

# ── 2. Install binaries ───────────────────────────────────────────────────────
mkdir -p "$BIN_DIR"
cp "$REPO_ROOT/target/release/clashd" "$BIN_DIR/clashd"
cp "$REPO_ROOT/target/release/calciforge" "$BIN_DIR/calciforge"
chmod +x "$BIN_DIR/clashd" "$BIN_DIR/calciforge"
ok "Installed clashd → $BIN_DIR/clashd"
ok "Installed calciforge → $BIN_DIR/calciforge"

if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    warn "$BIN_DIR is not in PATH — add: export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

# ── 3. Create ~/.clash/ policy ────────────────────────────────────────────────
mkdir -p "$CLASH_DIR"

if [[ -f "$CLASHD_POLICY" ]]; then
    warn "Policy already exists at $CLASHD_POLICY — skipping (won't overwrite)"
else
    cp "$REPO_ROOT/crates/clashd/config/claude-code-policy.star" "$CLASHD_POLICY"
    ok "Policy installed → $CLASHD_POLICY"
fi

# Copy agents.json example if none exists
AGENTS_JSON="$CLASH_DIR/agents.json"
if [[ ! -f "$AGENTS_JSON" ]]; then
    cp "$REPO_ROOT/crates/clashd/config/agents.example.json" "$AGENTS_JSON" 2>/dev/null || \
    echo '{"agents": []}' > "$AGENTS_JSON"
    ok "Agent config → $AGENTS_JSON"
fi

# ── 4. service setup ──────────────────────────────────────────────────────────
if [[ "$(uname)" == "Darwin" ]]; then
    mkdir -p "$HOME/Library/LaunchAgents"
    LOG_DIR="$HOME/Library/Logs/clashd"
    mkdir -p "$LOG_DIR"

    cat > "$PLIST_PATH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${PLIST_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${BIN_DIR}/clashd</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>CLASHD_PORT</key>
        <string>${CLASHD_PORT}</string>
        <key>CLASHD_POLICY</key>
        <string>${CLASHD_POLICY}</string>
        <key>CLASHD_AGENTS</key>
        <string>${AGENTS_JSON}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${LOG_DIR}/clashd.log</string>
    <key>StandardErrorPath</key>
    <string>${LOG_DIR}/clashd.err</string>
</dict>
</plist>
EOF
    ok "LaunchAgent plist → $PLIST_PATH"

    if launchctl list | grep -q "$PLIST_LABEL" 2>/dev/null; then
        launchctl unload "$PLIST_PATH" 2>/dev/null || true
    fi
    launchctl load "$PLIST_PATH"
    ok "clashd service loaded (auto-starts at login)"

    sleep 1
    if curl -sf "http://localhost:${CLASHD_PORT}/health" > /dev/null 2>&1; then
        ok "clashd is running on port ${CLASHD_PORT}"
    else
        warn "clashd not yet responding — check $LOG_DIR/clashd.err"
    fi
else
    LOG_DIR="/var/log/clashd"
    warn "Non-macOS: skipping launchd. Start clashd manually or add a systemd unit:"
    warn "  ExecStart=$BIN_DIR/clashd"
    warn "  Environment=CLASHD_PORT=$CLASHD_PORT CLASHD_POLICY=$CLASHD_POLICY"
fi

# ── 5. Update ~/.claude/settings.json ────────────────────────────────────────
SETTINGS="$CLAUDE_DIR/settings.json"

if [[ ! -f "$SETTINGS" ]]; then
    die "Claude settings not found at $SETTINGS — is Claude Code installed?"
fi

# Use python3 to merge the hook into existing settings (preserves all other keys)
python3 - "$SETTINGS" "$CLASHD_PORT" <<'PYEOF'
import json, sys

settings_path = sys.argv[1]
port = sys.argv[2]

with open(settings_path) as f:
    settings = json.load(f)

hook_entry = {
    "matcher": "",
    "hooks": [
        {
            "type": "command",
            "command": f"curl -sf -X POST http://localhost:{port}/hooks/claude-code "
                       f"-H 'Content-Type: application/json' -d @-"
        }
    ]
}

hooks = settings.setdefault("hooks", {})
pre_tool_use = hooks.setdefault("PreToolUse", [])

# Remove any existing clashd hook entries before adding fresh one
pre_tool_use[:] = [
    h for h in pre_tool_use
    if not any(
        "hooks/claude-code" in str(hook.get("command", ""))
        for hook in h.get("hooks", [])
    )
]
pre_tool_use.insert(0, hook_entry)

with open(settings_path, "w") as f:
    json.dump(settings, f, indent=2)
    f.write("\n")

print("settings.json updated")
PYEOF

ok "Claude Code hook registered in $SETTINGS"

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "All done. Claude Code will now check every tool call"
echo "against clashd before executing."
echo ""
echo "  Policy:  $CLASHD_POLICY"
echo "  Logs:    $LOG_DIR/clashd.log"
echo "  Test:    curl http://localhost:${CLASHD_PORT}/health"
echo ""
echo "To adjust policy, edit $CLASHD_POLICY and restart clashd."
if [[ "$(uname)" == "Darwin" ]]; then
    echo "  launchctl unload $PLIST_PATH"
    echo "  launchctl load   $PLIST_PATH"
fi
echo ""

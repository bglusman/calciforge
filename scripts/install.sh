#!/usr/bin/env bash
# install.sh — ZeroClawed unified installer.
#
# Builds zeroclawed + clashd + security-proxy, installs all AI agents,
# wires clashd as the shared policy engine, and starts all services.
# Supports multi-node SSH deployment for homelab / Proxmox clusters.
#
# Flags:
#   --yes                Non-interactive: install all missing tools automatically
#   --configure-only     Skip builds; only configure (assumes everything present)
#   --agents <list>      Comma-separated subset: claude,opencode,openclaw,zeroclaw
#   --nodes-file <path>  JSON file listing SSH nodes to deploy to after local install
#                        (see deploy/nodes.example.json)
#   --nodes-only         Skip local install; only deploy to remote nodes
#
# Usage:
#   cd ~/projects/zeroclawed && bash scripts/install.sh
#   cd ~/projects/zeroclawed && bash scripts/install.sh --yes
#   cd ~/projects/zeroclawed && bash scripts/install.sh --nodes-file deploy/nodes.json
#   cd ~/projects/zeroclawed && bash scripts/install.sh --nodes-file deploy/nodes.json --nodes-only

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
NODES_ONLY=false
NODES_FILE=""
AGENTS="claude,opencode,openclaw,zeroclaw"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --yes)             YES=true ;;
        --configure-only)  CONFIGURE_ONLY=true ;;
        --nodes-file)      NODES_FILE="$2"; shift ;;
        --nodes-only)      NODES_ONLY=true ;;
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
# 8. Multi-node SSH deployment
# ══════════════════════════════════════════════════════════════════════════════

if [[ -n "$NODES_FILE" ]]; then
    hdr "Multi-node deployment"

    [[ -f "$NODES_FILE" ]] || die "Nodes file not found: $NODES_FILE"
    command -v python3 &>/dev/null || die "python3 required for node deployment"

    # ── binary build cache: arch → path ──────────────────────────────────────
    # Maps "x86_64-unknown-linux-musl" → /path/to/built/binary
    declare -A BUILT=()

    build_for_arch() {
        local target="$1" bin="$2"
        local cache_key="${target}:${bin}"
        [[ -n "${BUILT[$cache_key]+_}" ]] && { echo "${BUILT[$cache_key]}"; return; }

        local out_path="$REPO_ROOT/target/${target}/release/${bin}"

        if [[ "$target" == "aarch64-apple-darwin" ]]; then
            # Native — use already-built binary if present
            local native="$REPO_ROOT/target/release/${bin}"
            if [[ -f "$native" ]]; then
                BUILT[$cache_key]="$native"
                echo "$native"; return
            fi
        fi

        echo "  Building $bin for $target..." >&2
        if command -v cross &>/dev/null; then
            cross build --release -p "$bin" --target "$target" 2>&1 | \
                grep -E "^error|Finished" >&2 || true
        elif command -v cargo-zigbuild &>/dev/null; then
            cargo zigbuild --release -p "$bin" --target "$target" 2>&1 | \
                grep -E "^error|Finished" >&2 || true
        else
            warn "No cross-compilation tool found (install 'cross' or 'cargo-zigbuild')" >&2
            echo ""; return 1
        fi

        [[ -f "$out_path" ]] && BUILT[$cache_key]="$out_path" && echo "$out_path" || \
            { warn "Build failed for $target/$bin"; echo ""; return 1; }
    }

    # ── systemd unit generator ────────────────────────────────────────────────
    systemd_unit() {
        local bin="$1" install_dir="$2" env_pairs="$3"
        local env_lines=""
        while IFS='=' read -r k v; do
            env_lines+="Environment=\"${k}=${v}\"\n"
        done <<< "$env_pairs"

        printf '[Unit]\nDescription=ZeroClawed %s\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=%s/%s\n%sRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n' \
            "$bin" "$install_dir" "$bin" "$(printf '%b' "$env_lines")"
    }

    # ── launchd plist generator ───────────────────────────────────────────────
    launchd_plist() {
        local bin="$1" install_dir="$2" label="com.zeroclawed.${bin}" log_dir="$3"
        local env_block=""
        shift 3
        for pair in "$@"; do
            local k="${pair%%=*}" v="${pair#*=}"
            env_block+="        <key>${k}</key><string>${v}</string>\n"
        done

        printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>\n    <key>Label</key><string>%s</string>\n    <key>ProgramArguments</key><array><string>%s/%s</string></array>\n    <key>EnvironmentVariables</key><dict>\n%s    </dict>\n    <key>RunAtLoad</key><true/>\n    <key>KeepAlive</key><true/>\n    <key>StandardOutPath</key><string>%s/%s.log</string>\n    <key>StandardErrorPath</key><string>%s/%s.err</string>\n</dict></plist>\n' \
            "$label" "$install_dir" "$bin" "$(printf '%b' "$env_block")" \
            "$log_dir" "$bin" "$log_dir" "$bin"
    }

    # ── deploy one service to one node ───────────────────────────────────────
    deploy_service() {
        local name="$1" host="$2" user="$3" ssh_key="$4" arch="$5" os="$6"
        local bin="$7" install_dir="$8" config_dir="$9"

        local ssh_opts="-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"
        [[ -n "$ssh_key" ]] && ssh_opts+=" -i $ssh_key"
        local ssh_target="${user}@${host}"

        echo "  [$name] deploying $bin..."

        # ── get binary ───────────────────────────────────────────────────────
        local bin_path
        bin_path=$(build_for_arch "$arch" "$bin") || {
            # Cross-compile failed — try building on remote
            warn "  [$name] cross-compile unavailable; attempting remote build..."
            ssh $ssh_opts "$ssh_target" bash -s -- "$bin" "$install_dir" <<'REMOTE_BUILD'
set -e
BIN=$1; INSTALL_DIR=$2
command -v cargo &>/dev/null || {
    curl -sf https://sh.rustup.rs | sh -s -- -y --quiet
    source "$HOME/.cargo/env"
}
TMP=$(mktemp -d)
# Expect the source to be pre-rsynced or pull from git
if [[ -d /opt/zeroclawed ]]; then
    cd /opt/zeroclawed && cargo build --release -p "$BIN" 2>&1 | tail -3
    cp "target/release/$BIN" "$INSTALL_DIR/$BIN"
fi
REMOTE_BUILD
            ok "  [$name] $bin built and installed on remote"
            return
        }

        [[ -z "$bin_path" || ! -f "$bin_path" ]] && {
            warn "  [$name] no binary available for $bin on $arch — skipping"
            return
        }

        # ── rsync binary ─────────────────────────────────────────────────────
        ssh $ssh_opts "$ssh_target" "mkdir -p $install_dir" 2>/dev/null
        rsync -az --checksum -e "ssh $ssh_opts" "$bin_path" "${ssh_target}:${install_dir}/${bin}"
        ssh $ssh_opts "$ssh_target" "chmod +x ${install_dir}/${bin}"

        # ── rsync config files ────────────────────────────────────────────────
        if [[ "$bin" == "clashd" ]]; then
            ssh $ssh_opts "$ssh_target" "mkdir -p $config_dir"
            rsync -az -e "ssh $ssh_opts" \
                "$REPO_ROOT/crates/clashd/config/claude-code-policy.star" \
                "${ssh_target}:${config_dir}/policy.star" 2>/dev/null || true
            # Write minimal agents.json if absent
            ssh $ssh_opts "$ssh_target" \
                "[[ -f ${config_dir}/agents.json ]] || echo '{\"agents\":[]}' > ${config_dir}/agents.json"
        fi

        # ── install service ───────────────────────────────────────────────────
        local remote_log_dir
        if [[ "$os" == "linux" ]]; then
            remote_log_dir="/var/log/zeroclawed"
            local env_pairs unit_content
            case "$bin" in
                clashd)         env_pairs="CLASHD_PORT=${CLASHD_PORT}\nCLASHD_POLICY=${config_dir}/policy.star\nCLASHD_AGENTS=${config_dir}/agents.json" ;;
                security-proxy) env_pairs="SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}\nAGENT_CONFIG=${config_dir}/agents.json" ;;
                zeroclawed)     env_pairs="" ;;
            esac
            unit_content=$(systemd_unit "$bin" "$install_dir" "$(printf '%b' "$env_pairs")")
            ssh $ssh_opts "$ssh_target" "mkdir -p $remote_log_dir && cat > /etc/systemd/system/${bin}.service" <<< "$unit_content"
            ssh $ssh_opts "$ssh_target" "systemctl daemon-reload && systemctl enable --now ${bin}" 2>&1 | tail -2
        else
            remote_log_dir="\$HOME/Library/Logs/zeroclawed"
            local plist_content label="com.zeroclawed.${bin}"
            plist_content=$(launchd_plist "$bin" "$install_dir" "$remote_log_dir" \
                "CLASHD_PORT=${CLASHD_PORT}" "SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}")
            local plist_path="\$HOME/Library/LaunchAgents/${label}.plist"
            ssh $ssh_opts "$ssh_target" "mkdir -p \$HOME/Library/LaunchAgents \$HOME/Library/Logs/zeroclawed"
            ssh $ssh_opts "$ssh_target" "cat > ${plist_path}" <<< "$plist_content"
            ssh $ssh_opts "$ssh_target" "launchctl unload ${plist_path} 2>/dev/null; launchctl load ${plist_path}"
        fi

        ok "  [$name] $bin deployed and started"
    }

    # ── iterate nodes from JSON ───────────────────────────────────────────────
    python3 - "$NODES_FILE" <<'PYEOF' | while IFS='|' read -r name host user ssh_key arch os services install_dir config_dir; do
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
for n in data.get("nodes", []):
    print("|".join([
        n.get("name", n["host"]),
        n["host"],
        n.get("user", "root"),
        n.get("ssh_key", ""),
        n.get("arch", "x86_64-unknown-linux-musl"),
        n.get("os", "linux"),
        ",".join(n.get("services", ["clashd","security-proxy"])),
        n.get("install_dir", "/usr/local/bin"),
        n.get("config_dir", "/etc/zeroclawed"),
    ]))
PYEOF
        echo ""
        echo "  Node: $name ($user@$host, $arch, $os)"
        IFS=',' read -ra svc_list <<< "$services"
        for svc in "${svc_list[@]}"; do
            deploy_service "$name" "$host" "$user" "$ssh_key" "$arch" "$os" \
                "$svc" "$install_dir" "$config_dir" || true
        done
    done
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

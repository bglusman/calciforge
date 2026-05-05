#!/usr/bin/env bash
# install.sh — Calciforge unified installer.
#
# Builds calciforge + clashd + security-proxy, installs all AI agents,
# wires clashd as the shared policy engine, and starts all services.
# Supports multi-node SSH deployment for homelab / Proxmox clusters.
#
# Flags:
#   --yes                Non-interactive: install all missing tools automatically
#   --configure-only     Skip builds; only configure (assumes everything present)
#   --agents-only        Skip core binary builds and service setup; only install agent runtimes
#   --agents <list>      Comma-separated subset: claude,opencode,openclaw,zeroclaw,ironclaw,hermes,dirac
#   --nodes-file <path>  JSON file listing SSH nodes to deploy to after local install
#                        (see deploy/nodes.example.json)
#   --nodes-only         Skip local install; only deploy to remote nodes
#
# Usage:
#   cd ~/projects/calciforge && bash scripts/install.sh
#   cd ~/projects/calciforge && bash scripts/install.sh --yes
#   cd ~/projects/calciforge && bash scripts/install.sh --nodes-file deploy/nodes.json
#   cd ~/projects/calciforge && bash scripts/install.sh --nodes-file deploy/nodes.json --nodes-only

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLASH_DIR="$HOME/.clash"
CLAUDE_DIR="$HOME/.claude"
CALCIFORGE_CONFIG_HOME="${CALCIFORGE_CONFIG_HOME:-${XDG_CONFIG_HOME:-$HOME/.config}/calciforge}"
CLASHD_PORT="${CLASHD_PORT:-9001}"
SECURITY_PROXY_PORT="${SECURITY_PROXY_PORT:-8888}"
SECURITY_PROXY_BIND="${SECURITY_PROXY_BIND:-127.0.0.1}"
SECURITY_PROXY_URL="${SECURITY_PROXY_URL:-http://127.0.0.1:${SECURITY_PROXY_PORT}}"
SECURITY_PROXY_NO_PROXY="${SECURITY_PROXY_NO_PROXY:-localhost,127.0.0.1,::1}"
SECURITY_PROXY_MITM_ENABLED="${SECURITY_PROXY_MITM_ENABLED:-true}"
SECURITY_PROXY_CA_CERT="${SECURITY_PROXY_CA_CERT:-$CALCIFORGE_CONFIG_HOME/secrets/mitm-ca.pem}"
SECURITY_PROXY_CA_KEY="${SECURITY_PROXY_CA_KEY:-$CALCIFORGE_CONFIG_HOME/secrets/mitm-ca-key.pem}"
SECURITY_PROXY_TRUST_MITM_CA="${SECURITY_PROXY_TRUST_MITM_CA:-true}"
CALCIFORGE_MANAGED_OPENCLAW="${CALCIFORGE_MANAGED_OPENCLAW:-true}"
CALCIFORGE_OPENCLAW_NAME="${CALCIFORGE_OPENCLAW_NAME:-openclaw-local}"
CALCIFORGE_OPENCLAW_AGENT_ID="${CALCIFORGE_OPENCLAW_AGENT_ID:-main}"
CALCIFORGE_OPENCLAW_PORT="${CALCIFORGE_OPENCLAW_PORT:-18789}"
CALCIFORGE_OPENCLAW_ENDPOINT="${CALCIFORGE_OPENCLAW_ENDPOINT:-http://127.0.0.1:${CALCIFORGE_OPENCLAW_PORT}}"
CALCIFORGE_OPENCLAW_REPLY_WEBHOOK="${CALCIFORGE_OPENCLAW_REPLY_WEBHOOK:-http://127.0.0.1:18797/hooks/reply}"
CALCIFORGE_OPENCLAW_AUTH_TOKEN_FILE="${CALCIFORGE_OPENCLAW_AUTH_TOKEN_FILE:-$CALCIFORGE_CONFIG_HOME/secrets/openclaw-inbound-token}"
CALCIFORGE_OPENCLAW_REPLY_TOKEN_FILE="${CALCIFORGE_OPENCLAW_REPLY_TOKEN_FILE:-$CALCIFORGE_CONFIG_HOME/secrets/openclaw-reply-token}"
CALCIFORGE_OPENCLAW_POLICY_ENDPOINT="${CALCIFORGE_OPENCLAW_POLICY_ENDPOINT:-http://127.0.0.1:${CLASHD_PORT}/evaluate}"
CALCIFORGE_OPENCLAW_PROXY_ENDPOINT="${CALCIFORGE_OPENCLAW_PROXY_ENDPOINT:-${SECURITY_PROXY_URL}}"
CALCIFORGE_OPENCLAW_NO_PROXY="${CALCIFORGE_OPENCLAW_NO_PROXY:-${SECURITY_PROXY_NO_PROXY}}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_ENDPOINT="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_ENDPOINT:-http://127.0.0.1:18083/v1}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_PROVIDER="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_PROVIDER:-calciforge}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MODEL="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MODEL:-local-dispatcher}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_CONTEXT="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_CONTEXT:-60000}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MAX_TOKENS="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MAX_TOKENS:-8192}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY:-}"
CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY_FILE="${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY_FILE:-}"
CALCIFORGE_IRONCLAW_PORT="${CALCIFORGE_IRONCLAW_PORT:-3000}"
CALCIFORGE_IRONCLAW_ENDPOINT="${CALCIFORGE_IRONCLAW_ENDPOINT:-http://127.0.0.1:${CALCIFORGE_IRONCLAW_PORT}}"
if [[ "$(uname -s)" == "Darwin" ]] || [[ $EUID -ne 0 ]]; then
    _IRONCLAW_DEFAULT_DIR="$HOME/.local/share/ironclaw"
else
    _IRONCLAW_DEFAULT_DIR="/opt/ironclaw"
fi
CALCIFORGE_IRONCLAW_INSTALL_DIR="${CALCIFORGE_IRONCLAW_INSTALL_DIR:-$_IRONCLAW_DEFAULT_DIR}"
CALCIFORGE_IRONCLAW_LLM_BACKEND="${CALCIFORGE_IRONCLAW_LLM_BACKEND:-openai_compatible}"
CALCIFORGE_HERMES_PORT="${CALCIFORGE_HERMES_PORT:-8642}"
CALCIFORGE_HERMES_ENDPOINT="${CALCIFORGE_HERMES_ENDPOINT:-http://127.0.0.1:${CALCIFORGE_HERMES_PORT}}"
if [[ "$(uname -s)" == "Darwin" ]] || [[ $EUID -ne 0 ]]; then
    _HERMES_DEFAULT_DIR="$HOME/.local/share/hermes"
else
    _HERMES_DEFAULT_DIR="/opt/hermes"
fi
CALCIFORGE_HERMES_INSTALL_DIR="${CALCIFORGE_HERMES_INSTALL_DIR:-$_HERMES_DEFAULT_DIR}"
CALCIFORGE_HERMES_DEFAULT_MODEL="${CALCIFORGE_HERMES_DEFAULT_MODEL:-${CALCIFORGE_IRONCLAW_DEFAULT_MODEL:-kimi-k2.5}}"
CALCIFORGE_GATEWAY_PORT="${CALCIFORGE_GATEWAY_PORT:-18083}"
CALCIFORGE_GATEWAY_BACKEND_URL="${CALCIFORGE_GATEWAY_BACKEND_URL:-http://127.0.0.1:18801/v1}"
CALCIFORGE_GATEWAY_BACKEND_API_KEY_FILE="${CALCIFORGE_GATEWAY_BACKEND_API_KEY_FILE:-$CALCIFORGE_CONFIG_HOME/secrets/gateway-backend-key}"
CALCIFORGE_FNOX_PROVIDER_NAME="${CALCIFORGE_FNOX_PROVIDER_NAME:-calciforge-local}"
CALCIFORGE_FNOX_PROVIDER_TYPE="${CALCIFORGE_FNOX_PROVIDER_TYPE:-}"
CALCIFORGE_FNOX_DIR="${CALCIFORGE_FNOX_DIR:-$CALCIFORGE_CONFIG_HOME}"
FNOX_AGE_KEY_FILE="${FNOX_AGE_KEY_FILE:-${CALCIFORGE_FNOX_AGE_KEY_FILE:-}}"
CALCIFORGE_FNOX_AGE_RECIPIENT="${CALCIFORGE_FNOX_AGE_RECIPIENT:-}"
REMOTE_SCANNER_ENABLED="${CALCIFORGE_REMOTE_SCANNER_ENABLED:-${REMOTE_SCANNER_ENABLED:-0}}"
REMOTE_SCANNER_PORT="${REMOTE_SCANNER_PORT:-9801}"
REMOTE_SCANNER_URL=""
REMOTE_SCANNER_FAIL_CLOSED="${REMOTE_SCANNER_FAIL_CLOSED:-true}"
REMOTE_SCANNER_API_KEY_FILE="${REMOTE_SCANNER_API_KEY_FILE:-$CALCIFORGE_CONFIG_HOME/secrets/remote-scanner-api-key}"
REMOTE_SCANNER_API_BASE="${REMOTE_SCANNER_API_BASE:-https://api.openai.com/v1}"
REMOTE_SCANNER_MODEL="${REMOTE_SCANNER_MODEL:-gpt-5.4-mini}"
REMOTE_SCANNER_PROMPT_FILE="${REMOTE_SCANNER_PROMPT_FILE:-$CALCIFORGE_CONFIG_HOME/remote-llm-scanner-prompt.txt}"
LOG_MAX_BYTES="${CALCIFORGE_LOG_MAX_BYTES:-10485760}"
LOG_BACKUPS="${CALCIFORGE_LOG_BACKUPS:-5}"
CALCIFORGE_INSTALL_DOCTOR_NETWORK="${CALCIFORGE_INSTALL_DOCTOR_NETWORK:-true}"
CALCIFORGE_INSTALL_DOCTOR_STRIP_PROXIES="${CALCIFORGE_INSTALL_DOCTOR_STRIP_PROXIES:-false}"
ZC_CONFIG="${CALCIFORGE_CONFIG:-$CALCIFORGE_CONFIG_HOME/config.toml}"
ZC_LOG_DIR="${ZC_LOG_DIR:-$CALCIFORGE_CONFIG_HOME/logs}"
INSTALL_NODES_STATE="${CALCIFORGE_INSTALL_NODES_STATE:-$CALCIFORGE_CONFIG_HOME/install-nodes.json}"
LEGACY_SERVICE_PREFIX="${CALCIFORGE_LEGACY_SERVICE_PREFIX:-}"
CLASHD_POLICY="${CLASHD_POLICY:-$CLASH_DIR/policy.star}"
AGENTS_JSON="$CLASH_DIR/agents.json"
CLASHD_DEFAULT_POLICY="$REPO_ROOT/crates/clashd/config/default-policy.star"
CLASHD_DEFAULT_AGENTS="$REPO_ROOT/crates/clashd/config/agents.example.json"

case "$REMOTE_SCANNER_ENABLED" in
    1|true|TRUE|yes|YES|on|ON)
        REMOTE_SCANNER_URL="http://127.0.0.1:${REMOTE_SCANNER_PORT}"
        ;;
esac

# ── platform detection ────────────────────────────────────────────────────────
# Drives choice of service manager (launchd vs systemd --user) and package
# installer fallbacks (brew vs apt/dnf). Scripts that don't have both paths
# tested will warn rather than fail.
PLATFORM="$(uname -s)"
if [[ -z "$FNOX_AGE_KEY_FILE" && "$PLATFORM" != "Darwin" ]]; then
    FNOX_AGE_KEY_FILE="$CALCIFORGE_CONFIG_HOME/secrets/fnox-age-ed25519"
fi
# Running as root on Linux installs system-wide; non-root uses --user.
# On Darwin we always use per-user LaunchAgents.
IS_ROOT=false
[[ $EUID -eq 0 ]] && IS_ROOT=true
# systemd install target: system units use multi-user.target; user units
# (systemctl --user) use default.target. Keep the generators in sync so
# `systemctl enable` doesn't silently no-op on one mode.
WANTED_BY_TARGET="default.target"
$IS_ROOT && WANTED_BY_TARGET="multi-user.target"
case "$PLATFORM" in
    Darwin)
        BIN_DIR="$HOME/.local/bin"
        PLIST_DIR="$HOME/Library/LaunchAgents"
        LOG_DIR="$HOME/Library/Logs/clashd"
        SEC_LOG_DIR="$HOME/Library/Logs/security-proxy"
        SYSTEMCTL=""   # unused on Darwin
        ;;
    Linux)
        if $IS_ROOT; then
            BIN_DIR="/usr/local/bin"
            PLIST_DIR="/etc/systemd/system"
            LOG_DIR="/var/log/calciforge"
            SEC_LOG_DIR="/var/log/calciforge"
            SYSTEMCTL="systemctl"
        else
            BIN_DIR="$HOME/.local/bin"
            PLIST_DIR="$HOME/.config/systemd/user"
            LOG_DIR="$HOME/.local/state/calciforge/logs"
            SEC_LOG_DIR="$HOME/.local/state/calciforge/logs"
            SYSTEMCTL="systemctl --user"
            # systemctl --user requires XDG_RUNTIME_DIR to locate the user bus.
            # Login shells set this via pam_systemd; sudo / su / SSH-in-some-configs
            # don't. Fill it in when missing so the script can still enable units.
            if [[ -z "${XDG_RUNTIME_DIR:-}" ]] && [[ -d "/run/user/$EUID" ]]; then
                export XDG_RUNTIME_DIR="/run/user/$EUID"
            fi
            if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]] && [[ -S "${XDG_RUNTIME_DIR:-}/bus" ]]; then
                export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR}/bus"
            fi
        fi
        ;;
    *)
        echo "Unsupported platform: $PLATFORM" >&2
        exit 1
        ;;
esac

rotate_log_file() {
    local file="$1" max_bytes="${2:-$LOG_MAX_BYTES}" backups="${3:-$LOG_BACKUPS}"
    [[ -f "$file" ]] || return 0
    local size
    size="$(wc -c < "$file" 2>/dev/null || echo 0)"
    [[ "$size" =~ ^[0-9]+$ ]] || size=0
    (( size < max_bytes )) && return 0

    local i
    for ((i=backups; i>=1; i--)); do
        if [[ -f "${file}.${i}" ]]; then
            if (( i == backups )); then
                rm -f "${file}.${i}"
            else
                mv -f "${file}.${i}" "${file}.$((i + 1))"
            fi
        fi
    done
    mv -f "$file" "${file}.1"
    : > "$file"
}

truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

expand_home_path() {
    local path="$1"
    case "$path" in
        "~") printf '%s\n' "$HOME" ;;
        "~/"*) printf '%s/%s\n' "$HOME" "${path#~/}" ;;
        *) printf '%s\n' "$path" ;;
    esac
}

ensure_mitm_ca() {
    truthy "$SECURITY_PROXY_MITM_ENABLED" || return 0

    if [[ -f "$SECURITY_PROXY_CA_CERT" && -f "$SECURITY_PROXY_CA_KEY" ]]; then
        chmod 600 "$SECURITY_PROXY_CA_KEY" 2>/dev/null || true
        ok "MITM CA already present → $SECURITY_PROXY_CA_CERT"
        return 0
    fi
    if [[ -e "$SECURITY_PROXY_CA_CERT" || -e "$SECURITY_PROXY_CA_KEY" ]]; then
        die "MITM CA is incomplete: expected both $SECURITY_PROXY_CA_CERT and $SECURITY_PROXY_CA_KEY"
    fi
    command -v openssl >/dev/null 2>&1 || \
        die "openssl is required to generate the Calciforge MITM CA; set SECURITY_PROXY_MITM_ENABLED=false to skip"

    mkdir -p "$(dirname "$SECURITY_PROXY_CA_CERT")" "$(dirname "$SECURITY_PROXY_CA_KEY")"
    ( umask 077
      openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes \
        -keyout "$SECURITY_PROXY_CA_KEY" \
        -out "$SECURITY_PROXY_CA_CERT" \
        -subj "/CN=Calciforge Local MITM CA" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1 )
    chmod 600 "$SECURITY_PROXY_CA_KEY"
    chmod 644 "$SECURITY_PROXY_CA_CERT"
    ok "Generated MITM CA → $SECURITY_PROXY_CA_CERT"
}

trust_mitm_ca_if_supported() {
    truthy "$SECURITY_PROXY_MITM_ENABLED" || return 0
    truthy "$SECURITY_PROXY_TRUST_MITM_CA" || {
        warn "Skipping macOS MITM CA trust (SECURITY_PROXY_TRUST_MITM_CA=false)"
        return 0
    }
    [[ "$PLATFORM" == "Darwin" ]] || return 0
    command -v security >/dev/null 2>&1 || {
        warn "macOS security CLI not found; trust $SECURITY_PROXY_CA_CERT manually for browser MITM"
        return 0
    }
    if security verify-cert -c "$SECURITY_PROXY_CA_CERT" -p ssl >/dev/null 2>&1; then
        ok "Calciforge MITM CA is already trusted in the macOS login keychain"
        return 0
    fi

    echo ""
    echo "Calciforge HTTPS inspection uses a local MITM CA for tested proxied clients."
    echo "macOS may ask for your password before adding this CA to the login keychain."
    echo "This lets browsers, tools, and agent runtimes trust Calciforge's local proxy"
    echo "when they are configured to send HTTPS traffic through security-proxy."
    echo "Skip with SECURITY_PROXY_TRUST_MITM_CA=false; without trust, proxied HTTPS"
    echo "clients may show certificate errors instead of inspected pages."
    echo ""
    if [[ "$YES" != true ]]; then
        if [[ ! -t 0 ]]; then
            warn "Skipping macOS MITM CA trust prompt because stdin is not interactive"
            warn "Rerun with --yes or run: security add-trusted-cert -r trustRoot -p ssl -p basic -k \"\$HOME/Library/Keychains/login.keychain-db\" \"$SECURITY_PROXY_CA_CERT\""
            return 0
        fi
        read -r -p "  Trust the Calciforge MITM CA in the macOS login keychain now? [Y/n] " ans
        if [[ ! "${ans:-Y}" =~ ^[Yy] ]]; then
            warn "Skipping macOS MITM CA trust at operator request"
            return 0
        fi
    fi

    if security add-trusted-cert -r trustRoot -p ssl -p basic \
        -k "$HOME/Library/Keychains/login.keychain-db" \
        "$SECURITY_PROXY_CA_CERT"; then
        ok "Trusted Calciforge MITM CA in the macOS login keychain"
    else
        warn "Could not trust Calciforge MITM CA automatically; browser MITM may show certificate errors"
        warn "Manual command: security add-trusted-cert -r trustRoot -p ssl -p basic -k \"\$HOME/Library/Keychains/login.keychain-db\" \"$SECURITY_PROXY_CA_CERT\""
    fi
}

write_log_rotator() {
    mkdir -p "$BIN_DIR"
    cat > "$BIN_DIR/calciforge-rotate-logs" <<'ROTATE'
#!/usr/bin/env bash
set -euo pipefail

max_bytes="${CALCIFORGE_LOG_MAX_BYTES:-10485760}"
backups="${CALCIFORGE_LOG_BACKUPS:-5}"

rotate_one() {
    local file="$1"
    [[ -f "$file" ]] || return 0

    local size
    size="$(wc -c < "$file" 2>/dev/null || echo 0)"
    [[ "$size" =~ ^[0-9]+$ ]] || size=0
    (( size < max_bytes )) && return 0

    local i
    for ((i=backups; i>=1; i--)); do
        if [[ -f "${file}.${i}" ]]; then
            if (( i == backups )); then
                rm -f "${file}.${i}"
            else
                mv -f "${file}.${i}" "${file}.$((i + 1))"
            fi
        fi
    done
    mv -f "$file" "${file}.1"
    : > "$file"
}

for dir in "$@"; do
    [[ -d "$dir" ]] || continue
    while IFS= read -r file; do
        rotate_one "$file"
    done < <(find "$dir" -maxdepth 1 -type f \( -name '*.log' -o -name '*.err' \) -print)
done
ROTATE
    chmod 755 "$BIN_DIR/calciforge-rotate-logs"
}

install_log_rotation() {
    write_log_rotator
    mkdir -p "$CALCIFORGE_CONFIG_HOME" "$CALCIFORGE_FNOX_DIR" "$LOG_DIR" "$SEC_LOG_DIR" "$ZC_LOG_DIR"

    if [[ "$PLATFORM" == "Darwin" ]]; then
        local rotate_plist="$PLIST_DIR/com.calciforge.log-rotate.plist"
        mkdir -p "$PLIST_DIR"
        cat > "$rotate_plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.calciforge.log-rotate</string>
    <key>ProgramArguments</key><array>
        <string>${BIN_DIR}/calciforge-rotate-logs</string>
        <string>${LOG_DIR}</string>
        <string>${SEC_LOG_DIR}</string>
        <string>${ZC_LOG_DIR}</string>
        <string>${HOME}/Library/Logs/calciforge</string>
    </array>
    <key>StartInterval</key><integer>300</integer>
    <key>RunAtLoad</key><true/>
</dict></plist>
EOF
        load_launch_agent "com.calciforge.log-rotate" "$rotate_plist" || \
            warn "log rotation LaunchAgent did not load"
        ok "Log rotation installed (launchd, ${LOG_MAX_BYTES} bytes, ${LOG_BACKUPS} backups)"
    elif $IS_ROOT; then
        local patterns="${LOG_DIR}/*.log ${LOG_DIR}/*.err"
        if [[ "$SEC_LOG_DIR" != "$LOG_DIR" ]]; then
            patterns="${patterns} ${SEC_LOG_DIR}/*.log ${SEC_LOG_DIR}/*.err"
        fi
        if [[ "$ZC_LOG_DIR" != "$LOG_DIR" && "$ZC_LOG_DIR" != "$SEC_LOG_DIR" ]]; then
            patterns="${patterns} ${ZC_LOG_DIR}/*.log ${ZC_LOG_DIR}/*.err"
        fi
        cat > /etc/logrotate.d/calciforge <<EOF
${patterns} {
    size ${LOG_MAX_BYTES}
    rotate ${LOG_BACKUPS}
    missingok
    notifempty
    copytruncate
    compress
}
EOF
        ok "Log rotation installed (/etc/logrotate.d/calciforge)"
    else
        local rotate_service="$PLIST_DIR/calciforge-log-rotate.service"
        local rotate_timer="$PLIST_DIR/calciforge-log-rotate.timer"
        mkdir -p "$PLIST_DIR"
        cat > "$rotate_service" <<EOF
[Unit]
Description=Rotate Calciforge logs

[Service]
Type=oneshot
ExecStart=${BIN_DIR}/calciforge-rotate-logs ${LOG_DIR} ${SEC_LOG_DIR} ${ZC_LOG_DIR}
EOF
        cat > "$rotate_timer" <<EOF
[Unit]
Description=Rotate Calciforge logs periodically

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
Unit=calciforge-log-rotate.service

[Install]
WantedBy=timers.target
EOF
        $SYSTEMCTL daemon-reload
        $SYSTEMCTL enable --now calciforge-log-rotate.timer 2>&1 | tail -3 || \
            warn "log rotation timer did not start — if running as non-root, run: loginctl enable-linger \$USER"
        ok "Log rotation installed (systemd user timer, ${LOG_MAX_BYTES} bytes, ${LOG_BACKUPS} backups)"
    fi
}

YES=false
CONFIGURE_ONLY=false
NODES_ONLY=false
AGENTS_ONLY=false
NODES_FILE=""
AGENTS="claude,opencode,openclaw,zeroclaw,ironclaw,hermes,dirac"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --yes)             YES=true ;;
        --configure-only)  CONFIGURE_ONLY=true ;;
        --agents-only)     AGENTS_ONLY=true ;;
        --nodes-file)      NODES_FILE="$2"; shift ;;
        --nodes-only)      NODES_ONLY=true ;;
        --agents)          AGENTS="$2"; shift ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
    shift
done

export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$HOME/.cargo/bin:$BIN_DIR:$PATH"
SERVICE_PATH="$BIN_DIR:$HOME/.cargo/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin"

# ── colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
die()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }
hdr()  { echo -e "\n${CYAN}━━ $* ━━${NC}"; }

agent_enabled() { [[ ",$AGENTS," == *",$1,"* ]]; }

# Source shared agent runtime helpers (binary install, service management, config registration)
# shellcheck source=lib/agent-runtime.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/agent-runtime.sh"

validate_security_proxy_bind() {
    local value="$1" label="$2"
    [[ -n "$value" ]] || die "$label must not be empty"
    [[ "$value" =~ ^[A-Za-z0-9_.:-]+$ ]] || \
        die "$label contains unsupported characters; use a host or IP such as 127.0.0.1 or 0.0.0.0"
}

validate_security_proxy_bind "$SECURITY_PROXY_BIND" "SECURITY_PROXY_BIND"

if [[ "$NODES_ONLY" != "true" ]] && [[ -e /etc/pve/.version || -d /etc/pve/nodes ]]; then
    die "refusing to install Calciforge/OpenClaw directly on a Proxmox host node; target a VM/LXC guest instead"
fi

run_build() {
    local label="$1"
    shift
    local log
    log="$(mktemp)"
    echo "  Building ${label}..." >&2
    set +e
    "$@" >"$log" 2>&1
    local rc=$?
    set -e
    grep -E "^error|error:|Compiling (clashd|security.proxy|calciforge.mcp|paste.server|calciforge)|Finished" "$log" >&2 || true
    if [[ $rc -ne 0 ]]; then
        tail -160 "$log" >&2
        rm -f "$log"
        die "Build failed for ${label}"
    fi
    rm -f "$log"
}

disable_legacy_local_service() {
    local current="$1" legacy="$2"
    [[ "$current" != "$legacy" ]] || return 0
    if [[ "$PLATFORM" == "Darwin" ]]; then
        local legacy_plist="$PLIST_DIR/com.calciforge.${legacy}.plist"
        launchctl bootout "gui/$(id -u)" "$legacy_plist" 2>/dev/null || \
            launchctl unload "$legacy_plist" 2>/dev/null || true
        [[ -f "$legacy_plist" ]] && warn "Legacy LaunchAgent remains at $legacy_plist; remove it after verifying $current"
    elif [[ -n "${SYSTEMCTL:-}" ]]; then
        $SYSTEMCTL disable --now "${legacy}.service" >/dev/null 2>&1 || true
    fi
    return 0
}

install_clashd_policy_file() {
    local policy_path="$1"
    local default_policy="$2"

    if [[ ! -f "$policy_path" ]]; then
        cp "$default_policy" "$policy_path"
        ok "Policy installed → $policy_path"
        return 0
    fi

    if grep -q "clashd policy for Claude Code tool calls" "$policy_path" 2>/dev/null; then
        local backup="${policy_path}.claude-template.bak.$(date -u +%Y%m%dT%H%M%SZ)"
        cp "$policy_path" "$backup"
        cp "$default_policy" "$policy_path"
        warn "Replaced Claude-specific policy at $policy_path with shared clashd policy (backup: $backup)"
        return 0
    fi

    ok "Policy already present → $policy_path"
}

install_clashd_agents_file() {
    local agents_path="$1"
    local default_agents="$2"
    local agents_compact=""

    if [[ -f "$agents_path" ]]; then
        agents_compact="$(tr -d '[:space:]' < "$agents_path")"
    fi

    if [[ ! -s "$agents_path" || "$agents_compact" == '{"agents":[]}' ]]; then
        if [[ -f "$agents_path" ]]; then
            local backup="${agents_path}.bak.$(date -u +%Y%m%dT%H%M%SZ)"
            cp "$agents_path" "$backup"
            warn "Replacing empty clashd agent config at $agents_path (backup: $backup)"
        fi
        cp "$default_agents" "$agents_path" 2>/dev/null || echo '{"agents":[]}' > "$agents_path"
        ok "Agent config → $agents_path"
        return 0
    fi

    ok "Agent config already present → $agents_path"
}

load_launch_agent() {
    local label="$1" plist="$2"
    local domain="gui/$(id -u)"
    launchctl bootout "$domain" "$plist" >/dev/null 2>&1 || \
        launchctl unload "$plist" >/dev/null 2>&1 || true
    if ! launchctl bootstrap "$domain" "$plist" >/dev/null 2>&1; then
        if launchctl print "${domain}/${label}" >/dev/null 2>&1; then
            launchctl kickstart -k "${domain}/${label}" >/dev/null 2>&1 || true
        else
            launchctl load "$plist"
        fi
    fi
}

enable_restart_service() {
    local service="$1"
    $SYSTEMCTL enable "$service" 2>&1 | tail -3 || {
        warn "systemctl enable failed for $service"
        return 1
    }
    # `enable --now` does not restart an already-running service after
    # replacing its binary. Restart explicitly so upgrades run new code.
    $SYSTEMCTL restart "$service" 2>&1 | tail -3 || {
        warn "systemctl restart failed for $service"
        return 1
    }
}

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

fnox_release_asset() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "${os}:${arch}" in
        Linux:x86_64|Linux:amd64) echo "fnox-x86_64-unknown-linux-gnu.tar.gz" ;;
        Linux:aarch64|Linux:arm64) echo "fnox-aarch64-unknown-linux-gnu.tar.gz" ;;
        Darwin:x86_64) echo "fnox-x86_64-apple-darwin.tar.gz" ;;
        Darwin:arm64|Darwin:aarch64) echo "fnox-aarch64-apple-darwin.tar.gz" ;;
        *) return 1 ;;
    esac
}

install_fnox_release() {
    local version="${FNOX_VERSION:-v1.23.0}"
    local asset install_dir url tmp

    asset="$(fnox_release_asset)" || return 1
    url="https://github.com/jdx/fnox/releases/download/${version}/${asset}"

    if [[ -w /usr/local/bin || "$IS_ROOT" == true ]]; then
        install_dir="/usr/local/bin"
    else
        install_dir="$HOME/.local/bin"
        mkdir -p "$install_dir"
        export PATH="$install_dir:$PATH"
    fi

    tmp="$(mktemp -d)"
    echo "  Installing fnox ${version} release..."
    if ! curl -fsSL "$url" -o "$tmp/fnox.tar.gz" ||
        ! tar -xzf "$tmp/fnox.tar.gz" -C "$tmp" ||
        ! install -m 0755 "$tmp/fnox" "$install_dir/fnox"; then
        rm -rf "$tmp"
        return 1
    fi
    rm -rf "$tmp"
}

ensure_fnox_cargo_deps() {
    [[ "$PLATFORM" == "Linux" ]] || return 0
    command -v pkg-config &>/dev/null && pkg-config --exists libudev && return 0

    if $IS_ROOT && command -v apt-get &>/dev/null; then
        echo "  Installing fnox build prerequisites..."
        if ! apt-get update -qq; then
            warn "Failed to update apt package lists for fnox prerequisites"
            return 1
        fi
        if ! DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libudev-dev >/dev/null; then
            warn "Failed to install pkg-config/libudev-dev for fnox cargo fallback"
            return 1
        fi
        return 0
    fi

    warn "fnox cargo fallback needs pkg-config and libudev-dev on Linux"
    return 1
}

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

# fnox — secret resolver (brew on macOS, release tarball on Linux, cargo last).
# Uses a dedicated helper because fnox isn't on npm. Prefer prebuilt release
# tarballs on Linux because compiling fnox can overwhelm small deployment VMs.
ensure_fnox() {
    if command -v fnox &>/dev/null; then
        ok "fnox $(fnox --version 2>/dev/null | head -1 || echo '(installed)')"
        ensure_fnox_config
        return $?
    fi
    if [[ "$CONFIGURE_ONLY" == true ]]; then
        die "fnox not found — run without --configure-only to install"
    fi
    if [[ "$PLATFORM" == "Darwin" ]] && command -v brew &>/dev/null; then
        if ask_install fnox "via brew install fnox"; then
            echo "  Installing fnox..."
            # Use PIPESTATUS to catch brew's real exit code — `| tail -3`
            # would otherwise bury a failure behind a successful `tail`.
            set +e
            brew install fnox 2>&1 | tail -3
            local brew_rc=${PIPESTATUS[0]}
            set -e
            if [[ $brew_rc -eq 0 ]]; then
                ok "fnox installed"
                ensure_fnox_config
                return $?
            fi
            warn "brew install fnox failed (exit $brew_rc); falling back to cargo path"
        fi
    fi

    if [[ "$PLATFORM" == "Linux" ]] && command -v curl &>/dev/null && command -v tar &>/dev/null; then
        if ask_install fnox "from upstream release tarball"; then
            if install_fnox_release; then
                ok "fnox installed"
                ensure_fnox_config
                return $?
            fi
            warn "fnox release install failed; falling back to cargo path"
        fi
    fi

    local cargo_bin="$HOME/.cargo/bin/cargo"
    if [[ -x "$cargo_bin" ]] && ask_install fnox "via cargo install fnox (compiles from source, ~1–2 min)"; then
        if ! ensure_fnox_cargo_deps; then
            warn "Skipping cargo fnox fallback because prerequisites are unavailable"
            return 1
        fi
        echo "  Installing fnox via cargo..."
        # Same pattern as above — the grep|tail pipeline masks
        # `cargo install`'s exit code otherwise.
        set +e
        "$cargo_bin" install fnox 2>&1 | grep -E "Installing|Installed|error" | tail -3
        local cargo_rc=${PIPESTATUS[0]}
        set -e
        if [[ $cargo_rc -eq 0 ]]; then
            ok "fnox installed"
            ensure_fnox_config
            return $?
        fi
        warn "cargo install fnox failed (exit $cargo_rc) — see output above"
    fi
    warn "fnox not installed — secret lookup will skip the fnox layer (env → vaultwarden still works)"
    return 1
}

ensure_fnox_config() {
    mkdir -p "$CALCIFORGE_FNOX_DIR"
    local err_file
    err_file="$(mktemp)"
    if (cd "$CALCIFORGE_FNOX_DIR" && fnox list >/dev/null 2>"$err_file"); then
        rm -f "$err_file"
        ok "fnox config usable"
        ensure_fnox_provider
        return 0
    fi

    if grep -Eqi "No configuration file found|No providers configured" "$err_file"; then
        echo "  Initializing fnox global config..."
        if fnox init --global --skip-wizard >/dev/null 2>"$err_file"; then
            if ensure_fnox_provider; then
                rm -f "$err_file"
                ok "fnox global config initialized"
                return 0
            fi
        fi
    fi

    warn "fnox is installed but not usable from this environment"
    sed 's/^/  fnox: /' "$err_file" | tail -5
    rm -f "$err_file"
    return 1
}

fnox_provider_count() {
    fnox provider list 2>/dev/null | awk 'NF { count++ } END { print count + 0 }'
}

default_fnox_provider_type() {
    if [[ -n "$CALCIFORGE_FNOX_PROVIDER_TYPE" ]]; then
        echo "$CALCIFORGE_FNOX_PROVIDER_TYPE"
    elif [[ "$PLATFORM" == "Darwin" ]]; then
        echo "keychain"
    else
        echo "age"
    fi
}

fnox_global_config_file() {
    echo "${FNOX_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/fnox}/config.toml"
}

toml_basic_string() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//$'\n'/\\n}"
    value="${value//$'\r'/\\r}"
    value="${value//$'\t'/\\t}"
    printf '"%s"\n' "$value"
}

ensure_fnox_age_key() {
    local key_file recipient
    if [[ -n "$CALCIFORGE_FNOX_AGE_RECIPIENT" ]]; then
        echo "$CALCIFORGE_FNOX_AGE_RECIPIENT"
        return 0
    fi

    key_file="${FNOX_AGE_KEY_FILE:-$CALCIFORGE_CONFIG_HOME/secrets/fnox-age-ed25519}"
    mkdir -p "$(dirname "$key_file")"
    if [[ ! -f "$key_file" ]]; then
        if ! command -v ssh-keygen >/dev/null 2>&1; then
            warn "fnox age provider needs ssh-keygen to create ${key_file}; set CALCIFORGE_FNOX_AGE_RECIPIENT and FNOX_AGE_KEY_FILE to use your own key"
            return 1
        fi
        echo "  Generating fnox age key ${key_file}..." >&2
        ssh-keygen -q -t ed25519 -N "" -C "calciforge-fnox@$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo host)" -f "$key_file"
    fi
    chmod 600 "$key_file" 2>/dev/null || true
    chmod 644 "${key_file}.pub" 2>/dev/null || true
    FNOX_AGE_KEY_FILE="$key_file"

    if [[ ! -f "${key_file}.pub" ]]; then
        warn "fnox age public key ${key_file}.pub is missing"
        return 1
    fi
    recipient="$(cat "${key_file}.pub")"
    if [[ -z "$recipient" ]]; then
        warn "fnox age public key ${key_file}.pub is empty"
        return 1
    fi
    echo "$recipient"
}

ensure_fnox_age_provider() {
    local recipient config_file escaped_name escaped_recipient
    if [[ -z "$CALCIFORGE_FNOX_AGE_RECIPIENT" && -z "$FNOX_AGE_KEY_FILE" ]]; then
        FNOX_AGE_KEY_FILE="$CALCIFORGE_CONFIG_HOME/secrets/fnox-age-ed25519"
    fi
    recipient="$(ensure_fnox_age_key)" || return 1
    config_file="$(fnox_global_config_file)"
    mkdir -p "$(dirname "$config_file")"
    touch "$config_file"
    escaped_name="$(toml_basic_string "$CALCIFORGE_FNOX_PROVIDER_NAME")"
    escaped_recipient="$(toml_basic_string "$recipient")"
    {
        echo ""
        echo "[providers.${escaped_name}]"
        echo "type = \"age\""
        echo "recipients = [${escaped_recipient}]"
    } >> "$config_file"

    if FNOX_AGE_KEY_FILE="$FNOX_AGE_KEY_FILE" fnox provider test "$CALCIFORGE_FNOX_PROVIDER_NAME" >/dev/null 2>&1; then
        ok "fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' ready"
        return 0
    fi

    warn "fnox age provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' was written but did not pass its connection test"
    return 1
}

ensure_fnox_provider() {
    local count provider_type err_file
    count="$(fnox_provider_count)"
    if [[ "$count" -gt 0 ]]; then
        ok "fnox provider configured"
        return 0
    fi

    provider_type="$(default_fnox_provider_type)"
    if [[ -z "$provider_type" ]]; then
        warn "fnox has no provider configured; run 'fnox provider add <name> <type> --global' or set CALCIFORGE_FNOX_PROVIDER_TYPE before install"
        return 1
    fi

    if [[ "$provider_type" == "age" ]]; then
        ensure_fnox_age_provider
        return $?
    fi

    err_file="$(mktemp)"
    echo "  Adding fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' (${provider_type})..."
    if fnox provider add "$CALCIFORGE_FNOX_PROVIDER_NAME" "$provider_type" --global >/dev/null 2>"$err_file"; then
        if fnox provider test "$CALCIFORGE_FNOX_PROVIDER_NAME" >/dev/null 2>"$err_file"; then
            rm -f "$err_file"
            ok "fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' ready"
            return 0
        fi
        warn "fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' was added but did not pass its connection test"
    else
        warn "failed to add fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}'"
    fi
    sed 's/^/  fnox: /' "$err_file" | tail -5
    rm -f "$err_file"
    return 1
}

# Cross-platform package install: prefers brew on macOS, falls back to npm.
# On Linux, goes straight to npm (which works on any node-enabled distro).
# Args: <bin> [brew_pkg] [npm_pkg] — brew_pkg defaults to bin, npm_pkg to bin.
ensure_tool() {
    local bin="$1"
    local brew_pkg="${2:-$1}"
    local npm_pkg="${3:-$1}"
    if command -v "$bin" &>/dev/null; then
        ok "$bin $("$bin" --version 2>/dev/null | head -1 || echo '(installed)')"
        return 0
    fi
    case "$PLATFORM" in
        Darwin)
            if command -v brew &>/dev/null; then
                ensure_brew "$brew_pkg" "$bin"
                return $?
            fi
            ensure_npm "$npm_pkg" "$bin"
            ;;
        Linux)
            # Prefer npm (most universal across distros).
            if command -v npm &>/dev/null; then
                ensure_npm "$npm_pkg" "$bin"
                return $?
            fi
            # --yes: try the distro package manager to install node+npm, then
            # retry via ensure_npm. Silent if no supported package manager.
            if [[ "$YES" == true ]]; then
                local sudo_cmd=""
                $IS_ROOT || sudo_cmd="sudo"
                if command -v apt-get &>/dev/null; then
                    $sudo_cmd apt-get update -qq && \
                        $sudo_cmd apt-get install -y -qq nodejs npm
                elif command -v dnf &>/dev/null; then
                    $sudo_cmd dnf install -y -q nodejs npm
                fi
                if command -v npm &>/dev/null; then
                    ensure_npm "$npm_pkg" "$bin"
                    return $?
                fi
            fi
            warn "$bin not found and npm unavailable — install node+npm first, or rerun with --yes on apt/dnf systems"
            return 1
            ;;
    esac
}

ensure_secret_token_file() {
    local path="$1" label="$2"
    path="$(expand_home_path "$path")"
    mkdir -p "$(dirname "$path")"
    if [[ -s "$path" ]]; then
        chmod 600 "$path" 2>/dev/null || true
        ok "$label token file already present → $path"
        return 0
    fi
    command -v openssl >/dev/null 2>&1 || die "openssl is required to generate $label token"
    ( umask 077; openssl rand -hex 32 > "$path" )
    chmod 600 "$path"
    ok "Generated $label token → $path"
}

managed_openclaw_claw_spec() {
    local no_proxy_spec="${CALCIFORGE_OPENCLAW_NO_PROXY//,/;}"
    local spec="name=${CALCIFORGE_OPENCLAW_NAME},adapter=openclaw-channel,host=local,endpoint=${CALCIFORGE_OPENCLAW_ENDPOINT},auth_token_file=${CALCIFORGE_OPENCLAW_AUTH_TOKEN_FILE},reply_webhook=${CALCIFORGE_OPENCLAW_REPLY_WEBHOOK},reply_auth_token_file=${CALCIFORGE_OPENCLAW_REPLY_TOKEN_FILE}"
    if [[ -n "$CALCIFORGE_OPENCLAW_POLICY_ENDPOINT" ]]; then
        spec="${spec},policy_endpoint=${CALCIFORGE_OPENCLAW_POLICY_ENDPOINT}"
    fi
    if [[ -n "$CALCIFORGE_OPENCLAW_PROXY_ENDPOINT" ]]; then
        spec="${spec},proxy_endpoint=${CALCIFORGE_OPENCLAW_PROXY_ENDPOINT}"
    fi
    if [[ -n "$no_proxy_spec" ]]; then
        spec="${spec},no_proxy=${no_proxy_spec}"
    fi
    printf '%s' "$spec"
}

ensure_managed_openclaw_calciforge_agent() {
    mkdir -p "$(dirname "$ZC_CONFIG")"
    python3 - "$ZC_CONFIG" \
        "$CALCIFORGE_OPENCLAW_NAME" \
        "$CALCIFORGE_OPENCLAW_ENDPOINT" \
        "$CALCIFORGE_OPENCLAW_AUTH_TOKEN_FILE" \
        "$CALCIFORGE_OPENCLAW_REPLY_TOKEN_FILE" \
        "$CALCIFORGE_OPENCLAW_AGENT_ID" <<'PYEOF'
import json
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1]).expanduser()
agent_id, endpoint, api_key_file, reply_token_file, openclaw_agent_id = sys.argv[2:7]

if not path.exists() or not path.read_text().strip():
    path.write_text("[calciforge]\nversion = 2\n")

text = path.read_text()
agent_blocks = re.split(r"(?m)^\[\[agents\]\]\s*$", text)[1:]
for block in agent_blocks:
    next_table = re.split(r"(?m)^\[", block, maxsplit=1)[0]
    match = re.search(r"(?m)^\s*id\s*=\s*[\"']([^\"']+)[\"']", next_table)
    if match and match.group(1) == agent_id:
        print(f"calciforge agent {agent_id!r} already present in {path}")
        raise SystemExit(0)

def q(value: str) -> str:
    return json.dumps(value)

block = f"""

# Managed by calciforge install for the local OpenClaw gateway.
[[agents]]
id = {q(agent_id)}
kind = "openclaw-channel"
endpoint = {q(endpoint)}
api_key_file = {q(api_key_file)}
reply_auth_token_file = {q(reply_token_file)}
openclaw_agent_id = {q(openclaw_agent_id)}
timeout_ms = 600000
aliases = ["openclaw"]
registry = {{ display_name = "OpenClaw Local", specialties = ["local", "managed-openclaw"] }}
"""

with path.open("a", encoding="utf-8") as fh:
    fh.write(block)
print(f"added calciforge agent {agent_id!r} to {path}")
PYEOF
}

configure_openclaw_model_gateway() {
    local patch_json patch_stderr
    patch_stderr="$(mktemp)"
    if ! patch_json="$(python3 - "$ZC_CONFIG" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_ENDPOINT" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_PROVIDER" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MODEL" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_CONTEXT" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MAX_TOKENS" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY" \
        "$CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY_FILE" 2>"$patch_stderr" <<'PYEOF'
import json
import pathlib
import sys
try:
    import tomllib
except ModuleNotFoundError:
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        tomllib = None

config_path = pathlib.Path(sys.argv[1]).expanduser()
endpoint, provider, model, context, max_tokens, inline_key, key_file = sys.argv[2:10]
provider = provider.strip() or "calciforge"
model = model.strip() or "local-dispatcher"
model_id = model.split("/", 1)[1] if model.startswith(f"{provider}/") else model
context_tokens = int(context)
max_output_tokens = int(max_tokens)

def read_key_file(path):
    if not path:
        return None
    file_path = pathlib.Path(path).expanduser()
    if not file_path.exists():
        return None
    value = file_path.read_text(encoding="utf-8").strip()
    return value or None

def unquote_toml_string(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {'"', "'"}:
        return value[1:-1]
    return value

def strip_toml_comment(value):
    quote = None
    escaped = False
    out = []
    for ch in value:
        if quote:
            out.append(ch)
            if quote == '"' and ch == "\\" and not escaped:
                escaped = True
                continue
            if ch == quote and not escaped:
                quote = None
            escaped = False
            continue
        if ch in {'"', "'"}:
            quote = ch
            out.append(ch)
            continue
        if ch == "#":
            break
        out.append(ch)
    return "".join(out).strip()

def normalize_table_header(line):
    stripped = strip_toml_comment(line)
    if not (stripped.startswith("[") and stripped.endswith("]")):
        return None
    return stripped[1:-1].strip()

def read_proxy_key_without_toml(path):
    if not path.exists():
        return None
    in_proxy = False
    proxy = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        table = normalize_table_header(stripped)
        if table is not None:
            in_proxy = table == "proxy"
            continue
        if not in_proxy or "=" not in stripped:
            continue
        key, value = stripped.split("=", 1)
        key = key.strip()
        if key in {"api_key", "api_key_file"}:
            proxy[key] = unquote_toml_string(strip_toml_comment(value))
    return (proxy.get("api_key") or "").strip() or read_key_file(proxy.get("api_key_file"))

api_key = inline_key.strip() or read_key_file(key_file)
if api_key is None and tomllib is not None and config_path.exists():
    config = tomllib.loads(config_path.read_text(encoding="utf-8"))
    proxy = config.get("proxy", {})
    api_key = (proxy.get("api_key") or "").strip() or read_key_file(proxy.get("api_key_file"))
elif api_key is None:
    api_key = read_proxy_key_without_toml(config_path)

if api_key is None:
    raise SystemExit(
        "OpenClaw model gateway wiring requires [proxy].api_key or [proxy].api_key_file "
        "in Calciforge config, or CALCIFORGE_OPENCLAW_MODEL_GATEWAY_API_KEY(_FILE)."
    )

print(json.dumps({
    "agents": {
        "defaults": {
            "agentRuntime": {"id": "pi", "fallback": "pi"},
            "model": {"primary": f"{provider}/{model_id}"},
            "models": {f"{provider}/{model_id}": {}},
        }
    },
    "models": {
        "mode": "merge",
        "providers": {
            provider: {
                "baseUrl": endpoint,
                "apiKey": api_key,
                "api": "openai-completions",
                "contextWindow": context_tokens,
                "maxTokens": max_output_tokens,
                "request": {"allowPrivateNetwork": True},
                "models": [{
                    "id": model_id,
                    "name": f"Calciforge {model_id}",
                    "api": "openai-completions",
                    "contextWindow": context_tokens,
                    "maxTokens": max_output_tokens,
                    "input": ["text"],
                    "cost": {"input": 0, "output": 0},
                }],
            }
        },
    },
}))
PYEOF
    )"; then
        warn "$(cat "$patch_stderr")"
        rm -f "$patch_stderr"
        return 1
    fi
    if [[ -s "$patch_stderr" ]]; then
        warn "$(cat "$patch_stderr")"
    fi
    rm -f "$patch_stderr"

    printf '%s\n' "$patch_json" | openclaw config patch --stdin >/dev/null
    ok "openclaw default model routed through Calciforge model gateway (${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_PROVIDER}/${CALCIFORGE_OPENCLAW_MODEL_GATEWAY_MODEL})"
    openclaw gateway restart --json >/dev/null 2>&1 || \
        warn "openclaw gateway restart failed after model gateway patch; restart it manually before testing"
}

configure_managed_openclaw() {
    truthy "$CALCIFORGE_MANAGED_OPENCLAW" || {
        warn "Skipping managed OpenClaw configuration (CALCIFORGE_MANAGED_OPENCLAW=false)"
        return 0
    }
    if [[ ! -x "$BIN_DIR/calciforge" ]]; then
        warn "Skipping managed OpenClaw configuration — calciforge binary not found at $BIN_DIR/calciforge"
        return 0
    fi

    ensure_secret_token_file "$CALCIFORGE_OPENCLAW_AUTH_TOKEN_FILE" "OpenClaw inbound"
    ensure_secret_token_file "$CALCIFORGE_OPENCLAW_REPLY_TOKEN_FILE" "OpenClaw reply"

    local claw_spec
    claw_spec="$(managed_openclaw_claw_spec)"
    "$BIN_DIR/calciforge" --config "$ZC_CONFIG" install \
        --calciforge-host local \
        --claw "$claw_spec" \
        --yes || die "managed OpenClaw configuration failed; inspect installer output above"
    ensure_managed_openclaw_calciforge_agent
    configure_openclaw_model_gateway || warn "managed OpenClaw model gateway configuration skipped (API key not configured)"
}

# ── banner ────────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Calciforge — Unified Installer"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Agents:  $AGENTS"
echo "  Mode:    $(if [ "$AGENTS_ONLY" = true ]; then echo agents-only; elif [ "$CONFIGURE_ONLY" = true ]; then echo configure-only; else echo install+configure; fi)"
echo "  Yes:     $YES"
echo ""

if [[ "$NODES_ONLY" != true ]]; then

if [[ "$AGENTS_ONLY" != true ]]; then
install_log_rotation

# ══════════════════════════════════════════════════════════════════════════════
# 1. Build + install calciforge, clashd, security-proxy
# ══════════════════════════════════════════════════════════════════════════════
if [[ "$CONFIGURE_ONLY" != true ]]; then
    hdr "Building Calciforge binaries"
    CARGO="$HOME/.cargo/bin/cargo"
    [[ -x "$CARGO" ]] || die "cargo not found — install Rust from https://rustup.rs"

    # channel-matrix is optional in Cargo.toml but on for real deployments; enable by default.
    # Build each crate separately so --features only applies to calciforge.
    run_build "support binaries" \
        "$CARGO" build --release -p clashd -p security-proxy -p mcp-server -p paste-server \
            -p secrets-client --bin calciforge-secrets
    run_build "calciforge with Matrix channel support" \
        "$CARGO" build --release -p calciforge --features channel-matrix

    mkdir -p "$BIN_DIR"
    for bin in clashd calciforge security-proxy mcp-server paste-server calciforge-secrets; do
        src="$REPO_ROOT/target/release/$bin"
        [[ -f "$src" ]] || { warn "Binary not found: $src (build may have failed)"; continue; }
        # On Linux, overwriting a running binary fails with "Text file busy".
        # `install` (coreutils) handles this by unlinking first — safe to call even when binary is running,
        # since the live process keeps its mapping via the original inode until it exits.
        install -m 755 "$src" "$BIN_DIR/$bin" 2>/dev/null || {
            # install(1) not present (rare) — fall back to unlink+cp
            rm -f "$BIN_DIR/$bin" 2>/dev/null
            cp "$src" "$BIN_DIR/$bin"
            chmod +x "$BIN_DIR/$bin"
        }
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
disable_legacy_local_service "calciforge-clashd" "clashd"
disable_legacy_local_service "calciforge-clashd" "${LEGACY_SERVICE_PREFIX}-clashd"

install_clashd_policy_file "$CLASHD_POLICY" "$CLASHD_DEFAULT_POLICY"
install_clashd_agents_file "$AGENTS_JSON" "$CLASHD_DEFAULT_AGENTS"

if [[ "$PLATFORM" == "Darwin" ]]; then
    CLASHD_PLIST="$PLIST_DIR/com.calciforge.clashd.plist"
    cat > "$CLASHD_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.calciforge.clashd</string>
    <key>ProgramArguments</key><array><string>${BIN_DIR}/clashd</string></array>
    <key>EnvironmentVariables</key><dict>
        <key>CLASHD_PORT</key><string>${CLASHD_PORT}</string>
        <key>CLASHD_POLICY</key><string>${CLASHD_POLICY}</string>
        <key>CLASHD_AGENTS</key><string>${AGENTS_JSON}</string>
        <key>PATH</key><string>${SERVICE_PATH}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${LOG_DIR}/clashd.log</string>
    <key>StandardErrorPath</key><string>${LOG_DIR}/clashd.err</string>
</dict></plist>
EOF
    load_launch_agent "com.calciforge.clashd" "$CLASHD_PLIST"
else
    CLASHD_UNIT="$PLIST_DIR/calciforge-clashd.service"
    cat > "$CLASHD_UNIT" <<EOF
[Unit]
Description=Calciforge clashd policy engine
After=network.target

[Service]
Type=simple
ExecStart=${BIN_DIR}/clashd
Environment=CLASHD_PORT=${CLASHD_PORT}
Environment=CLASHD_POLICY=${CLASHD_POLICY}
Environment=CLASHD_AGENTS=${AGENTS_JSON}
Environment=PATH=${SERVICE_PATH}
Restart=always
RestartSec=5
StandardOutput=append:${LOG_DIR}/clashd.log
StandardError=append:${LOG_DIR}/clashd.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
    $SYSTEMCTL daemon-reload
    enable_restart_service calciforge-clashd.service || \
        warn "systemctl failed — if running as non-root, run: loginctl enable-linger \$USER"
fi

sleep 1
curl -sf "http://localhost:${CLASHD_PORT}/health" > /dev/null \
    && ok "clashd running on :${CLASHD_PORT}" \
    || warn "clashd not yet responding — check $LOG_DIR/clashd.err"

# ══════════════════════════════════════════════════════════════════════════════
# 3. optional remote LLM scanner
# ══════════════════════════════════════════════════════════════════════════════
if [[ -n "$REMOTE_SCANNER_URL" ]]; then
    hdr "remote LLM scanner"
    mkdir -p "$SEC_LOG_DIR" "$(dirname "$REMOTE_SCANNER_API_KEY_FILE")" "$(dirname "$REMOTE_SCANNER_PROMPT_FILE")"

    if [[ -f "$REPO_ROOT/scripts/remote-llm-scanner.py" ]]; then
        install -m 755 "$REPO_ROOT/scripts/remote-llm-scanner.py" "$BIN_DIR/remote-llm-scanner"
        ok "Installed remote-llm-scanner → $BIN_DIR/remote-llm-scanner"
    elif [[ ! -x "$BIN_DIR/remote-llm-scanner" ]]; then
        warn "remote-llm-scanner script not found; skipping service setup"
        REMOTE_SCANNER_URL=""
    fi

    if [[ -n "$REMOTE_SCANNER_URL" ]]; then
        if [[ ! -s "$REMOTE_SCANNER_PROMPT_FILE" && -f "$REPO_ROOT/scripts/remote-llm-scanner-prompt.txt" ]]; then
            if install -m 600 "$REPO_ROOT/scripts/remote-llm-scanner-prompt.txt" "$REMOTE_SCANNER_PROMPT_FILE"; then
                ok "Seeded remote scanner prompt → $REMOTE_SCANNER_PROMPT_FILE"
            else
                warn "Failed to seed remote scanner prompt → $REMOTE_SCANNER_PROMPT_FILE"
            fi
        fi

        if [[ ! -s "$REMOTE_SCANNER_API_KEY_FILE" && -z "${REMOTE_SCANNER_API_KEY:-}" ]]; then
            warn "remote scanner API key not configured; write it to $REMOTE_SCANNER_API_KEY_FILE or set REMOTE_SCANNER_API_KEY"
        fi

        if [[ "$PLATFORM" == "Darwin" ]]; then
            SCANNER_PLIST="$PLIST_DIR/com.calciforge.remote-llm-scanner.plist"
            cat > "$SCANNER_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.calciforge.remote-llm-scanner</string>
    <key>ProgramArguments</key><array><string>${BIN_DIR}/remote-llm-scanner</string></array>
    <key>EnvironmentVariables</key><dict>
        <key>REMOTE_SCANNER_PORT</key><string>${REMOTE_SCANNER_PORT}</string>
        <key>REMOTE_SCANNER_API_KEY_FILE</key><string>${REMOTE_SCANNER_API_KEY_FILE}</string>
        <key>REMOTE_SCANNER_API_BASE</key><string>${REMOTE_SCANNER_API_BASE}</string>
        <key>REMOTE_SCANNER_MODEL</key><string>${REMOTE_SCANNER_MODEL}</string>
        <key>REMOTE_SCANNER_PROMPT_FILE</key><string>${REMOTE_SCANNER_PROMPT_FILE}</string>
        <key>PATH</key><string>${SERVICE_PATH}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${SEC_LOG_DIR}/remote-llm-scanner.log</string>
    <key>StandardErrorPath</key><string>${SEC_LOG_DIR}/remote-llm-scanner.err</string>
</dict></plist>
EOF
            load_launch_agent "com.calciforge.remote-llm-scanner" "$SCANNER_PLIST"
        else
            SCANNER_UNIT="$PLIST_DIR/calciforge-remote-llm-scanner.service"
            cat > "$SCANNER_UNIT" <<EOF
[Unit]
Description=Calciforge remote LLM security scanner
After=network.target

[Service]
Type=simple
ExecStart=${BIN_DIR}/remote-llm-scanner
Environment=REMOTE_SCANNER_PORT=${REMOTE_SCANNER_PORT}
Environment=REMOTE_SCANNER_API_KEY_FILE=${REMOTE_SCANNER_API_KEY_FILE}
Environment=REMOTE_SCANNER_API_BASE=${REMOTE_SCANNER_API_BASE}
Environment=REMOTE_SCANNER_MODEL=${REMOTE_SCANNER_MODEL}
Environment=REMOTE_SCANNER_PROMPT_FILE=${REMOTE_SCANNER_PROMPT_FILE}
Environment=PATH=${SERVICE_PATH}
Restart=always
RestartSec=5
StandardOutput=append:${SEC_LOG_DIR}/remote-llm-scanner.log
StandardError=append:${SEC_LOG_DIR}/remote-llm-scanner.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
            $SYSTEMCTL daemon-reload
            enable_restart_service calciforge-remote-llm-scanner.service || \
                warn "systemctl failed — if running as non-root, run: loginctl enable-linger \$USER"
        fi

        sleep 1
        curl -sf "${REMOTE_SCANNER_URL}/health" > /dev/null \
            && ok "remote LLM scanner running on :${REMOTE_SCANNER_PORT}" \
            || warn "remote LLM scanner not yet responding — check $SEC_LOG_DIR/remote-llm-scanner.err"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 4. security-proxy (launchd service)
# ══════════════════════════════════════════════════════════════════════════════
hdr "security-proxy"

mkdir -p "$SEC_LOG_DIR"
ensure_mitm_ca
trust_mitm_ca_if_supported
disable_legacy_local_service "calciforge-security-proxy" "${LEGACY_SERVICE_PREFIX}-security-proxy"
disable_legacy_local_service "calciforge-security-proxy" "${LEGACY_SERVICE_PREFIX}-proxy"

if [[ "$PLATFORM" == "Darwin" ]]; then
    SEC_PLIST="$PLIST_DIR/com.calciforge.security-proxy.plist"
    cat > "$SEC_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.calciforge.security-proxy</string>
    <key>ProgramArguments</key><array><string>${BIN_DIR}/security-proxy</string></array>
    <key>EnvironmentVariables</key><dict>
        <key>SECURITY_PROXY_PORT</key><string>${SECURITY_PROXY_PORT}</string>
        <key>SECURITY_PROXY_BIND</key><string>${SECURITY_PROXY_BIND}</string>
        <key>SECURITY_PROXY_MITM_ENABLED</key><string>${SECURITY_PROXY_MITM_ENABLED}</string>
        <key>SECURITY_PROXY_CA_CERT</key><string>${SECURITY_PROXY_CA_CERT}</string>
        <key>SECURITY_PROXY_CA_KEY</key><string>${SECURITY_PROXY_CA_KEY}</string>
        <key>SECURITY_PROXY_REMOTE_SCANNER_URL</key><string>${REMOTE_SCANNER_URL}</string>
        <key>SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED</key><string>${REMOTE_SCANNER_FAIL_CLOSED}</string>
        <key>CALCIFORGE_CONFIG_HOME</key><string>${CALCIFORGE_CONFIG_HOME}</string>
        <key>AGENT_CONFIG</key><string>${AGENTS_JSON}</string>
        <key>PATH</key><string>${SERVICE_PATH}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${SEC_LOG_DIR}/security-proxy.log</string>
    <key>StandardErrorPath</key><string>${SEC_LOG_DIR}/security-proxy.err</string>
</dict></plist>
EOF
    load_launch_agent "com.calciforge.security-proxy" "$SEC_PLIST"
else
    SEC_UNIT="$PLIST_DIR/calciforge-security-proxy.service"
    cat > "$SEC_UNIT" <<EOF
[Unit]
Description=Calciforge security-proxy (MITM traffic inspection)
After=network.target

[Service]
Type=simple
ExecStart=${BIN_DIR}/security-proxy
Environment=SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}
Environment=SECURITY_PROXY_BIND=${SECURITY_PROXY_BIND}
Environment=SECURITY_PROXY_MITM_ENABLED=${SECURITY_PROXY_MITM_ENABLED}
Environment=SECURITY_PROXY_CA_CERT=${SECURITY_PROXY_CA_CERT}
Environment=SECURITY_PROXY_CA_KEY=${SECURITY_PROXY_CA_KEY}
Environment=SECURITY_PROXY_REMOTE_SCANNER_URL=${REMOTE_SCANNER_URL}
Environment=SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED=${REMOTE_SCANNER_FAIL_CLOSED}
Environment=CALCIFORGE_CONFIG_HOME=${CALCIFORGE_CONFIG_HOME}
Environment=AGENT_CONFIG=${AGENTS_JSON}
Environment=PATH=${SERVICE_PATH}
Restart=always
RestartSec=5
StandardOutput=append:${SEC_LOG_DIR}/security-proxy.log
StandardError=append:${SEC_LOG_DIR}/security-proxy.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
    $SYSTEMCTL daemon-reload
    enable_restart_service calciforge-security-proxy.service || \
        warn "systemctl failed — if running as non-root, run: loginctl enable-linger \$USER"
fi

sleep 1
curl -sf "http://localhost:${SECURITY_PROXY_PORT}/health" > /dev/null \
    && ok "security-proxy running on :${SECURITY_PROXY_PORT}" \
    || warn "security-proxy not yet responding — check $SEC_LOG_DIR/security-proxy.err"

run_calciforge_doctor() {
    local mode="${1:-local}"
    if [[ -f "$ZC_CONFIG" && -x "$BIN_DIR/calciforge" ]]; then
        hdr "calciforge doctor (${mode})"
        local doctor_args=(--config "$ZC_CONFIG" doctor)
        if ! truthy "$CALCIFORGE_INSTALL_DOCTOR_NETWORK"; then
            doctor_args+=(--no-network)
        fi
        local doctor_env=()
        if ! truthy "$CALCIFORGE_INSTALL_DOCTOR_NETWORK" || truthy "$CALCIFORGE_INSTALL_DOCTOR_STRIP_PROXIES"; then
            doctor_env=(env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u NO_PROXY -u no_proxy)
        fi
        if [[ ${#doctor_env[@]} -gt 0 ]]; then
            "${doctor_env[@]}" "$BIN_DIR/calciforge" "${doctor_args[@]}" \
                || warn "calciforge doctor reported issues; see output above"
        else
            "$BIN_DIR/calciforge" "${doctor_args[@]}" \
                || warn "calciforge doctor reported issues; see output above"
        fi
    else
        warn "Skipping calciforge doctor — config or binary not available yet"
    fi
}

# ══════════════════════════════════════════════════════════════════════════════
# 5. fnox — encrypted secret resolver (fallback between env and vaultwarden)
# ══════════════════════════════════════════════════════════════════════════════
# secrets-client's vault.rs lookup order is: env → fnox → vaultwarden. fnox is
# not hard-required by the Rust resolver, but real channel/gateway deployments
# need it configured before services start so service PATH and HOME match the
# operator's interactive shell.
hdr "fnox (secret resolver)"
ensure_fnox || true

fi # !AGENTS_ONLY — core services above, agent runtimes below

# ══════════════════════════════════════════════════════════════════════════════
# 6. openclaw — package bootstrap + managed channel/plugin configuration
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled openclaw; then
    hdr "openclaw"
    ensure_npm openclaw || true

    if command -v openclaw &>/dev/null; then
        _oc_approvals_json='{"version":1,"defaults":{"tools.exec":{"security":"restricted","ask":"on"}},"agents":{"main":{"allowlist":["git","ls","cat","grep","find","echo","pwd","wc","head","tail","curl","wget","python","python3","node","npm","cargo","make","cmake","rustc"]}}}'
        local _timeout_cmd="timeout -k 3 15"
        if [[ "$PLATFORM" == "Darwin" ]]; then
            _timeout_cmd="$(command -v gtimeout 2>/dev/null || echo "")"
            [[ -n "$_timeout_cmd" ]] && _timeout_cmd="$_timeout_cmd -k 3 15" || _timeout_cmd=""
        fi
        if [[ -n "$_timeout_cmd" ]]; then
            echo "$_oc_approvals_json" | $_timeout_cmd openclaw approvals set --stdin >/dev/null 2>&1 || \
                warn "openclaw approvals set timed out or failed — skipping"
        else
            echo "$_oc_approvals_json" | openclaw approvals set --stdin >/dev/null 2>&1 || \
                warn "openclaw approvals set failed — skipping"
        fi
        ok "openclaw exec-approvals configured (restricted+ask, common tools allowlisted)"
        configure_managed_openclaw
    else
        warn "openclaw not available — skipping managed OpenClaw configuration"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 7. calciforge — main agent gateway (channels + router + proxy)
# ══════════════════════════════════════════════════════════════════════════════
# Runs as a system service so channels (Telegram, Matrix, WhatsApp) reconnect
# across reboots. Expects config at $CALCIFORGE_CONFIG_HOME/config.toml by
# default; users must populate it before the service starts (or the service
# will fail health and launchd/systemd will keep retrying).
if [[ "$AGENTS_ONLY" != true ]]; then
hdr "calciforge"

mkdir -p "$ZC_LOG_DIR"
disable_legacy_local_service "calciforge" "${LEGACY_SERVICE_PREFIX}"

_write_proxy_section() {
    local dest="$1" mode="${2:-overwrite}"
    local content
    content="[proxy]
enabled = true
bind = \"127.0.0.1:${CALCIFORGE_GATEWAY_PORT}\"
backend_type = \"http\"
timeout_seconds = 300"
    [[ -n "$CALCIFORGE_GATEWAY_BACKEND_URL" ]] && \
        content+=$'\n'"backend_url = \"${CALCIFORGE_GATEWAY_BACKEND_URL}\""
    [[ -n "$CALCIFORGE_GATEWAY_BACKEND_API_KEY_FILE" && -f "$CALCIFORGE_GATEWAY_BACKEND_API_KEY_FILE" ]] && \
        content+=$'\n'"backend_api_key_file = \"${CALCIFORGE_GATEWAY_BACKEND_API_KEY_FILE}\""

    if [[ "$mode" == "overwrite" ]]; then
        printf '[calciforge]\nversion = 2\n\n%s\n' "$content" > "$dest"
    else
        printf '\n%s\n' "$content" >> "$dest"
    fi
}

_ensure_proxy_enabled() {
    local config_path="$1"
    python3 - "$config_path" <<'PY'
import pathlib, re, sys

path = pathlib.Path(sys.argv[1])
text = path.read_text()
match = re.search(r'(?ms)^\[proxy\]\n.*?(?=^\[|\Z)', text)
if not match:
    raise SystemExit(1)

section = match.group(0)
if re.search(r'(?m)^enabled\s*=', section):
    new_section = re.sub(r'(?m)^enabled\s*=.*$', 'enabled = true', section, count=1)
else:
    new_section = section.replace('[proxy]\n', '[proxy]\nenabled = true\n', 1)

if new_section != section:
    path.write_text(text[:match.start()] + new_section + text[match.end():])
PY
}

if [[ ! -f "$ZC_CONFIG" ]]; then
    warn "Config not found at $ZC_CONFIG — creating minimal config with model gateway enabled"
    mkdir -p "$(dirname "$ZC_CONFIG")"
    _write_proxy_section "$ZC_CONFIG" overwrite
    ok "Created config at $ZC_CONFIG with model gateway on :${CALCIFORGE_GATEWAY_PORT}"
fi

# Ensure [proxy] section has enabled = true (idempotent)
if ! grep -q '^\[proxy\]' "$ZC_CONFIG" 2>/dev/null; then
    _write_proxy_section "$ZC_CONFIG" append
    ok "Added [proxy] section to config (model gateway on :${CALCIFORGE_GATEWAY_PORT})"
elif ! python3 -c "
import pathlib, re, sys
text = pathlib.Path('$ZC_CONFIG').read_text()
m = re.search(r'(?m)^\[proxy\].*?(?=^\[|\Z)', text, re.S)
sys.exit(0 if m and 'enabled = true' in m.group() else 1)
" 2>/dev/null; then
    _ensure_proxy_enabled "$ZC_CONFIG" || warn "Could not set [proxy].enabled = true in $ZC_CONFIG"
fi

if [[ "$PLATFORM" == "Darwin" ]]; then
    ZC_PLIST="$PLIST_DIR/com.calciforge.calciforge.plist"
    cat > "$ZC_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>com.calciforge.calciforge</string>
    <key>ProgramArguments</key><array>
        <string>${BIN_DIR}/calciforge</string>
        <string>--config</string>
        <string>${ZC_CONFIG}</string>
    </array>
    <key>EnvironmentVariables</key><dict>
        <key>RUST_LOG</key><string>calciforge=info</string>
        <key>CALCIFORGE_CONFIG_HOME</key><string>${CALCIFORGE_CONFIG_HOME}</string>
        <key>CALCIFORGE_FNOX_DIR</key><string>${CALCIFORGE_FNOX_DIR}</string>
        <key>FNOX_AGE_KEY_FILE</key><string>${FNOX_AGE_KEY_FILE}</string>
        <key>CALCIFORGE_REMOTE_SCANNER_URL</key><string>${REMOTE_SCANNER_URL}</string>
        <key>CALCIFORGE_REMOTE_SCANNER_FAIL_CLOSED</key><string>${REMOTE_SCANNER_FAIL_CLOSED}</string>
        <key>PATH</key><string>${SERVICE_PATH}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ThrottleInterval</key><integer>30</integer>
    <key>StandardOutPath</key><string>${ZC_LOG_DIR}/calciforge.log</string>
    <key>StandardErrorPath</key><string>${ZC_LOG_DIR}/calciforge.err</string>
</dict></plist>
EOF
    load_launch_agent "com.calciforge.calciforge" "$ZC_PLIST" 2>&1 | tail -3
else
    ZC_UNIT="$PLIST_DIR/calciforge.service"
    cat > "$ZC_UNIT" <<EOF
[Unit]
Description=Calciforge agent gateway (channels + router + proxy)
After=network.target calciforge-clashd.service calciforge-security-proxy.service
Wants=calciforge-clashd.service calciforge-security-proxy.service

[Service]
Type=simple
ExecStart=${BIN_DIR}/calciforge --config ${ZC_CONFIG}
Environment=RUST_LOG=calciforge=info
Environment=CALCIFORGE_CONFIG_HOME=${CALCIFORGE_CONFIG_HOME}
Environment=CALCIFORGE_FNOX_DIR=${CALCIFORGE_FNOX_DIR}
Environment=FNOX_AGE_KEY_FILE=${FNOX_AGE_KEY_FILE}
Environment=CALCIFORGE_REMOTE_SCANNER_URL=${REMOTE_SCANNER_URL}
Environment=CALCIFORGE_REMOTE_SCANNER_FAIL_CLOSED=${REMOTE_SCANNER_FAIL_CLOSED}
Environment=PATH=${SERVICE_PATH}
Restart=always
RestartSec=30
StandardOutput=append:${ZC_LOG_DIR}/calciforge.log
StandardError=append:${ZC_LOG_DIR}/calciforge.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
    $SYSTEMCTL daemon-reload
    enable_restart_service calciforge.service || \
        warn "systemctl failed — if running as non-root, run: loginctl enable-linger \$USER"
fi

# Give calciforge a moment to come up, then check if the process is alive.
# calciforge only binds a health port when proxy is enabled in config, so we
# can't rely on /health — probe the process instead.
sleep 2
if pgrep -f "${BIN_DIR}/calciforge" > /dev/null; then
    ok "calciforge running"
else
    warn "calciforge did not start — check $ZC_LOG_DIR/calciforge.err"
fi
fi # !AGENTS_ONLY — calciforge service/config

# ══════════════════════════════════════════════════════════════════════════════
# 8. Claude Code hook
# ══════════════════════════════════════════════════════════════════════════════
# ── acpx — required for any agent with kind = "acpx" (claude, opencode, kilo, …)
# Needs to be installed regardless of which specific agent is enabled, since
# calciforge's ACPX adapter spawns `acpx` as a subprocess. Missing acpx means
# "Failed to spawn acpx: No such file or directory" at first message dispatch.
if agent_enabled claude || agent_enabled opencode; then
    hdr "acpx (ACP agent runtime)"
    ensure_npm acpx || warn "ACPX-kind agents (claude-acpx, opencode, kilo-qwen) will be unavailable without acpx"
fi

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

            # Register mcp-server as an MCP server. The server runs
            # via stdio when claude spawns it as a subprocess. Idempotent:
            # the python block replaces any existing entry with the same
            # name, so re-running install.sh won't grow duplicates.
            ZC_MCP_BIN="$BIN_DIR/mcp-server"
            if [[ -x "$ZC_MCP_BIN" ]]; then
                python3 - "$SETTINGS" "$ZC_MCP_BIN" <<'PYEOF'
import json, sys
path, mcp_bin = sys.argv[1], sys.argv[2]
with open(path) as f: s = json.load(f)
servers = s.setdefault("mcpServers", {})
servers["calciforge-secrets"] = {
    "command": mcp_bin,
    "args": [],
    "env": {},
}
with open(path, "w") as f: json.dump(s, f, indent=2); f.write("\n")
print(f"settings.json: registered MCP server calciforge-secrets → {mcp_bin}")
PYEOF
                ok "Claude Code MCP server calciforge-secrets → ${ZC_MCP_BIN}"
            else
                warn "mcp-server binary not found at $ZC_MCP_BIN — skipping MCP registration"
                warn "  Build it with: cargo build --release -p mcp-server"
            fi
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 9. opencode
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled opencode; then
    hdr "opencode"
    # opencode on brew (mac), opencode-ai on npm (Linux)
    ensure_tool opencode opencode opencode-ai || true

    PLUGIN_DIR="$REPO_ROOT/scripts/opencode-clashd-plugin"
    if [[ -d "$PLUGIN_DIR" ]]; then
        (cd "$PLUGIN_DIR" && npm pack --quiet 2>/dev/null)
        TARBALL=$(ls "$PLUGIN_DIR"/*.tgz 2>/dev/null | tail -1)
        [[ -n "$TARBALL" ]] && opencode plugin calciforge-clashd-policy --global 2>/dev/null \
            && ok "opencode clashd plugin registered" \
            || warn "opencode plugin not registered (run opencode plugin calciforge-clashd-policy manually)"
    else
        warn "opencode clashd plugin not yet built (scripts/opencode-clashd-plugin/ pending)"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 10. dirac
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled dirac; then
    hdr "dirac"
    ensure_npm dirac-cli dirac || true

    if command -v dirac &>/dev/null; then
        ok "dirac CLI installed"
        warn "Authenticate once before first use: dirac auth"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 11. zeroclaw
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled zeroclaw; then
    hdr "zeroclaw"
    # zeroclaw is only distributed via homebrew tap on macOS right now.
    # On Linux, user needs to build from source; script will skip with a warning.
    if [[ "$PLATFORM" == "Darwin" ]]; then
        ensure_brew zeroclaw || true
    elif ! command -v zeroclaw &>/dev/null; then
        warn "zeroclaw has no Linux package — build from source: cargo install zeroclawlabs"
    fi

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
        elif [[ "$PLATFORM" == "Darwin" ]]; then
            if ! brew services list 2>/dev/null | grep -q "zeroclaw.*started"; then
                brew services start zeroclaw 2>/dev/null \
                    && ok "zeroclaw service started" \
                    || warn "Could not start zeroclaw service — run: zeroclaw daemon"
            else
                ok "zeroclaw service already running"
            fi
        else
            warn "Start zeroclaw manually on Linux: zeroclaw daemon &"
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 12. ironclaw
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled ironclaw; then
    hdr "IronClaw Agent OS"
    IRONCLAW_DIR="$CALCIFORGE_IRONCLAW_INSTALL_DIR"

    if [[ "$CONFIGURE_ONLY" != true ]]; then
        # Prefer building from source (ensures adapter ↔ binary version sync).
        # Falls back to GitHub release if source not available.
        ironclaw_src="${CALCIFORGE_IRONCLAW_SOURCE:-$IRONCLAW_DIR/src}"
        # Clone source if not already present
        if [[ ! -f "$ironclaw_src/Cargo.toml" ]] && command -v git &>/dev/null; then
            echo "  Cloning ironclaw source..."
            git clone --depth 1 https://github.com/nearai/ironclaw "$ironclaw_src" 2>&1 | tail -2 || true
        fi
        if [[ -f "$ironclaw_src/Cargo.toml" ]] && grep -q 'name.*=.*"ironclaw"' "$ironclaw_src/Cargo.toml" 2>/dev/null; then
            echo "  Building ironclaw from source ($ironclaw_src)..."
            cargo="${CARGO:-$HOME/.cargo/bin/cargo}"
            if "$cargo" build --release --manifest-path "$ironclaw_src/Cargo.toml" -p ironclaw 2>&1 | tail -3; then
                built="$ironclaw_src/target/release/ironclaw"
                if [[ -f "$built" ]]; then
                    mkdir -p "$IRONCLAW_DIR/bin"
                    install -m 755 "$built" "$IRONCLAW_DIR/bin/ironclaw" 2>/dev/null || {
                        rm -f "$IRONCLAW_DIR/bin/ironclaw"
                        cp "$built" "$IRONCLAW_DIR/bin/ironclaw"
                        chmod +x "$IRONCLAW_DIR/bin/ironclaw"
                    }
                    [[ -w "$BIN_DIR" ]] && ln -sf "$IRONCLAW_DIR/bin/ironclaw" "$BIN_DIR/ironclaw"
                    ok "ironclaw built from source ($("$IRONCLAW_DIR/bin/ironclaw" --version 2>&1 | head -1 || true))"
                else
                    warn "ironclaw build produced no binary — falling back to GitHub release"
                    ensure_agent_binary "ironclaw" "ironclaw" "nearai/ironclaw" "$IRONCLAW_DIR"
                fi
            else
                warn "ironclaw source build failed — falling back to GitHub release"
                ensure_agent_binary "ironclaw" "ironclaw" "nearai/ironclaw" "$IRONCLAW_DIR"
            fi
        else
            ensure_agent_binary "ironclaw" "ironclaw" "nearai/ironclaw" "$IRONCLAW_DIR"
        fi
    else
        command -v ironclaw &>/dev/null || warn "ironclaw not found (run without --configure-only to install)"
    fi

    if command -v ironclaw &>/dev/null; then
        # Generate a shared webhook secret for IronClaw ↔ Calciforge auth.
        # Stored in IronClaw's .env (HTTP_WEBHOOK_SECRET) and a separate file
        # that Calciforge reads via api_key_file.
        secret_file="$IRONCLAW_DIR/webhook-secret"
        if [[ ! -f "$secret_file" ]]; then
            mkdir -p "$IRONCLAW_DIR"
            python3 -c "import secrets; print(secrets.token_hex(32))" > "$secret_file"
            chmod 600 "$secret_file"
            ok "Generated IronClaw webhook secret"
        fi
        webhook_secret="$(cat "$secret_file")"

        ca_cert="${SECURITY_PROXY_CA_CERT}"
        gateway_url="${CALCIFORGE_IRONCLAW_MODEL_GATEWAY:-http://127.0.0.1:18083/v1}"
        gateway_model="${CALCIFORGE_IRONCLAW_DEFAULT_MODEL:-kimi-k2.5}"
        # Read the gateway's inbound API key so IronClaw can authenticate
        gateway_api_key=""
        if [[ -f "$ZC_CONFIG" ]]; then
            gateway_api_key="$(python3 -c "
import pathlib, re, sys
text = pathlib.Path('$ZC_CONFIG').read_text()
m = re.search(r'^\[proxy\].*?^api_key_file\s*=\s*\"([^\"]+)\"', text, re.M|re.S)
if m:
    p = pathlib.Path(m.group(1)).expanduser()
    if p.exists(): print(p.read_text().strip())
else:
    m2 = re.search(r'^\[proxy\].*?^api_key\s*=\s*\"([^\"]+)\"', text, re.M|re.S)
    if m2: print(m2.group(1))
" 2>/dev/null || true)"
        fi
        ensure_agent_env "$IRONCLAW_DIR/.env" <<ENVEOF
IRONCLAW_PROFILE=server
HTTP_PORT=${CALCIFORGE_IRONCLAW_PORT}
HTTP_WEBHOOK_SECRET=${webhook_secret}
LLM_BACKEND=${CALCIFORGE_IRONCLAW_LLM_BACKEND}
LLM_BASE_URL=${gateway_url}
LLM_MODEL=${gateway_model}
${gateway_api_key:+LLM_API_KEY=${gateway_api_key}}
DATABASE_BACKEND=libsql
ONBOARD_COMPLETED=true
CLI_MODE=repl
CLI_ENABLED=false
GATEWAY_PORT=${CALCIFORGE_IRONCLAW_GATEWAY_PORT:-3001}
# Route IronClaw's outbound HTTP through Calciforge security proxy for
# credential injection, leak scanning, and policy enforcement.
HTTP_PROXY=${SECURITY_PROXY_URL}
HTTPS_PROXY=${SECURITY_PROXY_URL}
NO_PROXY=localhost,127.0.0.1,::1
# Trust Calciforge MITM CA for HTTPS inspection
SSL_CERT_FILE=${ca_cert}
REQUESTS_CA_BUNDLE=${ca_cert}
ENVEOF
        # Ensure webhook secret is present in existing .env files (idempotent)
        if ! grep -q "^HTTP_WEBHOOK_SECRET=" "$IRONCLAW_DIR/.env" 2>/dev/null; then
            echo "HTTP_WEBHOOK_SECRET=${webhook_secret}" >> "$IRONCLAW_DIR/.env"
        fi

        # Ensure HTTP channel is enabled and gateway uses separate port
        ironclaw config set channels.http_enabled true >/dev/null 2>&1 || true
        ironclaw config set channels.http_port "${CALCIFORGE_IRONCLAW_PORT}" >/dev/null 2>&1 || true
        ironclaw config set channels.http_host "0.0.0.0" >/dev/null 2>&1 || true
        ironclaw config set channels.cli_enabled false >/dev/null 2>&1 || true
        ironclaw config set channels.cli_mode repl >/dev/null 2>&1 || true
        ironclaw config set channels.gateway_port "${CALCIFORGE_IRONCLAW_GATEWAY_PORT:-3001}" >/dev/null 2>&1 || true

        # LLM backend is configured via env vars in .env above (LLM_BASE_URL,
        # LLM_BACKEND, LLM_MODEL). Also write to settings DB for CLI usage.
        ironclaw config set openai_compatible_base_url "$gateway_url" >/dev/null 2>&1 || true
        ironclaw config set llm_backend openai_compatible >/dev/null 2>&1 || true
        ironclaw config set selected_model "$gateway_model" >/dev/null 2>&1 || true

        if [[ ! -s "$IRONCLAW_DIR/.env" ]]; then
            warn "Edit $IRONCLAW_DIR/.env to set API keys (ANTHROPIC_API_KEY, etc.)"
        fi

        ensure_agent_service "ironclaw" "$(command -v ironclaw)" "$IRONCLAW_DIR" \
            "$IRONCLAW_DIR/.env" "IronClaw Agent OS" "--no-onboard"

        sleep 1
        if curl -sf "http://localhost:${CALCIFORGE_IRONCLAW_PORT}/health" > /dev/null 2>&1 || \
           pgrep -f ironclaw > /dev/null 2>&1; then
            ok "IronClaw running on :${CALCIFORGE_IRONCLAW_PORT}"
        else
            warn "IronClaw not yet responding — check logs or set API keys in $IRONCLAW_DIR/.env"
        fi

        ensure_calciforge_agent_config "ironclaw" "ironclaw" \
            "$CALCIFORGE_IRONCLAW_ENDPOINT" 300000 "iron" "$secret_file"
    fi
fi

# ══════════════════════════════════════════════════════════════════════════════
# 12b. hermes-agent (NousResearch)
# ══════════════════════════════════════════════════════════════════════════════
if agent_enabled hermes; then
    hdr "Hermes Agent (NousResearch)"
    HERMES_DIR="$CALCIFORGE_HERMES_INSTALL_DIR"

    if [[ "$CONFIGURE_ONLY" != true ]]; then
        # Hermes is a Python package — requires python3 + uv (or pip)
        if ! command -v python3 &>/dev/null; then
            warn "python3 not found — skipping Hermes installation"
        else
            mkdir -p "$HERMES_DIR"

            # Install uv if not present (Hermes's preferred package manager)
            if ! command -v uv &>/dev/null; then
                if command -v brew &>/dev/null; then
                    echo "  Installing uv with Homebrew..."
                    brew install uv 2>&1 | tail -5 || warn "Homebrew uv install failed; falling back to pip for Hermes"
                elif command -v apt-get &>/dev/null; then
                    echo "  Installing Python venv support for Hermes fallback..."
                    apt-get update -qq && apt-get install -y -qq python3-venv 2>&1 | tail -5 || warn "python3-venv install failed; Hermes pip fallback may fail"
                else
                    warn "uv not found and no supported package manager detected; falling back to venv pip for Hermes"
                fi
            fi

            # Clone or update hermes-agent source
            if [[ ! -f "$HERMES_DIR/pyproject.toml" ]]; then
                echo "  Cloning hermes-agent..."
                git clone --depth 1 https://github.com/NousResearch/hermes-agent "$HERMES_DIR" 2>&1 | tail -2 || true
            else
                echo "  Updating hermes-agent..."
                git -C "$HERMES_DIR" pull --ff-only 2>&1 | tail -2 || true
            fi

            # Install dependencies
            if [[ -f "$HERMES_DIR/pyproject.toml" ]]; then
                echo "  Installing Hermes dependencies..."
                (cd "$HERMES_DIR" && uv sync --quiet 2>&1 | tail -3) || \
                    (cd "$HERMES_DIR" && python3 -m venv .venv && .venv/bin/pip install -e . --quiet 2>&1 | tail -3) || \
                    warn "Hermes dependency install failed"
            fi

            # Create wrapper script for service invocation
            hermes_bin="$HERMES_DIR/.venv/bin/hermes"
            if [[ ! -f "$hermes_bin" ]]; then
                hermes_bin="$(command -v hermes 2>/dev/null || echo "")"
            fi

            if [[ -n "$hermes_bin" && -x "$hermes_bin" ]]; then
                ok "hermes installed at $hermes_bin"
            else
                warn "hermes binary not found after install"
            fi
        fi
    fi

    if command -v hermes &>/dev/null || [[ -x "$HERMES_DIR/.venv/bin/hermes" ]]; then
        hermes_bin="${HERMES_DIR}/.venv/bin/hermes"
        [[ -x "$hermes_bin" ]] || hermes_bin="$(command -v hermes)"

        # Generate API server key for Calciforge to authenticate with Hermes
        hermes_api_key_file="$HERMES_DIR/.api-server-key"
        if [[ ! -f "$hermes_api_key_file" ]]; then
            python3 -c "import secrets; print(secrets.token_hex(32))" > "$hermes_api_key_file"
            chmod 600 "$hermes_api_key_file"
        fi
        hermes_api_key="$(cat "$hermes_api_key_file")"

        # Extract gateway URL and API key from Calciforge config
        gateway_url="http://127.0.0.1:${CALCIFORGE_GATEWAY_PORT}/v1"
        gateway_model="${CALCIFORGE_HERMES_DEFAULT_MODEL}"
        gateway_api_key=""
        if [[ -f "$ZC_CONFIG" ]]; then
            gateway_api_key="$(python3 - "$ZC_CONFIG" <<'PYEOF'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).expanduser().read_text()
m = re.search(r'^\[proxy\].*?^api_key_file\s*=\s*\"([^\"]+)\"', text, re.M|re.S)
if m:
    p = pathlib.Path(m.group(1)).expanduser()
    if p.exists(): print(p.read_text().strip())
else:
    m2 = re.search(r'^\[proxy\].*?^api_key\s*=\s*\"([^\"]+)\"', text, re.M|re.S)
    if m2: print(m2.group(1))
PYEOF
            )"
        fi

        # Write Hermes config.yaml pointing at Calciforge gateway
        hermes_config_dir="$HOME/.hermes"
        mkdir -p "$hermes_config_dir"
        if [[ -z "$gateway_api_key" ]]; then
            warn "Calciforge gateway API key not found; writing Hermes provider without api_key"
        fi
        python3 - "$hermes_config_dir/config.yaml" "$gateway_model" "$gateway_url" "$gateway_api_key" "$CALCIFORGE_HERMES_PORT" "$hermes_api_key" <<'PYEOF'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1]).expanduser()
gateway_model, gateway_url, gateway_api_key, hermes_port, hermes_api_key = sys.argv[2:7]

def q(value: str) -> str:
    return json.dumps(value)

provider = [
    "- name: calciforge",
    f"  base_url: {q(gateway_url)}",
]
if gateway_api_key:
    provider.append(f"  api_key: {q(gateway_api_key)}")
provider.extend([
    f"  model: {q(gateway_model)}",
    "  api_mode: chat_completions",
])

path.write_text(
    "\n".join([
        "# Managed by Calciforge installer - edits may be overwritten",
        "model:",
        f"  default: {q(gateway_model)}",
        "  provider: calciforge",
        "custom_providers:",
        *provider,
        "",
        "platforms:",
        "  api_server:",
        "    enabled: true",
        "    extra:",
        "      host: \"127.0.0.1\"",
        f"      port: {int(hermes_port)}",
        f"      api_key: {q(hermes_api_key)}",
        "",
    ]),
    encoding="utf-8",
)
PYEOF
        chmod 600 "$hermes_config_dir/config.yaml"

        # Write .env for the service
        # Note: no HTTP_PROXY — Hermes talks only to the local gateway which
        # handles outbound routing itself. Proxy vars cause localhost 401s.
        ensure_agent_env "$HERMES_DIR/.env" <<ENVEOF
API_SERVER_KEY=${hermes_api_key}
HERMES_QUIET=1
GATEWAY_ALLOW_ALL_USERS=true
NO_PROXY=localhost,127.0.0.1,::1
ENVEOF

        # Start Hermes gateway in foreground mode (our service manages the process)
        ensure_agent_service "hermes" "$hermes_bin" "$HERMES_DIR" \
            "$HERMES_DIR/.env" "Hermes Agent (NousResearch)" "gateway run --accept-hooks"

        # Verify it's running
        sleep 3
        if curl -sf "http://127.0.0.1:${CALCIFORGE_HERMES_PORT}/health" > /dev/null 2>&1; then
            ok "Hermes running on :${CALCIFORGE_HERMES_PORT}"
        else
            warn "Hermes not responding on :${CALCIFORGE_HERMES_PORT} (may need a moment to start)"
        fi

        # Register in Calciforge config
        ensure_calciforge_agent_config "hermes" "hermes" \
            "$CALCIFORGE_HERMES_ENDPOINT" 600000 "h,nous" "$hermes_api_key_file"
    fi
fi

fi # !NODES_ONLY

# ══════════════════════════════════════════════════════════════════════════════
# 13. Multi-node SSH deployment
# ══════════════════════════════════════════════════════════════════════════════

if [[ -n "$NODES_FILE" ]]; then
    hdr "Multi-node deployment"

    [[ -f "$NODES_FILE" ]] || die "Nodes file not found: $NODES_FILE"
    command -v python3 &>/dev/null || die "python3 required for node deployment"

    mkdir -p "$(dirname "$INSTALL_NODES_STATE")"
    python3 - "$NODES_FILE" "$INSTALL_NODES_STATE" "$SECURITY_PROXY_BIND" <<'PYEOF'
import json
import sys

src, dst, default_bind = sys.argv[1], sys.argv[2], sys.argv[3]
with open(src) as f:
    data = json.load(f)
nodes = []
for node in data.get("nodes", []):
    nodes.append({
        "name": node.get("name", node["host"]),
        "host": node["host"],
        "user": node.get("user", "root"),
        "ssh_key": node.get("ssh_key", ""),
        "arch": node.get("arch", "x86_64-unknown-linux-musl"),
        "os": node.get("os", "linux"),
        "services": node.get("services", ["clashd", "security-proxy"]),
        "install_dir": node.get("install_dir", "/usr/local/bin"),
        "config_dir": node.get("config_dir", "/etc/calciforge"),
        "security_proxy_bind": node.get("security_proxy_bind", default_bind),
    })
with open(dst, "w") as f:
    json.dump({"nodes": nodes}, f, indent=2)
    f.write("\n")
PYEOF
    chmod 600 "$INSTALL_NODES_STATE"
    ok "Persisted install-node metadata for doctor → $INSTALL_NODES_STATE"

    # ── binary build cache: arch+bin → path ──────────────────────────────────
    # Use indexed arrays instead of associative arrays so the installer works
    # with macOS' default Bash 3.2.
    BUILT_KEYS=()
    BUILT_VALUES=()

    built_cache_get() {
        local key="$1" i
        for ((i=0; i<${#BUILT_KEYS[@]}; i++)); do
            if [[ "${BUILT_KEYS[$i]}" == "$key" ]]; then
                echo "${BUILT_VALUES[$i]}"
                return 0
            fi
        done
        return 1
    }

    built_cache_put() {
        local key="$1" value="$2" i
        for ((i=0; i<${#BUILT_KEYS[@]}; i++)); do
            if [[ "${BUILT_KEYS[$i]}" == "$key" ]]; then
                BUILT_VALUES[$i]="$value"
                return 0
            fi
        done
        BUILT_KEYS+=("$key")
        BUILT_VALUES+=("$value")
    }

    build_for_arch() {
        local target="$1" bin="$2"
        local cache_key="${target}:${bin}"
        local cached
        if cached="$(built_cache_get "$cache_key")"; then
            echo "$cached"
            return 0
        fi

        local out_path="$REPO_ROOT/target/${target}/release/${bin}"
        local package="$bin"
        if [[ "$bin" == "calciforge-secrets" ]]; then
            package="secrets-client"
        fi
        local cargo_args=(build --release -p "$package" --bin "$bin" --target "$target")
        if [[ "$bin" == "calciforge" ]]; then
            cargo_args+=(--features channel-matrix)
        fi

        if [[ "$target" == "aarch64-apple-darwin" ]]; then
            # Native — use already-built binary if present
            local native="$REPO_ROOT/target/release/${bin}"
            if [[ -f "$native" ]]; then
                built_cache_put "$cache_key" "$native"
                echo "$native"; return
            fi
        fi

        echo "  Building $bin for $target..." >&2
        if command -v cross &>/dev/null; then
            run_build "$bin for $target" cross "${cargo_args[@]}"
        elif command -v cargo-zigbuild &>/dev/null; then
            run_build "$bin for $target" cargo zigbuild "${cargo_args[@]:1}"
        elif command -v docker &>/dev/null && [[ "$target" == x86_64-unknown-linux-* ]]; then
            local platform="linux/amd64"
            local docker_target="$target"
            if [[ "$target" == "x86_64-unknown-linux-musl" ]]; then
                warn "Docker fallback builds GNU libc binaries; use arch=x86_64-unknown-linux-gnu for glibc Linux nodes" >&2
                echo ""; return 1
            fi
            local host_uid host_gid
            host_uid="$(id -u)"
            host_gid="$(id -g)"
            local docker_target_dir="target/docker-${target}"
            run_build "$bin for $target via Docker" docker run --rm --platform "$platform" \
                -v "$REPO_ROOT:/work" -w /work rust:1-bookworm bash -lc \
                "export PATH=/usr/local/cargo/bin:\$PATH CARGO_TARGET_DIR='$docker_target_dir' && apt-get update -qq >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libssl-dev libudev-dev cmake protobuf-compiler clang >/dev/null && cargo build --release -p '$package' --bin '$bin' --target '$docker_target' $([[ "$bin" == "calciforge" ]] && printf '%s' '--features channel-matrix') && chown -R '$host_uid:$host_gid' '$docker_target_dir'"
            out_path="$REPO_ROOT/${docker_target_dir}/${target}/release/${bin}"
        else
            warn "No cross-compilation tool found (install 'cross' or 'cargo-zigbuild')" >&2
            echo ""; return 1
        fi

        [[ -f "$out_path" ]] && built_cache_put "$cache_key" "$out_path" && echo "$out_path" || \
            { warn "Build failed for $target/$bin"; echo ""; return 1; }
    }

    deploy_binary_only() {
        local name="$1" host="$2" user="$3" ssh_key="$4" arch="$5" bin="$6" install_dir="$7"
        local ssh_opts=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
        [[ -n "$ssh_key" ]] && ssh_opts+=(-i "$ssh_key")
        local ssh_target="${user}@${host}"
        local rsync_ssh
        printf -v rsync_ssh '%q ' ssh "${ssh_opts[@]}"

        echo "  [$name] deploying support binary $bin..."
        local bin_path
        bin_path=$(build_for_arch "$arch" "$bin") || {
            warn "  [$name] no local/cross binary for support binary $bin on $arch"
            return 1
        }
        [[ -z "$bin_path" || ! -f "$bin_path" ]] && {
            warn "  [$name] no binary available for support binary $bin on $arch — skipping"
            return 1
        }

        ssh "${ssh_opts[@]}" "$ssh_target" "mkdir -p $install_dir" 2>/dev/null
        local remote_tmp="/tmp/calciforge-install-${bin}-$$"
        if command -v rsync >/dev/null 2>&1 && \
            ssh "${ssh_opts[@]}" "$ssh_target" "rsync --version >/dev/null 2>&1" 2>/dev/null; then
            rsync -az --checksum -e "$rsync_ssh" "$bin_path" "${ssh_target}:${remote_tmp}"
        else
            scp "${ssh_opts[@]}" "$bin_path" "${ssh_target}:${remote_tmp}"
        fi
        ssh "${ssh_opts[@]}" "$ssh_target" "install -m 0755 ${remote_tmp} ${install_dir}/${bin} && rm -f ${remote_tmp}"
        ok "  [$name] support binary $bin deployed"
    }

    # ── systemd unit generator ────────────────────────────────────────────────
    systemd_unit() {
        local bin="$1" install_dir="$2" env_pairs="$3"
        local service_path="${4:-$SERVICE_PATH}"
        local exec_args="${5:-}"
        local wanted_by="${6:-$WANTED_BY_TARGET}"
        local env_lines="Environment=\"PATH=${service_path}\"\n"
        while IFS='=' read -r k v; do
            [[ -z "$k" ]] && continue
            env_lines+="Environment=\"${k}=${v}\"\n"
        done <<< "$env_pairs"

        printf '[Unit]\nDescription=Calciforge %s\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=%s/%s%s\n%s\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=%s\n' \
            "$bin" "$install_dir" "$bin" "$exec_args" "$(printf '%b' "$env_lines")" "$wanted_by"
    }

    # ── launchd plist generator ───────────────────────────────────────────────
    xml_escape() {
        python3 - "${1-}" <<'PYEOF'
import html
import sys

print(html.escape(sys.argv[1], quote=True), end="")
PYEOF
    }

    launchd_plist() {
        local bin="$1" install_dir="$2" label="com.calciforge.${bin}" log_dir="$3"
        local env_block=""
        shift 3
        for pair in "$@"; do
            local k="${pair%%=*}" v="${pair#*=}"
            local escaped_k escaped_v
            escaped_k="$(xml_escape "$k")"
            escaped_v="$(xml_escape "$v")"
            env_block+="        <key>${escaped_k}</key><string>${escaped_v}</string>\n"
        done

        printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>\n    <key>Label</key><string>%s</string>\n    <key>ProgramArguments</key><array><string>%s/%s</string></array>\n    <key>EnvironmentVariables</key><dict>\n%s    </dict>\n    <key>RunAtLoad</key><true/>\n    <key>KeepAlive</key><true/>\n    <key>StandardOutPath</key><string>%s/%s.log</string>\n    <key>StandardErrorPath</key><string>%s/%s.err</string>\n</dict></plist>\n' \
            "$(xml_escape "$label")" "$(xml_escape "$install_dir")" "$(xml_escape "$bin")" "$(printf '%b' "$env_block")" \
            "$(xml_escape "$log_dir")" "$(xml_escape "$bin")" "$(xml_escape "$log_dir")" "$(xml_escape "$bin")"
    }

    ensure_remote_fnox() {
        local name="$1" ssh_target="$2" ssh_key="$3" config_dir="$4"
        local provider_type_arg="${CALCIFORGE_FNOX_PROVIDER_TYPE:-__calciforge_default__}"
        local ssh_opts=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
        [[ -n "$ssh_key" ]] && ssh_opts+=(-i "$ssh_key")

        echo "  [$name] checking fnox..."
        ssh "${ssh_opts[@]}" "$ssh_target" 'bash -s' -- "$CALCIFORGE_FNOX_PROVIDER_NAME" "$provider_type_arg" "$config_dir" <<'REMOTE_FNOX'
set -euo pipefail
provider_name="$1"
provider_type="${2:-}"
if [[ "$provider_type" == "__calciforge_default__" ]]; then
    provider_type=""
fi
config_dir="$3"
age_key_file="${config_dir}/secrets/fnox-age-ed25519"
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
mkdir -p "${config_dir}" "${config_dir}/secrets"
if ! command -v fnox >/dev/null 2>&1; then
    if [[ "$(uname -s)" == "Darwin" ]] && command -v brew >/dev/null 2>&1; then
        brew install fnox >/dev/null
    elif command -v curl >/dev/null 2>&1 && command -v tar >/dev/null 2>&1; then
        os="$(uname -s)"
        arch="$(uname -m)"
        case "${os}:${arch}" in
            Linux:x86_64|Linux:amd64) asset="fnox-x86_64-unknown-linux-gnu.tar.gz" ;;
            Linux:aarch64|Linux:arm64) asset="fnox-aarch64-unknown-linux-gnu.tar.gz" ;;
            Darwin:x86_64) asset="fnox-x86_64-apple-darwin.tar.gz" ;;
            Darwin:arm64|Darwin:aarch64) asset="fnox-aarch64-apple-darwin.tar.gz" ;;
            *) asset="" ;;
        esac
        if [[ -n "$asset" ]]; then
            tmp="$(mktemp -d)"
            trap 'rm -rf "$tmp"' EXIT
            version="${FNOX_VERSION:-v1.23.0}"
            curl -fsSL "https://github.com/jdx/fnox/releases/download/${version}/${asset}" -o "$tmp/fnox.tar.gz"
            tar -xzf "$tmp/fnox.tar.gz" -C "$tmp"
            if [[ -w /usr/local/bin || "$(id -u)" -eq 0 ]]; then
                install -m 0755 "$tmp/fnox" /usr/local/bin/fnox
            else
                mkdir -p "$HOME/.local/bin"
                install -m 0755 "$tmp/fnox" "$HOME/.local/bin/fnox"
            fi
        elif command -v cargo >/dev/null 2>&1; then
            cargo install fnox >/dev/null
        else
            echo "fnox missing and no supported release asset is available" >&2
            exit 2
        fi
    elif command -v cargo >/dev/null 2>&1; then
        if command -v apt-get >/dev/null 2>&1 && { ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists libudev; }; then
            apt-get update -qq
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libudev-dev >/dev/null
        fi
        cargo install fnox >/dev/null
    else
        echo "fnox missing and neither brew nor cargo is available" >&2
        exit 2
    fi
fi
if [[ -x "$HOME/.cargo/bin/fnox" && ! -e /usr/local/bin/fnox && -w /usr/local/bin ]]; then
    ln -s "$HOME/.cargo/bin/fnox" /usr/local/bin/fnox 2>/dev/null || true
fi
if ! (cd "$config_dir" && fnox list >/dev/null 2>&1); then
    fnox init --global --skip-wizard >/dev/null
fi
provider_count="$(fnox provider list 2>/dev/null | awk 'NF { count++ } END { print count + 0 }')"
if [[ "$provider_count" -eq 0 ]]; then
    if [[ -z "$provider_type" && "$(uname -s)" == "Darwin" ]]; then
        provider_type="keychain"
    elif [[ -z "$provider_type" ]]; then
        provider_type="age"
    fi
    if [[ "$provider_type" == "age" ]]; then
        if [[ ! -f "$age_key_file" ]]; then
            ssh-keygen -q -t ed25519 -N "" -C "calciforge-fnox@$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo host)" -f "$age_key_file"
        fi
        chmod 600 "$age_key_file" 2>/dev/null || true
        chmod 644 "${age_key_file}.pub" 2>/dev/null || true
        config_file="${FNOX_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/fnox}/config.toml"
        mkdir -p "$(dirname "$config_file")"
        provider_name_escaped="$(printf '%s' "$provider_name" | sed 's/\\/\\\\/g; s/"/\\"/g')"
        recipient="$(sed 's/\\/\\\\/g; s/"/\\"/g' "${age_key_file}.pub")"
        {
            echo ""
            echo "[providers.\"${provider_name_escaped}\"]"
            echo "type = \"age\""
            printf "recipients = [\"%s\"]\n" "$recipient"
        } >> "$config_file"
        FNOX_AGE_KEY_FILE="$age_key_file" fnox provider test "$provider_name" >/dev/null
    elif [[ -n "$provider_type" ]]; then
        fnox provider add "$provider_name" "$provider_type" --global >/dev/null
        fnox provider test "$provider_name" >/dev/null
    else
        echo "fnox has no provider configured; remote secret paste storage will not work until one is added" >&2
    fi
fi
export FNOX_AGE_KEY_FILE="$age_key_file"
cd "$config_dir"
fnox list >/dev/null
REMOTE_FNOX
        ok "  [$name] fnox ready"
    }

    preflight_node() {
        local name="$1" host="$2" user="$3" ssh_key="$4" os="$5"
        local services="$6" install_dir="$7" config_dir="$8"
        local ssh_opts=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o BatchMode=yes)
        [[ -n "$ssh_key" ]] && ssh_opts+=(-i "$ssh_key")
        local ssh_target="${user}@${host}"

        echo "  [$name] preflight SSH and permissions..."
        ssh "${ssh_opts[@]}" "$ssh_target" 'bash -s' -- \
            "$name" "$os" "$services" "$install_dir" "$config_dir" <<'REMOTE_PREFLIGHT'
set -euo pipefail
name="$1"; os="$2"; services="$3"; install_dir="$4"; config_dir="$5"

if [[ -e /etc/pve/.version || -d /etc/pve/nodes ]]; then
    echo "refusing to deploy Calciforge services directly to Proxmox host node '$name'; target a VM/LXC guest instead" >&2
    exit 9
fi

if [[ "$os" == "linux" && "$(id -u)" != "0" ]]; then
    echo "linux node '$name' must be installed by root for systemd units and /usr/local/bin writes; rerun with user=root or install via a root-capable SSH target" >&2
    exit 10
fi

mkdir -p "$install_dir" "$config_dir"
for dir in "$install_dir" "$config_dir"; do
    if [[ ! -d "$dir" ]]; then
        echo "required directory does not exist after mkdir: $dir" >&2
        exit 11
    fi
    if [[ ! -w "$dir" ]]; then
        echo "no write permission for required directory: $dir" >&2
        exit 12
    fi
    tmp="$dir/.calciforge-permission-test.$$"
    : > "$tmp"
    rm -f "$tmp"
done

if [[ "$os" == "linux" ]]; then
    command -v systemctl >/dev/null 2>&1 || {
        echo "systemctl not found on linux node '$name'" >&2
        exit 13
    }
    if [[ ! -w /etc/systemd/system ]]; then
        echo "no write permission for /etc/systemd/system on linux node '$name'" >&2
        exit 14
    fi
fi

echo "OK"
REMOTE_PREFLIGHT
        ok "  [$name] SSH and remote permissions ready"
    }

    # ── deploy one service to one node ───────────────────────────────────────
    deploy_service() {
        local name="$1" host="$2" user="$3" ssh_key="$4" arch="$5" os="$6"
        local bin="$7" install_dir="$8" config_dir="$9" security_proxy_bind="${10:-127.0.0.1}"

        local ssh_opts=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
        [[ -n "$ssh_key" ]] && ssh_opts+=(-i "$ssh_key")
        local rsync_ssh
        printf -v rsync_ssh '%q ' ssh "${ssh_opts[@]}"
        local ssh_target="${user}@${host}"
        local remote_home remote_service_path
        remote_home=$(ssh "${ssh_opts[@]}" "$ssh_target" 'printf "%s" "$HOME"' 2>/dev/null || true)
        if [[ -z "$remote_home" && "$user" == "root" ]]; then
            remote_home="/root"
        fi
        remote_service_path="${install_dir}:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin"
        if [[ -n "$remote_home" && "$remote_home" = /* ]]; then
            remote_service_path="${install_dir}:${remote_home}/.cargo/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin"
        fi
        local remote_wanted_by="default.target"
        [[ "$os" == "linux" && "$user" == "root" ]] && remote_wanted_by="multi-user.target"

        echo "  [$name] deploying $bin..."
        local service_name="$bin"
        local legacy_units=()
        case "$bin" in
            clashd)
                service_name="calciforge-clashd"
                legacy_units=("clashd" "${LEGACY_SERVICE_PREFIX}-clashd")
                ;;
            security-proxy)
                service_name="calciforge-security-proxy"
                legacy_units=("security-proxy" "${LEGACY_SERVICE_PREFIX}-security-proxy" "${LEGACY_SERVICE_PREFIX}-proxy")
                ;;
            calciforge)
                service_name="calciforge"
                legacy_units=("${LEGACY_SERVICE_PREFIX}")
                ;;
        esac

        # ── get binary ───────────────────────────────────────────────────────
        local bin_path
        bin_path=$(build_for_arch "$arch" "$bin") || {
            if [[ "${CALCIFORGE_REMOTE_BUILD:-false}" != "true" ]]; then
                warn "  [$name] no local/cross binary for $bin on $arch; set CALCIFORGE_REMOTE_BUILD=true to compile on the node"
                return 1
            fi
            # Remote builds are opt-in. Small deployment VMs can become
            # unreachable under Rust build load, so unattended installs should
            # prefer cross/Docker-built artifacts copied from the operator host.
            warn "  [$name] cross-compile unavailable; attempting opt-in remote build..."
            ssh "${ssh_opts[@]}" "$ssh_target" bash -s -- "$bin" "$install_dir" <<'REMOTE_BUILD'
set -e
BIN=$1; INSTALL_DIR=$2
command -v cargo &>/dev/null || {
    curl -sf https://sh.rustup.rs | sh -s -- -y --quiet
    source "$HOME/.cargo/env"
}
TMP=$(mktemp -d)
# Expect the source to be pre-rsynced or pull from git
if [[ -d /opt/calciforge ]]; then
    cd /opt/calciforge && cargo build --release -p "$BIN" 2>&1 | tail -3
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
        ssh "${ssh_opts[@]}" "$ssh_target" "mkdir -p $install_dir" 2>/dev/null
        local remote_tmp="/tmp/calciforge-install-${bin}-$$"
        if command -v rsync >/dev/null 2>&1 && \
            ssh "${ssh_opts[@]}" "$ssh_target" "rsync --version >/dev/null 2>&1" 2>/dev/null; then
            rsync -az --checksum -e "$rsync_ssh" "$bin_path" "${ssh_target}:${remote_tmp}"
        else
            scp "${ssh_opts[@]}" "$bin_path" "${ssh_target}:${remote_tmp}"
        fi
        ssh "${ssh_opts[@]}" "$ssh_target" "install -m 0755 ${remote_tmp} ${install_dir}/${bin} && rm -f ${remote_tmp}"

        # ── rsync config files ────────────────────────────────────────────────
        if [[ "$bin" == "clashd" ]]; then
            local remote_policy_tmp="/tmp/calciforge-default-policy-$$.star"
            local remote_agents_tmp="/tmp/calciforge-agents-example-$$.json"
            ssh "${ssh_opts[@]}" "$ssh_target" bash -s -- "$config_dir" <<'REMOTE_MKDIR'
set -euo pipefail
mkdir -p -- "$1"
REMOTE_MKDIR
            if command -v rsync >/dev/null 2>&1 && \
                ssh "${ssh_opts[@]}" "$ssh_target" "rsync --version >/dev/null 2>&1" 2>/dev/null; then
                rsync -az -e "$rsync_ssh" "$CLASHD_DEFAULT_POLICY" "${ssh_target}:${remote_policy_tmp}"
                rsync -az -e "$rsync_ssh" "$CLASHD_DEFAULT_AGENTS" "${ssh_target}:${remote_agents_tmp}"
            else
                scp "${ssh_opts[@]}" "$CLASHD_DEFAULT_POLICY" "${ssh_target}:${remote_policy_tmp}"
                scp "${ssh_opts[@]}" "$CLASHD_DEFAULT_AGENTS" "${ssh_target}:${remote_agents_tmp}"
            fi
            ssh "${ssh_opts[@]}" "$ssh_target" bash -s -- "$config_dir" "$remote_policy_tmp" "$remote_agents_tmp" <<'REMOTE_CLASHD_CONFIG'
set -euo pipefail
config_dir="$1"
default_policy="$2"
default_agents="$3"
policy="${config_dir}/policy.star"
agents="${config_dir}/agents.json"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"

mkdir -p "$config_dir"

if [[ ! -f "$policy" ]]; then
    if [[ -f /etc/clashd/policy.star ]]; then
        cp /etc/clashd/policy.star "$policy"
        echo "migrated legacy clashd policy to $policy"
    else
        install -m 0644 "$default_policy" "$policy"
        echo "installed default clashd policy to $policy"
    fi
elif grep -q "clashd policy for Claude Code tool calls" "$policy" 2>/dev/null; then
    cp "$policy" "${policy}.claude-template.bak.${stamp}"
    if [[ -f /etc/clashd/policy.star ]] && ! grep -q "clashd policy for Claude Code tool calls" /etc/clashd/policy.star 2>/dev/null; then
        cp /etc/clashd/policy.star "$policy"
        echo "replaced Claude-specific policy with migrated legacy OpenClaw policy at $policy"
    else
        install -m 0644 "$default_policy" "$policy"
        echo "replaced Claude-specific policy with default shared clashd policy at $policy"
    fi
fi

agents_compact=""
if [[ -f "$agents" ]]; then
    agents_compact="$(tr -d '[:space:]' < "$agents")"
fi
if [[ ! -s "$agents" || "$agents_compact" == '{"agents":[]}' ]]; then
    [[ -f "$agents" ]] && cp "$agents" "${agents}.bak.${stamp}"
    if [[ -s /root/.clash/agents.json ]] && [[ "$(tr -d '[:space:]' < /root/.clash/agents.json)" != '{"agents":[]}' ]]; then
        cp /root/.clash/agents.json "$agents"
        echo "migrated legacy clashd agent config to $agents"
    else
        install -m 0644 "$default_agents" "$agents"
        echo "installed default clashd agent config to $agents"
    fi
fi

rm -f "$default_policy" "$default_agents"
REMOTE_CLASHD_CONFIG
        fi

        # ── install service ───────────────────────────────────────────────────
        local remote_log_dir
        local remote_mitm_ca_cert="${config_dir}/mitm-ca.pem"
        local remote_mitm_ca_key="${config_dir}/mitm-ca-key.pem"
        if [[ "$bin" == "security-proxy" ]] && truthy "$SECURITY_PROXY_MITM_ENABLED"; then
            ssh "${ssh_opts[@]}" "$ssh_target" 'bash -s' -- "$remote_mitm_ca_cert" "$remote_mitm_ca_key" <<'REMOTE_MITM_CA'
set -euo pipefail
cert="$1"
key="$2"
if [[ -f "$cert" && -f "$key" ]]; then
    chmod 600 "$key" 2>/dev/null || true
    exit 0
fi
if [[ -e "$cert" || -e "$key" ]]; then
    echo "incomplete MITM CA; expected both $cert and $key" >&2
    exit 20
fi
command -v openssl >/dev/null 2>&1 || {
    echo "openssl is required to generate Calciforge MITM CA" >&2
    exit 21
}
mkdir -p "$(dirname "$cert")" "$(dirname "$key")"
umask 077
openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes \
    -keyout "$key" \
    -out "$cert" \
    -subj "/CN=Calciforge Local MITM CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1
chmod 600 "$key"
chmod 644 "$cert"
REMOTE_MITM_CA
        fi
        if [[ "$os" == "linux" ]]; then
            remote_log_dir="/var/log/calciforge"
            local env_pairs unit_content exec_args
            case "$bin" in
                clashd)         env_pairs="CLASHD_PORT=${CLASHD_PORT}\nCLASHD_POLICY=${config_dir}/policy.star\nCLASHD_AGENTS=${config_dir}/agents.json" ;;
                security-proxy) env_pairs="SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}\nSECURITY_PROXY_BIND=${security_proxy_bind}\nSECURITY_PROXY_MITM_ENABLED=${SECURITY_PROXY_MITM_ENABLED}\nSECURITY_PROXY_CA_CERT=${remote_mitm_ca_cert}\nSECURITY_PROXY_CA_KEY=${remote_mitm_ca_key}\nCALCIFORGE_CONFIG_HOME=${config_dir}\nAGENT_CONFIG=${config_dir}/agents.json" ;;
                calciforge)     env_pairs="CALCIFORGE_CONFIG_HOME=${config_dir}\nCALCIFORGE_FNOX_DIR=${config_dir}\nFNOX_AGE_KEY_FILE=${config_dir}/secrets/fnox-age-ed25519" ;;
            esac
            exec_args=""
            if [[ "$bin" == "calciforge" ]]; then
                exec_args=" --config ${config_dir}/config.toml"
                ssh "${ssh_opts[@]}" "$ssh_target" \
                    "[[ -f ${config_dir}/config.toml ]] || echo 'warning: ${config_dir}/config.toml not found; ${service_name} may fail to start' >&2"
            fi
            unit_content=$(systemd_unit "$bin" "$install_dir" "$(printf '%b' "$env_pairs")" "$remote_service_path" "$exec_args" "$remote_wanted_by")
            ssh "${ssh_opts[@]}" "$ssh_target" "mkdir -p $remote_log_dir && cat > /etc/systemd/system/${service_name}.service" <<< "$unit_content"
            local disable_script="set -e; systemctl daemon-reload;"
            local legacy
            for legacy in "${legacy_units[@]}"; do
                [[ -n "$legacy" && "$legacy" != "$service_name" ]] || continue
                disable_script+=" systemctl disable --now '${legacy}.service' >/dev/null 2>&1 || true;"
            done
            # Mirror enable_restart_service(): remote upgrades also need an
            # explicit restart so already-running units load the new binary.
            disable_script+=" systemctl enable '${service_name}.service';"
            disable_script+=" systemctl restart '${service_name}.service'"
            ssh "${ssh_opts[@]}" "$ssh_target" "$disable_script" 2>&1 | tail -2
        else
            remote_log_dir="\$HOME/Library/Logs/calciforge"
            local plist_content label="com.calciforge.${service_name}"
            local launchd_env=("CLASHD_PORT=${CLASHD_PORT}" "SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}" "PATH=${remote_service_path}")
            if [[ "$bin" == "clashd" ]]; then
                launchd_env+=(
                    "CLASHD_POLICY=${config_dir}/policy.star"
                    "CLASHD_AGENTS=${config_dir}/agents.json"
                )
            fi
            if [[ "$bin" == "calciforge" ]]; then
                launchd_env+=(
                    "CALCIFORGE_CONFIG_HOME=${config_dir}"
                    "CALCIFORGE_FNOX_DIR=${config_dir}"
                    "FNOX_AGE_KEY_FILE=${config_dir}/secrets/fnox-age-ed25519"
                )
            fi
            if [[ "$bin" == "security-proxy" ]]; then
                launchd_env+=(
                    "SECURITY_PROXY_BIND=${security_proxy_bind}"
                    "SECURITY_PROXY_MITM_ENABLED=${SECURITY_PROXY_MITM_ENABLED}"
                    "SECURITY_PROXY_CA_CERT=${remote_mitm_ca_cert}"
                    "SECURITY_PROXY_CA_KEY=${remote_mitm_ca_key}"
                    "CALCIFORGE_CONFIG_HOME=${config_dir}"
                    "AGENT_CONFIG=${config_dir}/agents.json"
                )
            fi
            plist_content=$(launchd_plist "$bin" "$install_dir" "$remote_log_dir" "${launchd_env[@]}")
            local plist_path="\$HOME/Library/LaunchAgents/${label}.plist"
            ssh "${ssh_opts[@]}" "$ssh_target" "mkdir -p \$HOME/Library/LaunchAgents \$HOME/Library/Logs/calciforge"
            ssh "${ssh_opts[@]}" "$ssh_target" "cat > ${plist_path}" <<< "$plist_content"
            ssh "${ssh_opts[@]}" "$ssh_target" "launchctl unload ${plist_path} 2>/dev/null; launchctl load ${plist_path}"
        fi

        ok "  [$name] $bin deployed and started"
    }

    # ── iterate nodes from JSON ───────────────────────────────────────────────
    python3 - "$NODES_FILE" "$SECURITY_PROXY_BIND" <<'PYEOF' | while IFS='|' read -r name host user ssh_key arch os services install_dir config_dir security_proxy_bind; do
import json, sys
default_bind = sys.argv[2]
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
        n.get("config_dir", "/etc/calciforge"),
        n.get("security_proxy_bind", default_bind),
    ]))
PYEOF
        echo ""
        echo "  Node: $name ($user@$host, $arch, $os)"
        validate_security_proxy_bind "$security_proxy_bind" "security_proxy_bind for node $name"
        preflight_node "$name" "$host" "$user" "$ssh_key" "$os" "$services" "$install_dir" "$config_dir"
        ensure_remote_fnox "$name" "${user}@${host}" "$ssh_key" "$config_dir" || \
            warn "  [$name] fnox not ready — secret resolution may fail on that node"
        deploy_binary_only "$name" "$host" "$user" "$ssh_key" "$arch" \
            "calciforge-secrets" "$install_dir" || \
            warn "  [$name] calciforge-secrets not deployed — CLI secret discovery may fail on that node"
        IFS=',' read -ra svc_list <<< "$services"
        for svc in "${svc_list[@]}"; do
            deploy_service "$name" "$host" "$user" "$ssh_key" "$arch" "$os" \
                "$svc" "$install_dir" "$config_dir" "$security_proxy_bind"
        done
    done
fi

if declare -F run_calciforge_doctor >/dev/null; then
    run_calciforge_doctor "post-install"
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
agent_enabled dirac && (command -v dirac >/dev/null 2>&1 \
    && echo "  ✓ dirac" || echo "  ✗ dirac (run: npm install -g dirac-cli)")
echo ""
echo "Optional external-agent proxy:"
echo "  HTTP_PROXY=${SECURITY_PROXY_URL}"
if truthy "$SECURITY_PROXY_MITM_ENABLED"; then
    echo "  HTTPS_PROXY=${SECURITY_PROXY_URL}"
    echo "  Calciforge MITM CA=${SECURITY_PROXY_CA_CERT}"
fi
echo "  NO_PROXY=${SECURITY_PROXY_NO_PROXY}"
if [[ -n "$REMOTE_SCANNER_URL" ]]; then
    echo "  Remote scanner=${REMOTE_SCANNER_URL} (fail_closed=${REMOTE_SCANNER_FAIL_CLOSED})"
fi
echo ""
echo "Use this only for manually started external agent daemons or tested wrappers."
echo "Do not set proxy env on the Calciforge service itself."
echo "HTTPS inspection also requires the target runtime to trust the Calciforge"
echo "MITM CA; otherwise leave HTTPS_PROXY unset for that runtime."
echo ""
echo "Logs:"
echo "  clashd:         $LOG_DIR/"
echo "  security-proxy: $SEC_LOG_DIR/"
echo "  Policy:         $CLASHD_POLICY"
echo ""

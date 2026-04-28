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
#   --agents <list>      Comma-separated subset: claude,opencode,openclaw,zeroclaw,dirac
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
CLASHD_PORT="${CLASHD_PORT:-9001}"
SECURITY_PROXY_PORT="${SECURITY_PROXY_PORT:-8888}"
SECURITY_PROXY_URL="${SECURITY_PROXY_URL:-http://127.0.0.1:${SECURITY_PROXY_PORT}}"
SECURITY_PROXY_NO_PROXY="${SECURITY_PROXY_NO_PROXY:-localhost,127.0.0.1,::1}"
REMOTE_SCANNER_ENABLED="${CALCIFORGE_REMOTE_SCANNER_ENABLED:-${REMOTE_SCANNER_ENABLED:-0}}"
REMOTE_SCANNER_PORT="${REMOTE_SCANNER_PORT:-9801}"
REMOTE_SCANNER_URL=""
REMOTE_SCANNER_FAIL_CLOSED="${REMOTE_SCANNER_FAIL_CLOSED:-true}"
REMOTE_SCANNER_API_KEY_FILE="${REMOTE_SCANNER_API_KEY_FILE:-$HOME/.calciforge/secrets/remote-scanner-api-key}"
REMOTE_SCANNER_API_BASE="${REMOTE_SCANNER_API_BASE:-https://api.openai.com/v1}"
REMOTE_SCANNER_MODEL="${REMOTE_SCANNER_MODEL:-gpt-5.4-mini}"
REMOTE_SCANNER_PROMPT_FILE="${REMOTE_SCANNER_PROMPT_FILE:-$HOME/.calciforge/remote-llm-scanner-prompt.txt}"
LOG_MAX_BYTES="${CALCIFORGE_LOG_MAX_BYTES:-10485760}"
LOG_BACKUPS="${CALCIFORGE_LOG_BACKUPS:-5}"
ZC_CONFIG="${CALCIFORGE_CONFIG:-$HOME/.calciforge/config.toml}"
ZC_LOG_DIR="${ZC_LOG_DIR:-$HOME/.calciforge/logs}"
LEGACY_SERVICE_PREFIX="${CALCIFORGE_LEGACY_SERVICE_PREFIX:-}"
CLASHD_POLICY="${CLASHD_POLICY:-$CLASH_DIR/policy.star}"
AGENTS_JSON="$CLASH_DIR/agents.json"

case "$REMOTE_SCANNER_ENABLED" in
    1|true|TRUE|yes|YES|on|ON)
        REMOTE_SCANNER_URL="http://127.0.0.1:${REMOTE_SCANNER_PORT}"
        ;;
esac

proxy_env_pairs() {
    printf 'HTTP_PROXY=%s\nHTTPS_PROXY=%s\nhttp_proxy=%s\nhttps_proxy=%s\nNO_PROXY=%s\nno_proxy=%s\n' \
        "$SECURITY_PROXY_URL" "$SECURITY_PROXY_URL" \
        "$SECURITY_PROXY_URL" "$SECURITY_PROXY_URL" \
        "$SECURITY_PROXY_NO_PROXY" "$SECURITY_PROXY_NO_PROXY"
}

# ── platform detection ────────────────────────────────────────────────────────
# Drives choice of service manager (launchd vs systemd --user) and package
# installer fallbacks (brew vs apt/dnf). Scripts that don't have both paths
# tested will warn rather than fail.
PLATFORM="$(uname -s)"
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
    mkdir -p "$LOG_DIR" "$SEC_LOG_DIR" "$ZC_LOG_DIR"

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
NODES_FILE=""
AGENTS="claude,opencode,openclaw,zeroclaw,dirac"

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
SERVICE_PATH="$BIN_DIR:$HOME/.cargo/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin"

# ── colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
die()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }
hdr()  { echo -e "\n${CYAN}━━ $* ━━${NC}"; }

agent_enabled() { [[ ",$AGENTS," == *",$1,"* ]]; }

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
    local err_file
    err_file="$(mktemp)"
    if fnox list >/dev/null 2>"$err_file"; then
        rm -f "$err_file"
        ok "fnox config usable"
        return 0
    fi

    if grep -qi "No configuration file found" "$err_file"; then
        echo "  Initializing fnox global config..."
        if fnox init --global --skip-wizard >/dev/null 2>"$err_file"; then
            if fnox list >/dev/null 2>"$err_file"; then
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

# ── banner ────────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Calciforge — Unified Installer"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Agents:  $AGENTS"
echo "  Mode:    $([ "$CONFIGURE_ONLY" = true ] && echo configure-only || echo install+configure)"
echo "  Yes:     $YES"
echo ""

if [[ "$NODES_ONLY" != true ]]; then
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
        "$CARGO" build --release -p clashd -p security-proxy -p mcp-server -p paste-server
    run_build "calciforge with Matrix channel support" \
        "$CARGO" build --release -p calciforge --features channel-matrix

    mkdir -p "$BIN_DIR"
    for bin in clashd calciforge security-proxy mcp-server paste-server; do
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
disable_legacy_local_service "calciforge-clashd" "${LEGACY_SERVICE_PREFIX}-clashd"

if [[ ! -f "$CLASHD_POLICY" ]]; then
    cp "$REPO_ROOT/crates/clashd/config/claude-code-policy.star" "$CLASHD_POLICY"
    ok "Policy installed → $CLASHD_POLICY"
else
    ok "Policy already present → $CLASHD_POLICY"
fi

[[ -f "$AGENTS_JSON" ]] || \
    { cp "$REPO_ROOT/crates/clashd/config/agents.example.json" "$AGENTS_JSON" 2>/dev/null || \
      echo '{"agents":[]}' > "$AGENTS_JSON"; ok "Agent config → $AGENTS_JSON"; }

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
    $SYSTEMCTL enable --now calciforge-clashd.service 2>&1 | tail -3 || \
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
            $SYSTEMCTL enable --now calciforge-remote-llm-scanner.service 2>&1 | tail -3 || \
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
        <key>SECURITY_PROXY_REMOTE_SCANNER_URL</key><string>${REMOTE_SCANNER_URL}</string>
        <key>SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED</key><string>${REMOTE_SCANNER_FAIL_CLOSED}</string>
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
Environment=SECURITY_PROXY_REMOTE_SCANNER_URL=${REMOTE_SCANNER_URL}
Environment=SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED=${REMOTE_SCANNER_FAIL_CLOSED}
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
    $SYSTEMCTL enable --now calciforge-security-proxy.service 2>&1 | tail -3 || \
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
        HTTP_PROXY="$SECURITY_PROXY_URL" \
        HTTPS_PROXY="$SECURITY_PROXY_URL" \
        http_proxy="$SECURITY_PROXY_URL" \
        https_proxy="$SECURITY_PROXY_URL" \
        NO_PROXY="$SECURITY_PROXY_NO_PROXY" \
        no_proxy="$SECURITY_PROXY_NO_PROXY" \
            "$BIN_DIR/calciforge" --config "$ZC_CONFIG" doctor --no-network \
            || warn "calciforge doctor reported issues; see output above"
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

# ══════════════════════════════════════════════════════════════════════════════
# 6. calciforge — main agent gateway (channels + router + proxy)
# ══════════════════════════════════════════════════════════════════════════════
# Runs as a system service so channels (Telegram, Matrix, WhatsApp) reconnect
# across reboots. Expects config at ~/.calciforge/config.toml; users must
# populate it before the service starts (or the service will fail health and
# launchd/systemd will keep retrying).
hdr "calciforge"

mkdir -p "$ZC_LOG_DIR"
disable_legacy_local_service "calciforge" "${LEGACY_SERVICE_PREFIX}"

if [[ ! -f "$ZC_CONFIG" ]]; then
    warn "Config not found at $ZC_CONFIG — calciforge will fail to start until you create it"
    warn "See README for a minimal config.toml"
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
        <key>CALCIFORGE_REMOTE_SCANNER_URL</key><string>${REMOTE_SCANNER_URL}</string>
        <key>CALCIFORGE_REMOTE_SCANNER_FAIL_CLOSED</key><string>${REMOTE_SCANNER_FAIL_CLOSED}</string>
        <key>HTTP_PROXY</key><string>${SECURITY_PROXY_URL}</string>
        <key>HTTPS_PROXY</key><string>${SECURITY_PROXY_URL}</string>
        <key>http_proxy</key><string>${SECURITY_PROXY_URL}</string>
        <key>https_proxy</key><string>${SECURITY_PROXY_URL}</string>
        <key>NO_PROXY</key><string>${SECURITY_PROXY_NO_PROXY}</string>
        <key>no_proxy</key><string>${SECURITY_PROXY_NO_PROXY}</string>
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
Environment=CALCIFORGE_REMOTE_SCANNER_URL=${REMOTE_SCANNER_URL}
Environment=CALCIFORGE_REMOTE_SCANNER_FAIL_CLOSED=${REMOTE_SCANNER_FAIL_CLOSED}
Environment=HTTP_PROXY=${SECURITY_PROXY_URL}
Environment=HTTPS_PROXY=${SECURITY_PROXY_URL}
Environment=http_proxy=${SECURITY_PROXY_URL}
Environment=https_proxy=${SECURITY_PROXY_URL}
Environment=NO_PROXY=${SECURITY_PROXY_NO_PROXY}
Environment=no_proxy=${SECURITY_PROXY_NO_PROXY}
Environment=PATH=${SERVICE_PATH}
Restart=always
RestartSec=30
StandardOutput=append:${ZC_LOG_DIR}/calciforge.log
StandardError=append:${ZC_LOG_DIR}/calciforge.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
    $SYSTEMCTL daemon-reload
    $SYSTEMCTL enable --now calciforge.service 2>&1 | tail -3 || \
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

# ══════════════════════════════════════════════════════════════════════════════
# 6. Claude Code hook
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
# 7. opencode
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
# 8. openclaw
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
# 9. dirac
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
# 10. zeroclaw
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

fi # !NODES_ONLY

# ══════════════════════════════════════════════════════════════════════════════
# 11. Multi-node SSH deployment
# ══════════════════════════════════════════════════════════════════════════════

if [[ -n "$NODES_FILE" ]]; then
    hdr "Multi-node deployment"

    [[ -f "$NODES_FILE" ]] || die "Nodes file not found: $NODES_FILE"
    command -v python3 &>/dev/null || die "python3 required for node deployment"

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
        local cargo_args=(build --release -p "$bin" --target "$target")
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
                "export PATH=/usr/local/cargo/bin:\$PATH CARGO_TARGET_DIR='$docker_target_dir' && apt-get update -qq >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libssl-dev libudev-dev cmake protobuf-compiler clang >/dev/null && cargo build --release -p '$bin' --target '$docker_target' $([[ "$bin" == "calciforge" ]] && printf '%s' '--features channel-matrix') && chown -R '$host_uid:$host_gid' '$docker_target_dir'"
            out_path="$REPO_ROOT/${docker_target_dir}/${target}/release/${bin}"
        else
            warn "No cross-compilation tool found (install 'cross' or 'cargo-zigbuild')" >&2
            echo ""; return 1
        fi

        [[ -f "$out_path" ]] && built_cache_put "$cache_key" "$out_path" && echo "$out_path" || \
            { warn "Build failed for $target/$bin"; echo ""; return 1; }
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
        local name="$1" ssh_target="$2" ssh_key="$3"
        local ssh_opts=(-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
        [[ -n "$ssh_key" ]] && ssh_opts+=(-i "$ssh_key")

        echo "  [$name] checking fnox..."
        ssh "${ssh_opts[@]}" "$ssh_target" 'bash -s' <<'REMOTE_FNOX'
set -euo pipefail
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
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
if ! fnox list >/dev/null 2>&1; then
    fnox init --global --skip-wizard >/dev/null
fi
fnox list >/dev/null
REMOTE_FNOX
        ok "  [$name] fnox ready"
    }

    # ── deploy one service to one node ───────────────────────────────────────
    deploy_service() {
        local name="$1" host="$2" user="$3" ssh_key="$4" arch="$5" os="$6"
        local bin="$7" install_dir="$8" config_dir="$9"

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
        if ssh "${ssh_opts[@]}" "$ssh_target" "command -v rsync >/dev/null 2>&1" 2>/dev/null; then
            rsync -az --checksum -e "$rsync_ssh" "$bin_path" "${ssh_target}:${remote_tmp}"
        else
            scp "${ssh_opts[@]}" "$bin_path" "${ssh_target}:${remote_tmp}"
        fi
        ssh "${ssh_opts[@]}" "$ssh_target" "install -m 0755 ${remote_tmp} ${install_dir}/${bin} && rm -f ${remote_tmp}"

        # ── rsync config files ────────────────────────────────────────────────
        if [[ "$bin" == "clashd" ]]; then
            ssh "${ssh_opts[@]}" "$ssh_target" "mkdir -p $config_dir"
            rsync -az -e "$rsync_ssh" \
                "$REPO_ROOT/crates/clashd/config/claude-code-policy.star" \
                "${ssh_target}:${config_dir}/policy.star" 2>/dev/null || true
            # Write minimal agents.json if absent
            ssh "${ssh_opts[@]}" "$ssh_target" \
                "[[ -f ${config_dir}/agents.json ]] || echo '{\"agents\":[]}' > ${config_dir}/agents.json"
        fi

        # ── install service ───────────────────────────────────────────────────
        local remote_log_dir
        if [[ "$os" == "linux" ]]; then
            remote_log_dir="/var/log/calciforge"
            local env_pairs unit_content exec_args
            case "$bin" in
                clashd)         env_pairs="CLASHD_PORT=${CLASHD_PORT}\nCLASHD_POLICY=${config_dir}/policy.star\nCLASHD_AGENTS=${config_dir}/agents.json" ;;
                security-proxy) env_pairs="SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}\nAGENT_CONFIG=${config_dir}/agents.json" ;;
                calciforge)     env_pairs="$(proxy_env_pairs)" ;;
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
            disable_script+=" systemctl enable --now '${service_name}.service'"
            ssh "${ssh_opts[@]}" "$ssh_target" "$disable_script" 2>&1 | tail -2
        else
            remote_log_dir="\$HOME/Library/Logs/calciforge"
            local plist_content label="com.calciforge.${service_name}"
            local launchd_env=("CLASHD_PORT=${CLASHD_PORT}" "SECURITY_PROXY_PORT=${SECURITY_PROXY_PORT}" "PATH=${remote_service_path}")
            if [[ "$bin" == "calciforge" ]]; then
                launchd_env+=(
                    "HTTP_PROXY=${SECURITY_PROXY_URL}"
                    "HTTPS_PROXY=${SECURITY_PROXY_URL}"
                    "http_proxy=${SECURITY_PROXY_URL}"
                    "https_proxy=${SECURITY_PROXY_URL}"
                    "NO_PROXY=${SECURITY_PROXY_NO_PROXY}"
                    "no_proxy=${SECURITY_PROXY_NO_PROXY}"
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
        n.get("config_dir", "/etc/calciforge"),
    ]))
PYEOF
        echo ""
        echo "  Node: $name ($user@$host, $arch, $os)"
        ensure_remote_fnox "$name" "${user}@${host}" "$ssh_key" || \
            warn "  [$name] fnox not ready — secret resolution may fail on that node"
        IFS=',' read -ra svc_list <<< "$services"
        for svc in "${svc_list[@]}"; do
            deploy_service "$name" "$host" "$user" "$ssh_key" "$arch" "$os" \
                "$svc" "$install_dir" "$config_dir" || true
        done
    done
fi

run_calciforge_doctor "post-install"

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
echo "Calciforge service proxy:"
echo "  HTTP_PROXY=${SECURITY_PROXY_URL}"
echo "  HTTPS_PROXY=${SECURITY_PROXY_URL}"
echo "  NO_PROXY=${SECURITY_PROXY_NO_PROXY}"
if [[ -n "$REMOTE_SCANNER_URL" ]]; then
    echo "  Remote scanner=${REMOTE_SCANNER_URL} (fail_closed=${REMOTE_SCANNER_FAIL_CLOSED})"
fi
echo ""
echo "For manually started external agent daemons, set the same proxy environment"
echo "before launch. Installer-managed Calciforge subprocess agents inherit it."
echo ""
echo "Logs:"
echo "  clashd:         $LOG_DIR/"
echo "  security-proxy: $SEC_LOG_DIR/"
echo "  Policy:         $CLASHD_POLICY"
echo ""

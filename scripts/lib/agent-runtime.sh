#!/usr/bin/env bash
# scripts/lib/agent-runtime.sh — Shared helpers for installing and managing
# first-class agent runtimes. Sourced by install.sh and usable standalone.
#
# Provides cross-platform (macOS/Linux) functions for:
#   - Downloading binaries from GitHub releases
#   - Installing via brew
#   - Managing services (launchd on Mac, systemd on Linux)
#   - Registering agents in calciforge config.toml
#
# Required globals (set by caller before sourcing):
#   PLATFORM       — "Darwin" or "Linux"
#   IS_ROOT        — true/false
#   BIN_DIR        — where symlinks go ($HOME/.local/bin or /usr/local/bin)
#   PLIST_DIR      — launchd plist dir or systemd unit dir
#   LOG_DIR        — log directory
#   SYSTEMCTL      — "systemctl" or "systemctl --user" or ""
#   WANTED_BY_TARGET — "multi-user.target" or "default.target"
#   ZC_CONFIG      — path to calciforge config.toml
#
# Optional globals:
#   YES            — "true" for non-interactive mode

# Guard against double-sourcing
[[ -n "${_AGENT_RUNTIME_LOADED:-}" ]] && return 0
_AGENT_RUNTIME_LOADED=1

# ── Binary installation ───────────────────────────────────────────────────────

# Download and install a binary from a GitHub release tarball.
# The release must follow the naming convention: <binary>-<triple>.tar.gz
#
# Args: <github_repo> <binary_name> <install_dir>
# Example: install_from_github_release "nearai/ironclaw" "ironclaw" "/opt/ironclaw"
install_from_github_release() {
    local repo="$1" bin_name="$2" install_dir="$3"

    mkdir -p "$install_dir/bin"

    local arch triple
    arch="$(uname -m)"
    case "${PLATFORM}:${arch}" in
        Linux:x86_64)                triple="x86_64-unknown-linux-gnu" ;;
        Linux:aarch64)               triple="aarch64-unknown-linux-gnu" ;;
        Darwin:x86_64)               triple="x86_64-apple-darwin" ;;
        Darwin:arm64|Darwin:aarch64) triple="aarch64-apple-darwin" ;;
        *) echo "Unsupported platform:arch ${PLATFORM}:${arch}" >&2; return 1 ;;
    esac

    local release_url="https://github.com/${repo}/releases/latest/download/${bin_name}-${triple}.tar.gz"
    echo "  Downloading ${bin_name} from ${release_url}..."
    local tmp
    tmp="$(mktemp -d)"

    if ! curl -fsSL "$release_url" -o "$tmp/${bin_name}.tar.gz"; then
        rm -rf "$tmp"
        echo "  Download failed" >&2
        return 1
    fi

    tar xzf "$tmp/${bin_name}.tar.gz" -C "$tmp"

    local binary
    binary="$(find "$tmp" -name "$bin_name" -type f \( -perm -u+x -o -perm -g+x -o -perm -o+x \) | head -1)"
    if [[ -z "$binary" ]]; then
        echo "  Could not find executable '$bin_name' in release archive" >&2
        rm -rf "$tmp"
        return 1
    fi

    # install(1) handles "Text file busy" by unlinking first
    install -m 755 "$binary" "$install_dir/bin/$bin_name" 2>/dev/null || {
        rm -f "$install_dir/bin/$bin_name"
        cp "$binary" "$install_dir/bin/$bin_name"
        chmod +x "$install_dir/bin/$bin_name"
    }
    rm -rf "$tmp"

    # macOS: remove quarantine attribute to prevent Gatekeeper popup
    if [[ "$PLATFORM" == "Darwin" ]]; then
        xattr -d com.apple.quarantine "$install_dir/bin/$bin_name" 2>/dev/null || true
    fi

    # Symlink into PATH
    if [[ -w "$BIN_DIR" ]]; then
        ln -sf "$install_dir/bin/$bin_name" "$BIN_DIR/$bin_name"
    fi
}

# Install a binary, trying brew first on macOS, falling back to GitHub release.
# Args: <brew_formula> <binary_name> <github_repo> <install_dir>
ensure_agent_binary() {
    local brew_formula="$1" bin_name="$2" github_repo="$3" install_dir="$4"

    if command -v "$bin_name" &>/dev/null; then
        ok "$bin_name $("$bin_name" --version 2>&1 | head -1 || echo '(installed)')"
        return 0
    fi

    case "$PLATFORM" in
        Darwin)
            if command -v brew &>/dev/null; then
                if brew install "$brew_formula" 2>/dev/null; then
                    ok "$bin_name installed via brew"
                    return 0
                fi
                warn "brew install $brew_formula failed — trying GitHub release"
            fi
            install_from_github_release "$github_repo" "$bin_name" "$install_dir"
            ;;
        Linux)
            install_from_github_release "$github_repo" "$bin_name" "$install_dir"
            ;;
    esac
}

# Build and install a binary from the local Cargo workspace.
# Args: <crate_name> <binary_name> <install_dir> [cargo_bin_name]
# cargo_bin_name defaults to crate_name if not specified.
install_from_workspace() {
    local crate="$1" bin_name="$2" install_dir="$3" cargo_bin="${4:-$1}"
    local repo_root
    repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

    echo "  Building $crate from workspace..."
    local cargo="${CARGO:-cargo}"
    "$cargo" build --release -p "$crate" 2>&1 | tail -5

    local src="$repo_root/target/release/$cargo_bin"
    if [[ ! -f "$src" ]]; then
        echo "  Binary not found after build: $src" >&2
        return 1
    fi

    mkdir -p "$install_dir/bin"
    install -m 755 "$src" "$install_dir/bin/$bin_name" 2>/dev/null || {
        rm -f "$install_dir/bin/$bin_name"
        cp "$src" "$install_dir/bin/$bin_name"
        chmod +x "$install_dir/bin/$bin_name"
    }

    if [[ -w "$BIN_DIR" ]]; then
        ln -sf "$install_dir/bin/$bin_name" "$BIN_DIR/$bin_name"
    fi
}

# ── Service management ────────────────────────────────────────────────────────

# Install and start a service. Cross-platform: launchd on macOS, systemd on Linux.
#
# Args: <service_name> <exec_path> <work_dir> [env_file] [description] [extra_args]
# The service_name becomes:
#   macOS:  com.calciforge.<service_name> LaunchAgent
#   Linux:  calciforge-<service_name>.service systemd unit
ensure_agent_service() {
    local name="$1" exec_path="$2" work_dir="$3"
    local env_file="${4:-}" description="${5:-$1}" extra_args="${6:-}"

    case "$PLATFORM" in
        Darwin)
            local label="com.calciforge.${name}"
            local plist_path="${PLIST_DIR}/${label}.plist"
            local agent_log_dir="$HOME/Library/Logs/${name}"
            mkdir -p "$agent_log_dir"

            # Build the plist
            cat > "$plist_path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>${label}</string>
    <key>ProgramArguments</key><array><string>${exec_path}</string>$(for arg in $extra_args; do printf "<string>%s</string>" "$arg"; done)</array>
    <key>WorkingDirectory</key><string>${work_dir}</string>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>${agent_log_dir}/${name}.log</string>
    <key>StandardErrorPath</key><string>${agent_log_dir}/${name}.err</string>
</dict></plist>
EOF

            # If there's an env file, inject EnvironmentVariables into the plist
            if [[ -n "$env_file" && -f "$env_file" ]]; then
                local env_dict="<key>EnvironmentVariables</key><dict>"
                while IFS='=' read -r k v; do
                    [[ -z "$k" || "$k" == \#* ]] && continue
                    # XML-escape special characters in values
                    v="${v//&/&amp;}"
                    v="${v//</&lt;}"
                    v="${v//>/&gt;}"
                    v="${v//\"/&quot;}"
                    env_dict+="<key>${k}</key><string>${v}</string>"
                done < "$env_file"
                env_dict+="</dict>"
                # Insert before </dict></plist>
                sed -i '' "s|</dict></plist>|    ${env_dict}\n</dict></plist>|" "$plist_path" 2>/dev/null || \
                    sed -i "s|</dict></plist>|    ${env_dict}\n</dict></plist>|" "$plist_path"
            fi

            load_launch_agent "$label" "$plist_path"
            ;;
        Linux)
            local unit_name="calciforge-${name}"
            local unit_path="${PLIST_DIR}/${unit_name}.service"

            cat > "$unit_path" <<EOF
[Unit]
Description=${description} (managed by Calciforge)
After=network.target

[Service]
Type=simple
ExecStart=${exec_path}${extra_args:+ ${extra_args}}
WorkingDirectory=${work_dir}
${env_file:+EnvironmentFile=${env_file}}
Restart=always
RestartSec=5
StandardOutput=append:${LOG_DIR}/${name}.log
StandardError=append:${LOG_DIR}/${name}.err

[Install]
WantedBy=${WANTED_BY_TARGET}
EOF
            $SYSTEMCTL daemon-reload
            enable_restart_service "${unit_name}.service"
            ;;
    esac
}

# Stop and disable a service. Cross-platform.
# Args: <service_name>
stop_agent_service() {
    local name="$1"
    case "$PLATFORM" in
        Darwin)
            local label="com.calciforge.${name}"
            local plist_path="${PLIST_DIR}/${label}.plist"
            launchctl bootout "gui/$(id -u)" "$plist_path" 2>/dev/null || \
                launchctl unload "$plist_path" 2>/dev/null || true
            ;;
        Linux)
            local unit_name="calciforge-${name}"
            $SYSTEMCTL stop "${unit_name}.service" 2>/dev/null || true
            $SYSTEMCTL disable "${unit_name}.service" 2>/dev/null || true
            ;;
    esac
}

# Check if a service is running. Returns 0 if active.
# Args: <service_name>
agent_service_is_running() {
    local name="$1"
    case "$PLATFORM" in
        Darwin)
            launchctl print "gui/$(id -u)/com.calciforge.${name}" >/dev/null 2>&1
            ;;
        Linux)
            $SYSTEMCTL is-active "calciforge-${name}.service" >/dev/null 2>&1
            ;;
    esac
}

# ── Config registration ───────────────────────────────────────────────────────

# Add an agent entry to calciforge config.toml if one with the same kind doesn't
# already exist. Idempotent.
#
# Args: <id> <kind> <endpoint> [timeout_ms] [aliases_csv] [api_key_file]
# Example: ensure_calciforge_agent_config "ironclaw" "ironclaw" "http://127.0.0.1:3000" 300000 "iron" "/opt/ironclaw/webhook-secret"
ensure_calciforge_agent_config() {
    local agent_id="$1" kind="$2" endpoint="$3"
    local timeout="${4:-300000}" aliases_csv="${5:-}" api_key_file="${6:-}"

    local config_path="${ZC_CONFIG:-}"
    [[ -z "$config_path" ]] && return 0

    mkdir -p "$(dirname "$config_path")"

    python3 - "$config_path" "$agent_id" "$kind" "$endpoint" "$timeout" "$aliases_csv" "$api_key_file" <<'PYEOF'
import json
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1]).expanduser()
agent_id, kind, endpoint, timeout, aliases_csv, api_key_file = sys.argv[2:8]
timeout_ms = int(timeout)
aliases = [a.strip() for a in aliases_csv.split(",") if a.strip()] if aliases_csv else []

if not path.exists() or not path.read_text().strip():
    path.write_text("[calciforge]\nversion = 2\n")

text = path.read_text()

# Check if an agent with this kind already exists
agent_blocks = re.split(r"(?m)^\[\[agents\]\]\s*$", text)[1:]
for block in agent_blocks:
    next_table = re.split(r"(?m)^\[", block, maxsplit=1)[0]
    if re.search(rf'(?m)^\s*kind\s*=\s*["\']' + re.escape(kind) + r'["\']', next_table):
        print(f"{kind} agent already present in {path}")
        raise SystemExit(0)

def q(value):
    return json.dumps(value)

aliases_line = ""
if aliases:
    aliases_line = f'\naliases = {json.dumps(aliases)}'

api_key_file_line = ""
if api_key_file:
    api_key_file_line = f'\napi_key_file = {q(api_key_file)}'

block = f"""

# {kind} agent (managed by calciforge install)
[[agents]]
id = {q(agent_id)}
kind = {q(kind)}
endpoint = {q(endpoint)}
timeout_ms = {timeout_ms}{aliases_line}{api_key_file_line}
"""

with path.open("a", encoding="utf-8") as fh:
    fh.write(block)
print(f"added {kind} agent ({agent_id!r}) to {path}")
PYEOF
}

# ── Environment file management ──────────────────────────────────────────────

# Write an agent .env file (always overwrites to ensure config is current).
# Args: <path> <key=value pairs as heredoc on stdin>
ensure_agent_env() {
    local env_path="$1"
    mkdir -p "$(dirname "$env_path")"
    cat > "$env_path"
    chmod 600 "$env_path"
}

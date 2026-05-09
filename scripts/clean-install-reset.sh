#!/usr/bin/env bash
# Prepare a machine for a clean Calciforge install test.
#
# This script is intentionally dry-run by default. Use --execute only after the
# plan looks right. Config, Docker state, and managed agent runtimes are opt-in
# so a service reset cannot silently delete operator data.

set -euo pipefail

EXECUTE=false
INCLUDE_CONFIG=false
INCLUDE_DOCKER=false
INCLUDE_AGENTS=false
INCLUDE_FNOX=false
SSH_TARGET=""

usage() {
    cat <<'EOF'
Usage: scripts/clean-install-reset.sh [options]

Options:
  --execute            Run the reset commands. Default is dry-run.
  --include-config     Also remove Calciforge config/state directories.
  --include-docker     Also remove known Calciforge Docker containers/images.
  --include-agents     Also stop/remove Calciforge-managed agent services.
  --include-fnox       Also remove fnox config/state. Use only when fnox is
                       dedicated to this Calciforge install.
  --ssh HOST           Run the same reset on HOST over SSH.
  -h, --help           Show this help.

Examples:
  scripts/clean-install-reset.sh
  scripts/clean-install-reset.sh --include-docker --include-config
  scripts/clean-install-reset.sh --ssh root@calciforge-staging.example --include-docker
  scripts/clean-install-reset.sh --execute --include-config --include-docker

The dry-run output is the contract: inspect it before adding --execute.
EOF
}

remote_args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --execute)
            EXECUTE=true
            remote_args+=("$1")
            ;;
        --include-config)
            INCLUDE_CONFIG=true
            remote_args+=("$1")
            ;;
        --include-docker)
            INCLUDE_DOCKER=true
            remote_args+=("$1")
            ;;
        --include-agents)
            INCLUDE_AGENTS=true
            remote_args+=("$1")
            ;;
        --include-fnox)
            INCLUDE_FNOX=true
            remote_args+=("$1")
            ;;
        --ssh)
            [[ $# -ge 2 ]] || {
                echo "missing value for --ssh" >&2
                exit 2
            }
            SSH_TARGET="$2"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if [[ -n "$SSH_TARGET" ]]; then
    ssh "$SSH_TARGET" bash -s -- "${remote_args[@]}" <"$0"
    exit
fi

quote() {
    printf '%q' "$1"
}

run() {
    if "$EXECUTE"; then
        echo "+ $*"
        "$@"
    else
        printf 'dry-run:'
        local arg
        for arg in "$@"; do
            printf ' %s' "$(quote "$arg")"
        done
        printf '\n'
    fi
}

run_shell() {
    if "$EXECUTE"; then
        echo "+ $*"
        bash -lc "$*"
    else
        echo "dry-run: bash -lc $(quote "$*")"
    fi
}

exists_cmd() {
    command -v "$1" >/dev/null 2>&1
}

stop_launchd_label() {
    local label="$1"
    local plist="$HOME/Library/LaunchAgents/$label.plist"
    run launchctl bootout "gui/$(id -u)" "$plist" || true
    run rm -f "$plist"
}

stop_systemd_unit() {
    local unit="$1"
    run systemctl disable --now "$unit" || true
    run rm -f "/etc/systemd/system/$unit"
}

platform="$(uname -s)"
echo "Calciforge clean-install reset on $(hostname) ($platform)"
if "$EXECUTE"; then
    echo "Mode: execute"
else
    echo "Mode: dry-run"
fi

if [[ "$platform" == "Darwin" ]]; then
    for label in \
        com.calciforge.calciforge \
        com.calciforge.security-proxy \
        com.calciforge.clashd \
        com.calciforge.helicone-ai-gateway \
        com.calciforge.log-rotate; do
        [[ -e "$HOME/Library/LaunchAgents/$label.plist" ]] && stop_launchd_label "$label"
    done

    if "$INCLUDE_AGENTS"; then
        for label in \
            ai.openclaw.gateway \
            com.calciforge.hermes \
            com.calciforge.ironclaw; do
            [[ -e "$HOME/Library/LaunchAgents/$label.plist" ]] && stop_launchd_label "$label"
        done
    fi
elif [[ "$platform" == "Linux" ]]; then
    if "$EXECUTE" && [[ "$(id -u)" != "0" ]]; then
        echo "error: --execute on Linux must run as root because reset touches systemd, /etc/calciforge, and /opt/calciforge." >&2
        echo "       Re-run with sudo, SSH as root, or omit --execute to inspect the dry-run plan." >&2
        exit 1
    fi

    for unit in \
        calciforge.service \
        calciforge-security-proxy.service \
        calciforge-clashd.service \
        calciforge-helicone-ai-gateway.service \
        calciforge-log-rotate.service; do
        systemctl list-unit-files "$unit" >/dev/null 2>&1 && stop_systemd_unit "$unit"
    done

    if "$INCLUDE_AGENTS"; then
        for unit in \
            calciforge-hermes.service \
            calciforge-ironclaw.service \
            openclaw-gateway.service; do
            systemctl list-unit-files "$unit" >/dev/null 2>&1 && stop_systemd_unit "$unit"
        done
    fi

    run systemctl daemon-reload || true
else
    echo "warning: unsupported platform $platform; only generic cleanup will run" >&2
fi

for path in \
    "$HOME/.local/bin/calciforge" \
    "$HOME/.local/bin/security-proxy" \
    "$HOME/.local/bin/clashd"; do
    [[ -e "$path" ]] && run rm -f "$path"
done

if "$INCLUDE_CONFIG"; then
    for path in \
        "$HOME/.config/calciforge" \
        "$HOME/.clash" \
        /etc/calciforge \
        /opt/calciforge; do
        [[ -e "$path" ]] && run rm -rf "$path"
    done
fi

if "$INCLUDE_FNOX"; then
    for path in "$HOME/.config/fnox"; do
        [[ -e "$path" ]] && run rm -rf "$path"
    done
fi

if "$INCLUDE_DOCKER"; then
    if exists_cmd docker; then
        run_shell 'docker ps -a --format "{{.ID}} {{.Names}} {{.Image}}" | awk '\''/calciforge|helicone|synapse|element-web/ { print $1 }'\'' | while read -r id; do [ -n "$id" ] && docker rm -f "$id"; done'
        run_shell 'docker images --format "{{.Repository}}:{{.Tag}}" | awk '\''/calciforge|helicone/ { print }'\'' | while read -r image; do [ -n "$image" ] && docker rmi "$image"; done'
    else
        echo "docker not found; skipping Docker cleanup" >&2
    fi
fi

echo "Reset plan complete."

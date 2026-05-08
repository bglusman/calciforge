#!/usr/bin/env bash
# scripts/lib/fnox.sh — fnox install and provider bootstrap helpers.
#
# Ownership:
#   This module owns local fnox CLI installation, global fnox config bootstrap,
#   provider creation, and the optional preflight write/remove warmup. Remote
#   node bootstrap is still embedded in install.sh until remote deployment gets
#   its own module.
#
# Required globals:
#   CALCIFORGE_CONFIG_HOME      — Calciforge config root.
#   CALCIFORGE_FNOX_DIR        — Working directory used for fnox commands.
#   CALCIFORGE_FNOX_PROVIDER_NAME
#   CALCIFORGE_FNOX_PROVIDER_TYPE
#   CALCIFORGE_FNOX_WARMUP
#   CALCIFORGE_FNOX_AGE_RECIPIENT
#   CONFIGURE_ONLY             — true/false installer mode.
#   FNOX_AGE_KEY_FILE          — age key path, may be empty on macOS.
#   IS_ROOT                    — true/false.
#   PLATFORM                   — "Darwin" or "Linux".
#
# Required functions from common.sh / install.sh:
#   ask_install, die, ok, warn, truthy, toml_basic_string.
#
# Optional globals:
#   FNOX_CONFIG_DIR, FNOX_VERSION, HOME, XDG_CONFIG_HOME.

[[ -n "${_CALCIFORGE_FNOX_LIB_LOADED:-}" ]] && return 0
_CALCIFORGE_FNOX_LIB_LOADED=1

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

# fnox — secret resolver (brew on macOS, release tarball on Linux, cargo last).
# Prefer prebuilt release tarballs on Linux because compiling fnox can overwhelm
# small deployment VMs.
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

warm_fnox_provider() {
    truthy "$CALCIFORGE_FNOX_WARMUP" || return 0

    local key value err_file
    key="CALCIFORGE_INSTALL_PRECHECK"
    value="calciforge-install-preflight-$(date +%s)-$$"
    err_file="$(mktemp)"

    echo "  Warming fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' with a temporary secret..."
    if ! (cd "$CALCIFORGE_FNOX_DIR" && printf '%s' "$value" | fnox set "$key" >/dev/null 2>"$err_file"); then
        warn "fnox provider warmup failed; first secret write may still ask for local approval"
        sed 's/^/  fnox: /' "$err_file" | tail -5
        rm -f "$err_file"
        return 0
    fi

    if ! (cd "$CALCIFORGE_FNOX_DIR" && fnox remove "$key" >/dev/null 2>"$err_file"); then
        warn "fnox provider warmup stored temporary secret '$key' but could not remove it; remove it manually with: fnox remove $key"
        sed 's/^/  fnox: /' "$err_file" | tail -5
        rm -f "$err_file"
        return 0
    fi

    rm -f "$err_file"
    ok "fnox provider write path warmed"
}

ensure_fnox_provider() {
    local count provider_type err_file
    count="$(fnox_provider_count)"
    if [[ "$count" -gt 0 ]]; then
        ok "fnox provider configured"
        warm_fnox_provider
        return 0
    fi

    provider_type="$(default_fnox_provider_type)"
    if [[ -z "$provider_type" ]]; then
        warn "fnox has no provider configured; run 'fnox provider add <name> <type> --global' or set CALCIFORGE_FNOX_PROVIDER_TYPE before install"
        return 1
    fi

    if [[ "$provider_type" == "age" ]]; then
        if ensure_fnox_age_provider; then
            warm_fnox_provider
            return 0
        fi
        return 1
    fi

    err_file="$(mktemp)"
    echo "  Adding fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' (${provider_type})..."
    if fnox provider add "$CALCIFORGE_FNOX_PROVIDER_NAME" "$provider_type" --global >/dev/null 2>"$err_file"; then
        if fnox provider test "$CALCIFORGE_FNOX_PROVIDER_NAME" >/dev/null 2>"$err_file"; then
            rm -f "$err_file"
            ok "fnox provider '${CALCIFORGE_FNOX_PROVIDER_NAME}' ready"
            warm_fnox_provider
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

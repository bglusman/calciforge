#!/usr/bin/env bash
# cleanup-210-polyclaw.sh
# One-shot cleanup of stale polyclaw artifacts on 192.168.1.210 after zeroclawed rename.
#
# Safety: Verifies zeroclawed.service is running and /etc/zeroclawed/config.toml exists
#         before touching anything. Idempotent — safe to run twice.
# Does NOT touch: polyclaw-whatsapp.service (separate Node.js sidecar)
#                 nonzeroclaw or zeroclawed binaries (current)

set -euo pipefail

# ── Config ──────────────────────────────────────────────────────────────────
REMOTE_HOST="root@192.168.1.210"
SSH_KEY="${HOME}/.ssh/id_ed25519"
SSH_OPTS="-i ${SSH_KEY} -o StrictHostKeyChecking=no -o ConnectTimeout=10"

# ── Helpers ──────────────────────────────────────────────────────────────────
log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"; }
die() { log "ERROR: $*" >&2; exit 1; }

log "=== cleanup-210-polyclaw.sh start ==="
log "Target: ${REMOTE_HOST}"

# ── Verify SSH key exists locally ────────────────────────────────────────────
[[ -f "${SSH_KEY}" ]] || die "SSH key not found: ${SSH_KEY}"

# ── Run everything remotely in a single SSH session ──────────────────────────
# shellcheck disable=SC2087
ssh ${SSH_OPTS} "${REMOTE_HOST}" bash -s <<'REMOTE'
set -euo pipefail

log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] [remote] $*"; }
die() { log "ERROR: $*" >&2; exit 1; }

ARCHIVE_DIR="/root/polyclaw-archived"
TIMESTAMP="$(date '+%Y%m%d-%H%M%S')"
POLYCLAW_SERVICE="/etc/systemd/system/polyclaw.service"
POLYCLAW_CONFIG="/etc/polyclaw"
ZEROCLAWED_CONFIG="/etc/zeroclawed/config.toml"

log "=== Remote cleanup starting on $(hostname) ==="

# ─────────────────────────────────────────────────────────────────────────────
# SAFETY CHECKS — abort before touching anything if conditions aren't met
# ─────────────────────────────────────────────────────────────────────────────

log "Safety check 1: zeroclawed.service must be active..."
if ! systemctl is-active --quiet zeroclawed.service; then
    die "zeroclawed.service is NOT running. Aborting cleanup to avoid orphaning the host."
fi
log "  ✓ zeroclawed.service is active"

log "Safety check 2: /etc/zeroclawed/config.toml must exist..."
if [[ ! -f "${ZEROCLAWED_CONFIG}" ]]; then
    die "${ZEROCLAWED_CONFIG} not found. Config may not have been migrated. Aborting."
fi
log "  ✓ ${ZEROCLAWED_CONFIG} exists"

log "Safety check 3: polyclaw-whatsapp.service will NOT be touched..."
if systemctl list-unit-files polyclaw-whatsapp.service &>/dev/null; then
    log "  ✓ polyclaw-whatsapp.service detected — will be left alone"
fi

# ─────────────────────────────────────────────────────────────────────────────
# STEP 1: Disable polyclaw.service
# ─────────────────────────────────────────────────────────────────────────────
log "Step 1: Stop and disable polyclaw.service..."

if systemctl is-enabled --quiet polyclaw.service 2>/dev/null; then
    systemctl disable polyclaw.service
    log "  ✓ polyclaw.service disabled"
else
    log "  (polyclaw.service already disabled — skipping)"
fi

# Stop it if somehow still running (shouldn't be, but be safe)
if systemctl is-active --quiet polyclaw.service 2>/dev/null; then
    systemctl stop polyclaw.service
    log "  ✓ polyclaw.service stopped"
else
    log "  (polyclaw.service already stopped — skipping)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# STEP 2: Remove polyclaw.service unit file
# ─────────────────────────────────────────────────────────────────────────────
log "Step 2: Remove systemd unit file..."

if [[ -f "${POLYCLAW_SERVICE}" ]]; then
    # Track size before removal
    UNIT_SIZE="$(du -sh "${POLYCLAW_SERVICE}" 2>/dev/null | cut -f1)"
    rm -f "${POLYCLAW_SERVICE}"
    log "  ✓ Removed ${POLYCLAW_SERVICE} (was ${UNIT_SIZE})"
else
    log "  (${POLYCLAW_SERVICE} already absent — skipping)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# STEP 3: daemon-reload
# ─────────────────────────────────────────────────────────────────────────────
log "Step 3: systemctl daemon-reload..."
systemctl daemon-reload
log "  ✓ daemon-reload complete"

# ─────────────────────────────────────────────────────────────────────────────
# STEP 4: Remove stale polyclaw binaries from /usr/local/bin/
#         Keep: nonzeroclaw, zeroclawed
#         Remove: polyclaw, polyclaw.bak*, polyclaw-acp-broken, polyclaw.v2-backup*
# ─────────────────────────────────────────────────────────────────────────────
log "Step 4: Remove stale polyclaw binaries from /usr/local/bin/..."

BINARIES_REMOVED=()
BINARIES_BYTES=0

# Build list of files to remove (explicit names + globs, evaluated safely)
mapfile -t BIN_CANDIDATES < <(
    find /usr/local/bin/ -maxdepth 1 \( \
        -name "polyclaw" \
        -o -name "polyclaw.bak*" \
        -o -name "polyclaw-acp-broken" \
        -o -name "polyclaw.v2-backup*" \
    \) 2>/dev/null || true
)

if [[ ${#BIN_CANDIDATES[@]} -eq 0 ]]; then
    log "  (no matching polyclaw binaries found — skipping)"
else
    for bin in "${BIN_CANDIDATES[@]}"; do
        # Double-check: never remove nonzeroclaw or zeroclawed
        basename_bin="$(basename "${bin}")"
        if [[ "${basename_bin}" == "nonzeroclaw" || "${basename_bin}" == "zeroclawed" ]]; then
            log "  SKIP (protected): ${bin}"
            continue
        fi
        size_bytes="$(stat -c%s "${bin}" 2>/dev/null || echo 0)"
        BINARIES_BYTES=$(( BINARIES_BYTES + size_bytes ))
        rm -f "${bin}"
        BINARIES_REMOVED+=("${basename_bin}")
        log "  ✓ Removed ${bin} ($(numfmt --to=iec-i --suffix=B "${size_bytes}" 2>/dev/null || echo "${size_bytes} bytes"))"
    done
    if [[ ${#BINARIES_REMOVED[@]} -eq 0 ]]; then
        log "  (all candidates were protected — nothing removed)"
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
# STEP 5: Archive /etc/polyclaw/ → /root/polyclaw-archived/<timestamp>/
# ─────────────────────────────────────────────────────────────────────────────
log "Step 5: Archive /etc/polyclaw/ config directory..."

if [[ -d "${POLYCLAW_CONFIG}" ]]; then
    ARCHIVE_TARGET="${ARCHIVE_DIR}/polyclaw-${TIMESTAMP}"
    mkdir -p "${ARCHIVE_DIR}"

    CONFIG_SIZE="$(du -sh "${POLYCLAW_CONFIG}" 2>/dev/null | cut -f1)"
    mv "${POLYCLAW_CONFIG}" "${ARCHIVE_TARGET}"
    log "  ✓ Moved ${POLYCLAW_CONFIG} → ${ARCHIVE_TARGET} (was ${CONFIG_SIZE})"
else
    log "  (/etc/polyclaw/ already absent — skipping)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# STEP 6: Disk space report
# ─────────────────────────────────────────────────────────────────────────────
log "Step 6: Disk space report..."

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Cleanup Summary"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Binaries freed
if [[ ${BINARIES_BYTES} -gt 0 ]]; then
    echo "  Binaries freed: $(numfmt --to=iec-i --suffix=B "${BINARIES_BYTES}" 2>/dev/null || echo "${BINARIES_BYTES} bytes")"
    echo "  Files removed:  ${BINARIES_REMOVED[*]:-none}"
else
    echo "  Binaries freed: 0 (already clean)"
fi

# Archive size
if [[ -d "${ARCHIVE_DIR}" ]]; then
    ARCHIVE_SIZE="$(du -sh "${ARCHIVE_DIR}" 2>/dev/null | cut -f1)"
    echo "  Archive dir:    ${ARCHIVE_DIR} (${ARCHIVE_SIZE})"
fi

# Current disk usage
echo ""
echo "  Current /usr/local/bin/ usage:"
du -sh /usr/local/bin/ 2>/dev/null || true
echo ""
echo "  Filesystem status:"
df -h / 2>/dev/null || true

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ─────────────────────────────────────────────────────────────────────────────
# STEP 7: Final verification
# ─────────────────────────────────────────────────────────────────────────────
log "Step 7: Final verification..."

# Confirm zeroclawed is still running after all changes
if systemctl is-active --quiet zeroclawed.service; then
    log "  ✓ zeroclawed.service still active — host is healthy"
else
    die "zeroclawed.service is no longer active after cleanup! Investigate immediately."
fi

# Confirm unit file is gone
if [[ ! -f "${POLYCLAW_SERVICE}" ]]; then
    log "  ✓ polyclaw.service unit file absent"
fi

# Confirm protected binaries are intact
for protected in zeroclawed nonzeroclaw; do
    if [[ -f "/usr/local/bin/${protected}" ]]; then
        log "  ✓ /usr/local/bin/${protected} intact"
    else
        log "  (note: /usr/local/bin/${protected} not present — may not be installed here)"
    fi
done

log "=== Remote cleanup complete ==="
REMOTE

log "=== cleanup-210-polyclaw.sh complete ==="

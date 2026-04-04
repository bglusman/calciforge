#!/bin/bash
# deploy-210.sh — Build and deploy zeroclawed + nonzeroclaw to CT 210
# 
# Usage: ./scripts/deploy-210.sh [--dry-run]
#
# What it does:
# 1. Builds zeroclawed and nonzeroclaw release binaries
# 2. Scans binaries for embedded secrets (safety check)
# 3. Copies binaries to .210
# 4. Creates /etc/zeroclawed/ config dir, migrates config from /etc/polyclaw/
# 5. Creates zeroclawed.service (replaces polyclaw.service)
# 6. Restarts services
# 7. Health checks

set -euo pipefail

TARGET="root@192.168.1.210"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH="ssh -i $SSH_KEY -o StrictHostKeyChecking=no -o ConnectTimeout=10 $TARGET"
SCP="scp -i $SSH_KEY -o StrictHostKeyChecking=no"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"
DRY_RUN=false

if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=true
    echo "🔍 DRY RUN — no changes will be made"
fi

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[deploy]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
err()  { echo -e "${RED}[error]${NC} $*"; }

# ---------------------------------------------------------------------------
# Step 1: Build
# ---------------------------------------------------------------------------
log "Building release binaries..."
cd "$REPO_DIR"

if ! cargo build --release -p zeroclawed 2>&1 | tail -5; then
    err "zeroclawed build failed"
    exit 1
fi

if ! cargo build --release -p nonzeroclaw 2>&1 | tail -5; then
    err "nonzeroclaw build failed"
    exit 1
fi

ZC_BIN="$REPO_DIR/target/release/zeroclawed"
NZC_BIN="$REPO_DIR/target/release/nonzeroclaw"

if [[ ! -f "$ZC_BIN" ]] || [[ ! -f "$NZC_BIN" ]]; then
    err "Binaries not found after build"
    exit 1
fi

log "Built: zeroclawed=$(du -h "$ZC_BIN" | cut -f1), nonzeroclaw=$(du -h "$NZC_BIN" | cut -f1)"

# ---------------------------------------------------------------------------
# Step 2: Secret scan
# ---------------------------------------------------------------------------
log "Scanning binaries for embedded secrets..."
for bin in "$ZC_BIN" "$NZC_BIN"; do
    if strings "$bin" | grep -qiE "sk-ant-oat01-|Cryptonomicon|glusman|12154609585"; then
        err "SECRETS FOUND in $(basename $bin)! Aborting."
        strings "$bin" | grep -iE "sk-ant-oat01-|Cryptonomicon|glusman|12154609585" | head -5
        exit 1
    fi
done
log "✅ No secrets in binaries"

# ---------------------------------------------------------------------------
# Step 3: Check .210 is reachable
# ---------------------------------------------------------------------------
log "Testing SSH to $TARGET..."
if ! $SSH "echo ok" >/dev/null 2>&1; then
    err "Cannot reach $TARGET via SSH"
    exit 1
fi
log "✅ SSH OK"

if $DRY_RUN; then
    log "Would copy binaries to /usr/local/bin/"
    log "Would create /etc/zeroclawed/ and migrate config"
    log "Would create zeroclawed.service"
    log "Would restart services"
    exit 0
fi

# ---------------------------------------------------------------------------
# Step 4: Backup current state on .210
# ---------------------------------------------------------------------------
log "Creating backup on .210..."
$SSH "
    mkdir -p /root/deploy-backup-$(date +%Y%m%d-%H%M%S)
    cp /usr/local/bin/polyclaw /root/deploy-backup-$(date +%Y%m%d-%H%M%S)/ 2>/dev/null || true
    cp /usr/local/bin/nonzeroclaw /root/deploy-backup-$(date +%Y%m%d-%H%M%S)/ 2>/dev/null || true
    cp -r /etc/polyclaw /root/deploy-backup-$(date +%Y%m%d-%H%M%S)/etc-polyclaw 2>/dev/null || true
    echo 'backup done'
"

# ---------------------------------------------------------------------------
# Step 5: Stop services before overwriting binaries
# ---------------------------------------------------------------------------
log "Stopping services before binary update..."
$SSH "
    systemctl stop zeroclawed.service 2>/dev/null || true
    systemctl stop polyclaw.service 2>/dev/null || true
    systemctl stop nonzeroclaw-david.service 2>/dev/null || true
    systemctl stop nonzeroclaw.service 2>/dev/null || true
    sleep 1
    echo 'services stopped'
"

# ---------------------------------------------------------------------------
# Step 6: Deploy binaries
# ---------------------------------------------------------------------------
log "Deploying binaries..."
$SCP "$ZC_BIN" "$TARGET:/usr/local/bin/zeroclawed"
$SCP "$NZC_BIN" "$TARGET:/usr/local/bin/nonzeroclaw"
$SSH "chmod +x /usr/local/bin/zeroclawed /usr/local/bin/nonzeroclaw"
log "✅ Binaries deployed"

# ---------------------------------------------------------------------------
# Step 6: Migrate config
# ---------------------------------------------------------------------------
log "Migrating config..."
$SSH "
    mkdir -p /etc/zeroclawed
    
    if [[ -f /etc/polyclaw/config.toml ]]; then
        # Copy and rename config section
        sed 's/^\[polyclaw\]/[zeroclawed]/' /etc/polyclaw/config.toml > /etc/zeroclawed/config.toml
        echo 'Config migrated from /etc/polyclaw/ to /etc/zeroclawed/'
    else
        echo 'No /etc/polyclaw/config.toml found — skipping config migration'
    fi
"
log "✅ Config migrated"

# ---------------------------------------------------------------------------
# Step 7: Create zeroclawed.service
# ---------------------------------------------------------------------------
log "Creating systemd service..."
$SSH 'cat > /etc/systemd/system/zeroclawed.service << EOF
[Unit]
Description=ZeroClawed — Agent gateway (Telegram, WhatsApp, Signal, Matrix)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=RUST_LOG=zeroclawed=info
ExecStart=/usr/local/bin/zeroclawed --config /etc/zeroclawed/config.toml
Environment=ZEROCLAWED_AGENT_TOKEN=Cryptonomicon381!
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=zeroclawed

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
echo "service created"'
log "✅ Service created"

# ---------------------------------------------------------------------------
# Step 8: Start new services
# ---------------------------------------------------------------------------
log "Starting new zeroclawed.service..."
$SSH "
    systemctl start zeroclawed.service
    sleep 2
    systemctl status zeroclawed.service --no-pager | head -15
"

# Restart nonzeroclaw services with new binary
log "Restarting nonzeroclaw services..."
$SSH "
    systemctl restart nonzeroclaw.service
    systemctl restart nonzeroclaw-david.service 2>/dev/null || true
    sleep 1
    echo 'nonzeroclaw services restarted'
"

# ---------------------------------------------------------------------------
# Step 9: Health check
# ---------------------------------------------------------------------------
log "Running health checks..."
$SSH "
    echo '--- zeroclawed service ---'
    systemctl is-active zeroclawed.service
    
    echo '--- nonzeroclaw service ---'
    systemctl is-active nonzeroclaw.service
    
    echo '--- nonzeroclaw-david service ---'  
    systemctl is-active nonzeroclaw-david.service 2>/dev/null || echo 'not found'
    
    echo '--- binary versions ---'
    /usr/local/bin/zeroclawed --version 2>/dev/null || echo 'no --version flag'
    /usr/local/bin/nonzeroclaw --version 2>/dev/null || echo 'no --version flag'
    
    echo '--- journal last 5 lines ---'
    journalctl -u zeroclawed.service --no-pager -n 5
"

log "✅ Deploy complete!"
log ""
log "Next steps:"
log "  1. Test Telegram: send a message to the bot"
log "  2. Test WhatsApp: send a message via WhatsApp"
log "  3. If all good: systemctl disable polyclaw.service"
log "  4. Optionally: rm /usr/local/bin/polyclaw"

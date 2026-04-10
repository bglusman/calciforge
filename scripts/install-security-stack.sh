#!/bin/bash
set -euo pipefail

# ZeroClawed Security Stack Installer
# Deploys: adversary-detector + security-gateway + clashd
# Reads agents.json to auto-configure credentials and proxy settings
#
# Supports multiple hosts — pass them all as args or via targets.txt

INSTALL_DIR="/opt/zeroclawed"
CONFIG_DIR="/etc/zeroclawed"
GATEWAY_PORT="8080"
DETECTOR_PORT="9800"
CLASHD_PORT="9001"
SSH_KEY="${SSH_KEY:-~/.ssh/id_ed25519}"

# ── Target resolution ──────────────────────────────────────────────
# Usage: install-security-stack.sh <action> [host1 host2 ...]
# If no hosts given, reads targets.txt (one IP per line)
# Example: ./scripts/install-security-stack.sh install 192.168.1.210 192.168.1.127 192.168.1.49

ACTION="${1:-help}"
shift || true

if [ $# -gt 0 ]; then
    TARGETS=("$@")
elif [ -f "$(dirname "$0")/targets.txt" ]; then
    mapfile -t TARGETS < <(grep -v '^#' "$(dirname "$0")/targets.txt" | grep -v '^$')
else
    echo "No targets specified. Usage:"
    echo "  $0 <action> host1 [host2 ...]"
    echo "  Or create scripts/targets.txt with one IP per line"
    exit 1
fi

echo "=== ZeroClawed Security Stack ==="
echo "Action: $ACTION"
echo "Targets: ${TARGETS[*]}"
echo ""

# ── SSH helper ─────────────────────────────────────────────────────
run_on() {
    local host="$1"; shift
    ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@$host" "$@"
}

copy_to() {
    local src="$1" host="$2" dst="$3"
    scp -i "$SSH_KEY" -o StrictHostKeyChecking=no "$src" "root@$host:$dst"
}

# ── Actions ────────────────────────────────────────────────────────
build() {
    echo "Building release binaries..."
    cd /root/projects/zeroclawed
    cargo build --release -p adversary-detector -p security-gateway -p clashd 2>&1 | tail -3
    echo "Build complete."
}

deploy_host() {
    local host="$1"
    echo "── Deploying to $host ──"

    # Create dirs
    run_on "$host" "mkdir -p $INSTALL_DIR/bin $CONFIG_DIR" 2>&1 || {
        echo "  ⚠ Cannot reach $host, skipping"
        return 1
    }

    # Copy binaries
    for bin in adversary-detector security-gateway clashd; do
        copy_to "/root/projects/zeroclawed/target/release/$bin" "$host" "$INSTALL_DIR/bin/$bin" 2>&1
        run_on "$host" "chmod +x $INSTALL_DIR/bin/$bin"
    done

    # Copy config
    copy_to "/root/projects/zeroclawed/crates/clashd/config/agents.json" "$host" "$CONFIG_DIR/agents.json" 2>&1
    copy_to "/root/projects/zeroclawed/crates/clashd/config/default-policy.star" "$host" "$CONFIG_DIR/default-policy.star" 2>&1

    # Install systemd services
    run_on "$host" "cat > /etc/systemd/system/adversary-detector.service << 'EOF'
[Unit]
Description=ZeroClawed Adversary Detector
After=network.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/bin/adversary-detector
Environment=ADVERSARY_DETECTOR_PORT=$DETECTOR_PORT
Environment=RUST_LOG=adversary_detector=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

cat > /etc/systemd/system/security-gateway.service << 'EOF'
[Unit]
Description=ZeroClawed Security Gateway
After=network.target adversary-detector.service

[Service]
Type=simple
ExecStart=$INSTALL_DIR/bin/security-gateway
Environment=AGENT_CONFIG=$CONFIG_DIR/agents.json
Environment=ADVERSARY_DETECTOR_PORT=$DETECTOR_PORT
Environment=RUST_LOG=security_gateway=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

cat > /etc/systemd/system/clashd.service << 'EOF'
[Unit]
Description=ZeroClawed Clashd Policy Engine
After=network.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/bin/clashd
Environment=CLASHD_CONFIG=$CONFIG_DIR/agents.json
Environment=RUST_LOG=clashd=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable adversary-detector security-gateway clashd
systemctl restart adversary-detector security-gateway clashd" 2>&1

    # Setup proxy env vars
    run_on "$host" "cat > /etc/profile.d/zeroclawed-proxy.sh << 'EOF'
# ZeroClawed Security Gateway — Tier 1 Enforcement
# All HTTP/HTTPS traffic is routed through the security gateway
export HTTP_PROXY=http://127.0.0.1:$GATEWAY_PORT
export HTTPS_PROXY=http://127.0.0.1:$GATEWAY_PORT
export NO_PROXY=localhost,127.0.0.1,192.168.1.*,10.*.*.*
EOF
chmod +x /etc/profile.d/zeroclawed-proxy.sh" 2>&1

    echo "  ✅ Deployed to $host"
}

verify_host() {
    local host="$1"
    echo "── Verifying $host ──"

    echo -n "  adversary-detector: "
    run_on "$host" "curl -s http://127.0.0.1:$DETECTOR_PORT/health" 2>/dev/null || echo "❌ not responding"

    echo -n "  security-gateway:  "
    run_on "$host" "curl -s http://127.0.0.1:$GATEWAY_PORT/health" 2>/dev/null || echo "❌ not responding"

    echo -n "  clashd:            "
    run_on "$host" "curl -s http://127.0.0.1:$CLASHD_PORT/health" 2>/dev/null || echo "❌ not responding"
}

# ── Main ───────────────────────────────────────────────────────────
case "$ACTION" in
    install)
        build
        for host in "${TARGETS[@]}"; do
            deploy_host "$host" || true
        done
        echo ""
        for host in "${TARGETS[@]}"; do
            verify_host "$host"
        done
        echo ""
        echo "=== Installation Complete ==="
        echo "Set API credentials on each host:"
        echo "  export OPENAI_API_KEY=sk-..."
        echo "  export ANTHROPIC_API_KEY=sk-ant-..."
        ;;
    deploy)
        for host in "${TARGETS[@]}"; do
            deploy_host "$host" || true
        done
        ;;
    verify)
        for host in "${TARGETS[@]}"; do
            verify_host "$host"
        done
        ;;
    build)
        build
        ;;
    restart)
        for host in "${TARGETS[@]}"; do
            echo "Restarting services on $host..."
            run_on "$host" "systemctl restart adversary-detector security-gateway clashd" 2>&1 || echo "  ⚠ $host unreachable"
        done
        ;;
    status)
        for host in "${TARGETS[@]}"; do
            echo "── $host ──"
            run_on "$host" "systemctl is-active adversary-detector security-gateway clashd 2>/dev/null | paste -sd' '" 2>&1 || echo "  unreachable"
        done
        ;;
    help|*)
        cat << 'USAGE'
ZeroClawed Security Stack Installer

Usage:
  scripts/install-security-stack.sh <action> [host1 host2 ...]

Actions:
  install   Build + deploy + configure + verify (full setup)
  deploy    Deploy binaries + config + services (skip build)
  build     Just build release binaries
  verify    Check service health on targets
  restart   Restart all services on targets
  status    Show service status on targets
  help      Show this help

Examples:
  # Single host
  ./scripts/install-security-stack.sh install 192.168.1.210

  # Multiple hosts
  ./scripts/install-security-stack.sh install 192.168.1.210 192.168.1.127 192.168.1.49

  # Use targets.txt (one IP per line)
  echo "192.168.1.210" > scripts/targets.txt
  echo "192.168.1.127" >> scripts/targets.txt
  ./scripts/install-security-stack.sh install

  # Just verify all hosts
  ./scripts/install-security-stack.sh verify 192.168.1.210 192.168.1.127

  # Set custom SSH key
  SSH_KEY=~/.ssh/id_ed25519_librarian ./scripts/install-security-stack.sh install 192.168.1.210
USAGE
        ;;
esac

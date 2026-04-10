#!/bin/bash
# clashd installation script
# Sets up clashd as a systemd service with default policies

set -e

CLASHD_USER="${CLASHD_USER:-librarian}"
CLASHD_DIR="${CLASHD_DIR:-/opt/clashd}"
CONFIG_DIR="${CONFIG_DIR:-/etc/clashd}"
SERVICE_NAME="clashd"

 echo "=== clashd Installation ==="
echo "User: $CLASHD_USER"
echo "Install dir: $CLASHD_DIR"
echo "Config dir: $CONFIG_DIR"
echo ""

# Build clashd
echo "Building clashd..."
cd "$(dirname "$0")/.."
cargo build --release -p clashd

# Create directories
echo "Creating directories..."
sudo mkdir -p "$CLASHD_DIR"
sudo mkdir -p "$CONFIG_DIR"
sudo mkdir -p /var/log/clashd

# Install binary
echo "Installing binary..."
sudo cp target/release/clashd "$CLASHD_DIR/"
sudo chmod +x "$CLASHD_DIR/clashd"

# Install default config if not exists
if [ ! -f "$CONFIG_DIR/policy.star" ]; then
    echo "Installing default policy..."
    sudo cp config/default-policy.star "$CONFIG_DIR/policy.star"
fi

# Create systemd service
echo "Creating systemd service..."
sudo tee /etc/systemd/system/$SERVICE_NAME.service > /dev/null <<EOF
[Unit]
Description=clashd - OpenClaw Policy Sidecar
After=network.target

[Service]
Type=simple
User=$CLASHD_USER
ExecStart=$CLASHD_DIR/clashd
Environment="CLASHD_POLICY=$CONFIG_DIR/policy.star"
Environment="CLASHD_AGENTS=$CONFIG_DIR/agents.json"
Environment="CLASHD_PORT=9001"
Environment="RUST_LOG=info"
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

# Reload and enable
echo "Enabling service..."
sudo systemctl daemon-reload
sudo systemctl enable $SERVICE_NAME

echo ""
echo "=== Installation Complete ==="
echo ""
echo "Start clashd:"
echo "  sudo systemctl start $SERVICE_NAME"
echo ""
echo "Check status:"
echo "  sudo systemctl status $SERVICE_NAME"
echo ""
echo "View logs:"
echo "  sudo journalctl -u $SERVICE_NAME -f"
echo ""
echo "Edit policy:"
echo "  sudo nano $CONFIG_DIR/policy.star"
echo ""
echo "Edit agent configs:"
echo "  sudo nano $CONFIG_DIR/agents.json"

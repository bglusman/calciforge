#!/bin/bash
# ZeroClawed Claw Integration Installer
# Sets up ZeptoClaw and IronClaw with OpenClaw proxy routing and fallbacks
#
# Usage: ./install-claw-integration.sh [zeroclawed-host]
# Default host: 192.168.1.210

set -euo pipefail

ZEROC_LAWED_HOST="${1:-192.168.1.210}"
SSH="ssh -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no -o ConnectTimeout=5"
OPENCLAW_PROXY="http://192.168.1.229:18789"
OPENCLAW_TOKEN="Cryptonomicon381!"

echo "=== ZeroClawed Claw Integration Installer ==="
echo "Target: $ZEROC_LAWED_HOST"
echo "OpenClaw Proxy: $OPENCLAW_PROXY"
echo ""

# Check connectivity
echo "Checking connectivity..."
if ! $SSH root@$ZEROC_LAWED_HOST "echo 'connected'" >/dev/null 2>&1; then
    echo "ERROR: Cannot connect to $ZEROC_LAWED_HOST"
    exit 1
fi
echo "✓ Connected"
echo ""

# Check if claws are installed
echo "Checking installed claws..."
$SSH root@$ZEROC_LAWED_HOST '
for cmd in zeptoclaw ironclaw zeroclawed; do
    if command -v $cmd >/dev/null 2>&1; then
        version=$($cmd --version 2>/dev/null || echo "unknown")
        echo "✓ $cmd: $version"
    else
        echo "✗ $cmd: NOT FOUND"
    fi
done
'
echo ""

# Create wrapper scripts
echo "Creating wrapper scripts..."

# ZeptoClaw wrapper
cat > /tmp/zeptoclaw-openclaw.sh << 'WRAPPER_EOF'
#!/bin/bash
# ZeptoClaw wrapper - routes through OpenClaw proxy with fallbacks
export ZEPTO_LLM_PROVIDER="openai"
export ZEPTO_LLM_BASE_URL="http://192.168.1.229:18789/v1"
export ZEPTO_LLM_API_KEY="Cryptonomicon381!"
export ZEPTO_LLM_MODEL="openclaw:main"
exec /usr/local/bin/zeptoclaw "$@"
WRAPPER_EOF

# IronClaw wrapper
cat > /tmp/ironclaw-openclaw.sh << 'WRAPPER_EOF'
#!/bin/bash
# IronClaw wrapper - routes through OpenClaw proxy with fallbacks
export IRONCLAW_LLM_PROVIDER="openai"
export IRONCLAW_LLM_BASE_URL="http://192.168.1.229:18789/v1"
export IRONCLAW_LLM_API_KEY="Cryptonomicon381!"
export IRONCLAW_LLM_MODEL="openclaw:main"
exec /usr/local/bin/ironclaw "$@"
WRAPPER_EOF

# Copy wrappers
scp -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no /tmp/zeptoclaw-openclaw.sh root@$ZEROC_LAWED_HOST:/usr/local/bin/zeptoclaw-openclaw
scp -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no /tmp/ironclaw-openclaw.sh root@$ZEROC_LAWED_HOST:/usr/local/bin/ironclaw-openclaw
$SSH root@$ZEROC_LAWED_HOST 'chmod +x /usr/local/bin/zeptoclaw-openclaw /usr/local/bin/ironclaw-openclaw'
echo "✓ Wrappers installed"
echo ""

# Backup and update ZeroClawed config
echo "Updating ZeroClawed config..."
$SSH root@$ZEROC_LAWED_HOST "
# Backup
backup_file=\"/etc/zeroclawed/config.toml.bak.\$(date +%Y%m%d-%H%M%S)\"
cp /etc/zeroclawed/config.toml \"\$backup_file\"
echo \"Backed up to: \$backup_file\"

# Check if zeptoclaw agent exists
if grep -q 'id = \"zeptoclaw\"' /etc/zeroclawed/config.toml; then
    # Update existing
    sed -i 's|command = \"/usr/local/bin/zeptoclaw\"|command = \"/usr/local/bin/zeptoclaw-openclaw\"|' /etc/zeroclawed/config.toml
    echo '✓ Updated zeptoclaw agent'
else
    # Add new
    cat >> /etc/zeroclawed/config.toml << 'EOF'

[[agents]]
id = \"zeptoclaw\"
kind = \"cli\"
command = \"/usr/local/bin/zeptoclaw-openclaw\"
args = [\"agent\", \"-m\", \"{message}\"]
timeout_ms = 60000
aliases = [\"zepto\"]
EOF
    echo '✓ Added zeptoclaw agent'
fi

# Check if ironclaw agent exists
if grep -q 'id = \"ironclaw\"' /etc/zeroclawed/config.toml; then
    # Update existing - remove old env vars and update command
    sed -i 's|command = \"/usr/local/bin/ironclaw\"|command = \"/usr/local/bin/ironclaw-openclaw\"|' /etc/zeroclawed/config.toml
    sed -i '/id = \"ironclaw\"/,/\[\[agents\]\]/ { /env = /d; }' /etc/zeroclawed/config.toml
    echo '✓ Updated ironclaw agent'
else
    # Add new
    cat >> /etc/zeroclawed/config.toml << 'EOF'

[[agents]]
id = \"ironclaw\"
kind = \"cli\"
command = \"/usr/local/bin/ironclaw-openclaw\"
args = [\"run\", \"-m\", \"{message}\"]
timeout_ms = 60000
EOF
    echo '✓ Added ironclaw agent'
fi

# Add to allowed_agents for brian
if grep -q 'identity = \"brian\"' /etc/zeroclawed/config.toml; then
    # Check if zeptoclaw is already in allowed list
    if ! grep -A5 'identity = \"brian\"' /etc/zeroclawed/config.toml | grep -q 'zeptoclaw'; then
        sed -i 's/allowed_agents = \[\([^]]*\)\]/allowed_agents = [\1, \"zeptoclaw\"]/' /etc/zeroclawed/config.toml
    fi
    if ! grep -A5 'identity = \"brian\"' /etc/zeroclawed/config.toml | grep -q 'ironclaw'; then
        sed -i 's/allowed_agents = \[\([^]]*\)\]/allowed_agents = [\1, \"ironclaw\"]/' /etc/zeroclawed/config.toml
    fi
    echo '✓ Updated routing for brian'
fi
"

echo ""
echo "Restarting ZeroClawed..."
$SSH root@$ZEROC_LAWED_HOST "systemctl restart zeroclawed && sleep 2 && systemctl is-active zeroclawed"
echo "✓ ZeroClawed restarted"
echo ""

echo "=== Installation Complete ==="
echo ""
echo "You can now use:"
echo "  @zepto or @zeptoclaw  -> Routes to ZeptoClaw via OpenClaw proxy"
echo "  @ironclaw              -> Routes to IronClaw via OpenClaw proxy"
echo ""
echo "Both claws use OpenClaw's model fallbacks:"
echo "  Primary: openclaw:main (routes through all providers)"
echo "  Fallbacks: kimi, openrouter models, local LLM"

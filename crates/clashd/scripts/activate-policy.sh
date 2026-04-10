#!/bin/bash
# Activate policy enforcement on OpenClaw agent
# This sets up the zeroclawed-policy-plugin to call clashd

set -e

CLASHD_HOST="${CLASHD_HOST:-127.0.0.1}"
CLASHD_PORT="${CLASHD_PORT:-9001}"
PLUGIN_DIR="${PLUGIN_DIR:-$HOME/.openclaw/plugins}"

 echo "=== Policy Activation ==="
echo "clashd endpoint: http://$CLASHD_HOST:$CLASHD_PORT"
echo "Plugin directory: $PLUGIN_DIR"
echo ""

# Ensure clashd is reachable
echo "Checking clashd..."
if ! curl -s "http://$CLASHD_HOST:$CLASHD_PORT/health" > /dev/null 2>&1; then
    echo "⚠️  clashd not responding at http://$CLASHD_HOST:$CLASHD_PORT"
    echo "   Start clashd first, then re-run this script"
    echo ""
    read -p "Continue anyway? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Create plugin directory
mkdir -p "$PLUGIN_DIR/before_tool_call"

# Get plugin path (relative to this script)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_SOURCE="$SCRIPT_DIR/../../zeroclawed-policy-plugin/before_tool_call"

if [ ! -d "$PLUGIN_SOURCE" ]; then
    echo "Error: Plugin source not found at $PLUGIN_SOURCE"
    exit 1
fi

# Copy plugin files
echo "Installing plugin..."
cp "$PLUGIN_SOURCE/index.ts" "$PLUGIN_DIR/before_tool_call/"
cp "$PLUGIN_SOURCE/tsconfig.json" "$PLUGIN_DIR/before_tool_call/" 2>/dev/null || true

# Create environment config for OpenClaw
echo "Configuring environment..."
mkdir -p "$HOME/.config/openclaw"
cat > "$HOME/.config/openclaw/policy.env" <<EOF
# Policy plugin configuration
CLASHD_ENDPOINT=http://$CLASHD_HOST:$CLASHD_PORT/evaluate
CLASHD_TIMEOUT_MS=500
EOF

echo ""
echo "=== Activation Complete ==="
echo ""
echo "Plugin installed to: $PLUGIN_DIR/before_tool_call/"
echo "Config: $HOME/.config/openclaw/policy.env"
echo ""
echo "Add to your shell profile to persist:"
echo "  source $HOME/.config/openclaw/policy.env"
echo ""
echo "Or add to OpenClaw's environment:"
echo "  export CLASHD_ENDPOINT=http://$CLASHD_HOST:$CLASHD_PORT/evaluate"
echo ""
echo "Test the setup:"
echo "  curl http://$CLASHD_HOST:$CLASHD_PORT/"

#!/bin/bash
# check-openclaw-version.sh
# Verify OpenClaw version meets requirements for zeroclawed-policy-plugin

set -e

MIN_VERSION="2026.3.24-beta.2"
REQUIRED_FEATURE="before_tool_call hook with requireApproval"

echo "Checking OpenClaw version requirements for ZeroClawed policy enforcement..."
echo ""

# Check if openclaw command exists
if ! command -v openclaw &> /dev/null; then
    echo "❌ ERROR: openclaw command not found"
    echo "   Please install OpenClaw first: https://docs.openclaw.ai/install"
    exit 1
fi

# Get OpenClaw version
OPENCLAW_VERSION=$(openclaw --version 2>/dev/null | head -1 | grep -oP '\d{4}\.\d+\.\d+[^\s]*' || echo "unknown")

if [ "$OPENCLAW_VERSION" = "unknown" ]; then
    echo "⚠️  WARNING: Could not determine OpenClaw version"
    echo "   Make sure openclaw is properly installed"
    exit 1
fi

echo "Detected OpenClaw version: $OPENCLAW_VERSION"
echo "Minimum required version:  $MIN_VERSION"
echo ""

# Simple version comparison (handles YYYY.M.N-beta.N format)
version_ge() {
    # Extract version components
    local v1="$1"
    local v2="$2"
    
    # Use sort -V for version comparison
    if printf '%s\n%s\n' "$v2" "$v1" | sort -V -C; then
        return 0  # v1 >= v2
    else
        return 1  # v1 < v2
    fi
}

if version_ge "$OPENCLAW_VERSION" "$MIN_VERSION"; then
    echo "✅ OpenClaw version check PASSED"
    echo ""
    echo "Required features:"
    echo "  ✅ before_tool_call hook"
    echo "  ✅ requireApproval in hook results"
    echo ""
    echo "You can now install the zeroclawed-policy-plugin"
else
    echo "❌ OpenClaw version check FAILED"
    echo ""
    echo "Your version: $OPENCLAW_VERSION"
    echo "Required:     >= $MIN_VERSION"
    echo ""
    echo "Required features missing:"
    echo "  - $REQUIRED_FEATURE"
    echo ""
    echo "Please upgrade OpenClaw:"
    echo "  openclaw update"
    echo ""
    echo "Or download the latest release:"
    echo "  https://github.com/openclaw/openclaw/releases"
    exit 1
fi

# Check if clashd is available
echo "Checking clashd availability..."
if curl -s http://localhost:9001/health > /dev/null 2>&1; then
    echo "✅ clashd is running on localhost:9001"
else
    echo "⚠️  clashd is not running on localhost:9001"
    echo "   The plugin will not work without clashd."
    echo ""
    echo "Start clashd with:"
    echo "  docker run -d --name clashd -p 9001:9001 zeroclawed/clashd:latest"
    echo ""
    echo "Or from source:"
    echo "  cd /path/to/zeroclawed && cargo run -p clashd"
    exit 1
fi

echo ""
echo "✅ All requirements met! You can install the policy plugin."

#!/bin/bash
# Test script to demonstrate config validation

echo "=== ZeroClawed Config Validator Demo ==="
echo ""

# Create a test config with intentional errors
cat > /tmp/test_bad_config.toml << 'EOF'
[zeroclawed]
version = 2

[[identities]]
id = "brian"
role = "owner"

[[identities]]
id = "brian"  # DUPLICATE!
role = "user"

[[agents]]
id = "test-agent"
kind = "zeroclaw"
endpoint = "http://localhost:8080"

[[routing]]
identity = "brian"
default_agent = "nonexistent-agent"  # Doesn't exist!

[[alloys]]
id = "test-alloy"
name = "Test Alloy"
strategy = "invalid_strategy"  # Invalid!
constituents = []
EOF

echo "1. Created test config with intentional errors:"
echo "   - Duplicate identity 'brian'"
echo "   - Routing references non-existent agent 'nonexistent-agent'"
echo "   - Alloy has invalid strategy 'invalid_strategy'"
echo "   - Alloy has no constituents"
echo ""

# This would be the command once built:
echo "2. Once built, you would run:"
echo "   zeroclawed --config /tmp/test_bad_config.toml --validate"
echo ""

# For now, show what the validator would catch
echo "3. The validator would catch these errors:"
echo "   ❌ Duplicate identity ID: 'brian'"
echo "   ❌ Routing rule for 'brian' references non-existent agent: 'nonexistent-agent'"
echo "   ❌ Alloy 'test-alloy' has invalid strategy: 'invalid_strategy'"
echo "   ⚠️  Alloy 'test-alloy' has no constituents and will be unusable"
echo ""

# Create a valid config
cat > /tmp/test_good_config.toml << 'EOF'
[zeroclawed]
version = 2

[[identities]]
id = "brian"
role = "owner"

[[agents]]
id = "test-agent"
kind = "zeroclaw"
endpoint = "http://localhost:8080"

[[routing]]
identity = "brian"
default_agent = "test-agent"
EOF

echo "4. Created valid test config"
echo ""
echo "5. Once built, validation would show:"
echo "   ✅ Configuration is valid!"
echo ""

# Cleanup
rm -f /tmp/test_bad_config.toml /tmp/test_good_config.toml

echo "=== Summary ==="
echo "The config validator prevents runtime failures by catching:"
echo "  - Duplicate IDs (identities, agents, channels, alloys)"
echo "  - Invalid references (routing to non-existent agents)"
echo "  - Invalid values (unknown strategies, bad ports)"
echo "  - Syntax errors (malformed TOML)"
echo ""
echo "Usage:"
echo "  # Validate before deploying"
echo "  zeroclawed --config /etc/zeroclawed/config.toml --validate"
echo ""
echo "  # CI/CD integration"
echo "  zeroclawed --validate && systemctl restart zeroclawed"

#!/bin/bash
# Run Ralph's backlog tests
# These tests INTENTIONALLY FAIL to highlight unimplemented features

set -e

echo "🔴📋 RALPH'S BACKLOG: INTENTIONALLY FAILING TESTS 📋🔴"
echo "======================================================"
echo ""
echo "These tests FAIL ON PURPOSE to highlight:"
echo "1. Unimplemented features"
echo "2. Bugs that need fixing"
echo "3. Security issues"
echo ""
echo "Each failure is a TODO item for Ralph!"
echo ""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Add tests to Cargo.toml if not already there
if ! grep -q "test_ralph_backlog" /root/projects/zeroclawed/Cargo.toml; then
    echo -e "${YELLOW}📝 Adding tests to Cargo.toml...${NC}"
    cat >> /root/projects/zeroclawed/Cargo.toml << 'EOF'

[[test]]
name = "ralph_backlog"
path = "test_ralph_backlog.rs"
EOF
fi

# Build first
echo -e "${YELLOW}🔨 Building tests...${NC}"
cd /root/projects/zeroclawed
cargo test --test ralph_backlog --no-run 2>&1 | tail -20

echo ""
echo -e "${YELLOW}🧪 Running backlog tests (expect failures!)...${NC}"
echo ""

# Run tests and capture output
TEST_OUTPUT=$(cargo test --test ralph_backlog -- --nocapture 2>&1 || true)

# Parse results
echo "$TEST_OUTPUT" | while IFS= read -r line; do
    if [[ "$line" == *"test test_"* && "$line" == *"..."* ]]; then
        TEST_NAME=$(echo "$line" | sed 's/.*test \(.*\) \.\.\..*/\1/')
        if [[ "$line" == *"FAILED"* ]]; then
            echo -e "${RED}❌ $TEST_NAME - FAILED (AS EXPECTED)${NC}"
        elif [[ "$line" == *"ok"* ]]; then
            echo -e "${GREEN}✅ $TEST_NAME - PASSED (UNEXPECTED - FIX MAY BE NEEDED)${NC}"
        else
            echo -e "${YELLOW}⚠️  $TEST_NAME - ${line##*...}${NC}"
        fi
    elif [[ "$line" == *"FAILED"* ]] && [[ "$line" == *"tests"* ]]; then
        echo ""
        echo -e "${RED}$line${NC}"
    elif [[ "$line" == *"test result:"* ]]; then
        echo ""
        echo -e "${YELLOW}$line${NC}"
    fi
done

# Extract summary
echo ""
echo -e "${YELLOW}📊 BACKLOG SUMMARY${NC}"
echo "=================="

# Count tests
TOTAL_TESTS=$(echo "$TEST_OUTPUT" | grep -c "test test_")
FAILED_TESTS=$(echo "$TEST_OUTPUT" | grep -c "test test_.*FAILED")
PASSED_TESTS=$(echo "$TEST_OUTPUT" | grep -c "test test_.*ok")

echo "Total tests: $TOTAL_TESTS"
echo -e "${RED}Intentionally failing: $FAILED_TESTS${NC}"
echo -e "${GREEN}Unexpectedly passing: $PASSED_TESTS${NC}"

echo ""
echo -e "${YELLOW}🎯 RALPH'S TODO LIST (from failing tests):${NC}"
echo "======================================"

# Extract test names and expected failures
echo "$TEST_OUTPUT" | grep -A1 "test test_.*FAILED" | while read -r line1 && read -r line2; do
    if [[ "$line1" == *"test test_"* ]]; then
        TEST_NAME=$(echo "$line1" | sed 's/.*test \(.*\) \.\.\..*/\1/')
        FAILURE_REASON=$(echo "$line2" | grep -o "expected .*" | sed 's/expected //' | head -1)
        if [ -n "$FAILURE_REASON" ]; then
            echo "• $TEST_NAME: $FAILURE_REASON"
        else
            echo "• $TEST_NAME: (see test for details)"
        fi
    fi
done

echo ""
echo -e "${YELLOW}📝 NEXT STEPS FOR RALPH:${NC}"
echo "=========================="
echo "1. Review failing tests above"
echo "2. Fix security issues first (error message leakage)"
echo "3. Implement missing features (rate limiting, streaming)"
echo "4. Fix bugs (invalid model handling, concurrent deadlocks)"
echo "5. Run tests again to track progress"
echo ""
echo -e "${GREEN}💡 Remember: These tests FAIL ON PURPOSE to guide development!${NC}"
echo "Each failure is a feature request or bug report in test form."
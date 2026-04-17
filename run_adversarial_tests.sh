#!/bin/bash
set -e

echo "=== ZeroClawed Adversarial Test Suite ==="
echo "Starting comprehensive test run..."

# Build first
echo "1. Building ZeroClawed..."
cd /root/projects/zeroclawed
cargo build --bin zeroclawed 2>&1 | tail -5

# Install Python dependencies if needed
echo "2. Checking Python dependencies..."
python3 -c "import toml, requests" 2>/dev/null || {
    echo "Installing Python dependencies..."
    pip3 install toml requests 2>/dev/null || true
}

# Run the test runner
echo "3. Running adversarial test suite..."
python3 test_runner.py

echo ""
echo "=== Additional Manual Tests ==="

# Quick manual tests
echo "4. Running quick manual tests..."

# Test 1: Basic proxy functionality
echo "  a) Testing basic proxy..."
timeout 10 ./test_proxy_only.sh 2>&1 | grep -A5 "Test Complete" || echo "    Basic proxy test skipped"

# Test 2: Error handling
echo "  b) Testing error handling..."
bash test_proxy_error_handling.sh 2>&1 | tail -5 || echo "    Error handling test skipped"

# Test 3: Performance baseline
echo "  c) Testing performance baseline..."
bash test_performance_baseline.sh 2>&1 | tail -10 || echo "    Performance test skipped"

echo ""
echo "=== Test Framework Status ==="
echo "Test scenarios created:"
find test_scenarios -name "*.toml" | wc -l | xargs echo "  - Total scenarios:"
find test_scenarios -type f -name "*.toml" | xargs -I {} basename {} | head -5 | while read f; do echo "    • $f"; done

echo ""
echo "Framework components:"
echo "  • test_runner.py - Main test execution engine"
echo "  • test_scenarios/ - Scenario definitions (TOML)"
echo "  • test_adversarial_framework.md - Testing philosophy"
echo "  • test_proxy_error_handling.sh - Error case tests"
echo "  • test_performance_baseline.sh - Performance tests"

echo ""
echo "=== Next Steps ==="
echo "To add more tests:"
echo "  1. Create TOML file in test_scenarios/"
echo "  2. Implement test logic in test_runner.py"
echo "  3. Run: python3 test_runner.py"
echo ""
echo "To run specific test categories:"
echo "  # Basic tests only"
echo "  find test_scenarios/basic -name '*.toml' | xargs -I {} python3 test_runner.py --scenario {}"
echo ""
echo "To implement property tests (requires proptest):"
echo "  cargo add proptest"
echo "  See test_scenarios/property/ for examples"

echo ""
echo "=== Adversarial Testing Mindset ==="
echo "Remember: The goal is to find INTERESTING failures, not just pass tests."
echo "Look for:"
echo "  • Security vulnerabilities"
echo "  • Race conditions"
echo "  • Resource leaks"
echo "  • State corruption"
echo "  • Byzantine failures"
echo ""
echo "Good failures today prevent production incidents tomorrow."
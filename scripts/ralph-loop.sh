#!/usr/bin/env bash
# Ralph Loop - Automated test iteration
# Iterates on cargo test until all pass or max iterations reached
#
# Usage: ./scripts/ralph-loop.sh [--max-iterations N]

set -euo pipefail

MAX_ITERATIONS=${1:-20}
ITERATION=0
LAST_FAILURE_COUNT=0

echo "=== Ralph Loop: Integration Test Iteration ==="
echo "Max iterations: $MAX_ITERATIONS"
echo ""

while [ $ITERATION -lt $MAX_ITERATIONS ]; do
    ITERATION=$((ITERATION + 1))
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "🔄 Iteration $ITERATION/$MAX_ITERATIONS"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    
    # Run cargo test, capture output
    if cargo test -p zeroclawed --no-fail-fast 2>&1 | tee /tmp/ralph-output.log; then
        echo ""
        echo "✅ ALL TESTS PASSED after $ITERATION iterations"
        echo ""
        
        # Show summary
        echo "📊 Test Summary:"
        grep "test result:" /tmp/ralph-output.log | tail -5 || true
        
        exit 0
    fi
    
    # Count failures
    FAILURES=$(grep -c "FAILED" /tmp/ralph-output.log || echo "0")
    echo ""
    echo "❌ $FAILURES test(s) failed"
    echo ""
    
    # Run failure analysis
    if [ -f scripts/analyze-failures.py ]; then
        echo "🔍 Analyzing failures..."
        python3 scripts/analyze-failures.py /tmp/ralph-output.log || true
        echo ""
    fi
    
    # Check if we're making progress
    if [ "$FAILURES" -eq "$LAST_FAILURE_COUNT" ] && [ $ITERATION -gt 1 ]; then
        echo "⚠️  Warning: Failure count not improving ($FAILURES -> $LAST_FAILURE_COUNT)"
    fi
    LAST_FAILURE_COUNT=$FAILURES
    
    # Brief pause between iterations
    if [ $ITERATION -lt $MAX_ITERATIONS ]; then
        echo "⏳ Pausing 2 seconds before next iteration..."
        sleep 2
    fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "⚠️  MAX ITERATIONS REACHED ($MAX_ITERATIONS)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Last failure summary:"
grep -A 5 "FAILED" /tmp/ralph-output.log | tail -30 || true

echo ""
echo "Consider:"
echo "  1. Running with --max-iterations N for more attempts"
echo "  2. Checking specific test: cargo test -p zeroclawed TEST_NAME"
echo "  3. Running with RUST_BACKTRACE=1 for more details"

exit 1

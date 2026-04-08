#!/usr/bin/env bash
# Ralph Loop - Automated test iteration with timer-based escalation
# 
# Usage: ./scripts/ralph-loop-overnight.sh [--max-iterations N] [--check-interval MINUTES]
# 
# This script:
# 1. Runs tests up to N iterations
# 2. If stuck (no progress), reworks tests and retries
# 3. Uses timer-based checks for overnight runs
# 4. Escalates iteration limits up to 60 if making progress

set -euo pipefail

MAX_ITERATIONS=${1:-20}
CHECK_INTERVAL_MINUTES=${2:-60}  # Check every hour by default
ITERATION=0
LAST_FAILURE_COUNT=-1
CONSECUTIVE_NO_PROGRESS=0
START_TIME=$(date +%s)

# Progress log
PROGRESS_LOG="/tmp/ralph-progress-$(date +%Y%m%d-%H%M%S).log"
echo "Ralph Loop started at $(date)" > "$PROGRESS_LOG"
echo "Max iterations: $MAX_ITERATIONS" >> "$PROGRESS_LOG"
echo "Check interval: ${CHECK_INTERVAL_MINUTES}m" >> "$PROGRESS_LOG"
echo "" >> "$PROGRESS_LOG"

log_progress() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') - $1" | tee -a "$PROGRESS_LOG"
}

run_tests() {
    local iteration=$1
    log_progress "=== Iteration $iteration/$MAX_ITERATIONS ==="
    
    # Run cargo test
    if cargo test -p zeroclawed --test e2e --no-fail-fast 2>&1 | tee /tmp/ralph-output-$iteration.log; then
        log_progress "✅ ALL TESTS PASSED on iteration $iteration"
        
        # Also run property tests
        log_progress "Running property tests..."
        if cargo test -p zeroclawed --test e2e property_tests --no-fail-fast 2>&1 | tee -a /tmp/ralph-output-$iteration.log; then
            log_progress "✅ Property tests passed"
        else
            log_progress "⚠️ Some property tests failed (may be expected for edge cases)"
        fi
        
        return 0
    fi
    
    return 1
}

count_failures() {
    grep -c "FAILED\|error\[E" /tmp/ralph-output-$1.log 2>/dev/null || echo "0"
}

analyze_failures() {
    local iteration=$1
    local log_file="/tmp/ralph-output-$iteration.log"
    
    log_progress "Analyzing failures..."
    
    if [ -f scripts/analyze-failures.py ]; then
        python3 scripts/analyze-failures.py "$log_file" 2>/dev/null | tee -a "$PROGRESS_LOG" || true
    fi
    
    # Check for specific patterns
    if grep -q "404" "$log_file"; then
        log_progress "⚠️ Detected: 404 errors (path routing issue)"
    fi
    
    if grep -q "tool_calls" "$log_file"; then
        log_progress "⚠️ Detected: Tool call related failures"
    fi
    
    if grep -q "unknown.*kind" "$log_file"; then
        log_progress "⚠️ Detected: Unknown adapter kind errors"
    fi
}

# Main loop
while [ $ITERATION -lt $MAX_ITERATIONS ]; do
    ITERATION=$((ITERATION + 1))
    
    # Run tests
    if run_tests $ITERATION; then
        log_progress "🎉 SUCCESS after $ITERATION iterations"
        log_progress "Total time: $(($(date +%s) - START_TIME))s"
        
        echo ""
        echo "📊 Final Summary:"
        echo "================="
        cat "$PROGRESS_LOG"
        echo ""
        echo "All tests passed! Exiting."
        exit 0
    fi
    
    # Count failures
    FAILURES=$(count_failures $ITERATION)
    log_progress "❌ $FAILURES test(s) failed on iteration $ITERATION"
    
    # Analyze failures
    analyze_failures $ITERATION
    
    # Check for progress
    if [ "$FAILURES" -eq "$LAST_FAILURE_COUNT" ] && [ $ITERATION -gt 1 ]; then
        CONSECUTIVE_NO_PROGRESS=$((CONSECUTIVE_NO_PROGRESS + 1))
        log_progress "⚠️ No progress ($CONSECUTIVE_NO_PROGRESS consecutive iterations)"
        
        if [ $CONSECUTIVE_NO_PROGRESS -ge 3 ]; then
            log_progress "🔄 STUCK - Consider reworking tests or prompt"
            log_progress "   Suggestion: Review test design, check for fundamental issues"
            
            # Option: Could trigger automated rework here
            # For now, just report and continue
        fi
    else
        CONSECUTIVE_NO_PROGRESS=0
        
        # If making progress but not done, consider escalating
        if [ $FAILURES -lt "$LAST_FAILURE_COUNT" ] && [ $ITERATION -gt 5 ]; then
            if [ $MAX_ITERATIONS -lt 60 ]; then
                NEW_MAX=$((MAX_ITERATIONS + 20))
                log_progress "📈 Making progress! Escalating max iterations: $MAX_ITERATIONS → $NEW_MAX"
                MAX_ITERATIONS=$NEW_MAX
            fi
        fi
    fi
    
    LAST_FAILURE_COUNT=$FAILURES
    
    # Timer-based check
    ELAPSED=$((($(date +%s) - START_TIME) / 60))  # in minutes
    log_progress "⏱️ Elapsed: ${ELAPSED}m"
    
    if [ $ITERATION -lt $MAX_ITERATIONS ]; then
        SLEEP_SECS=$((CHECK_INTERVAL_MINUTES * 60))
        log_progress "⏳ Sleeping ${CHECK_INTERVAL_MINUTES}m before next iteration..."
        sleep $SLEEP_SECS
    fi
done

log_progress "⚠️ MAX ITERATIONS ($MAX_ITERATIONS) REACHED"
log_progress "Total time: $(($(date +%s) - START_TIME))s"

echo ""
echo "📊 Final Progress Log:"
echo "====================="
cat "$PROGRESS_LOG"

echo ""
echo "📋 Suggestions:"
echo "1. Review failure logs: /tmp/ralph-output-*.log"
echo "2. Check specific failures: cargo test -p zeroclawed --test e2e -- --nocapture"
echo "3. Consider: Are tests too strict? Are bugs fundamental?"
echo "4. Run manual Docker test: ./scripts/manual-docker-test.sh"

exit 1

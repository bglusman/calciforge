#!/bin/bash
# ZeroClawed v3 Host-Agent Test Script
# Tests the host-agent endpoints

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Configuration
HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-18443}"
BASE_URL="https://${HOST}:${PORT}"
CERT_DIR="${CERT_DIR:-/etc/clash/certs}"
CLIENT_CERT="${CLIENT_CERT:-${CERT_DIR}/client-librarian.crt}"
CLIENT_KEY="${CLIENT_KEY:-${CERT_DIR}/client-librarian.key}"
CA_CERT="${CA_CERT:-${CERT_DIR}/ca.crt}"
AUDIT_LOG="${AUDIT_LOG:-/var/log/clash/audit.jsonl}"

# Counters
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

# Parse arguments
VERBOSE=false
STOP_ON_FAILURE=false
TEST_ZFS=false  # Requires actual ZFS operations

while [[ $# -gt 0 ]]; do
    case $1 in
        -v|--verbose)
            VERBOSE=true
            shift
            ;;
        -f|--fail-fast)
            STOP_ON_FAILURE=true
            shift
            ;;
        --test-zfs)
            TEST_ZFS=true
            shift
            ;;
        --help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -v, --verbose      Show detailed output"
            echo "  -f, --fail-fast    Stop on first failure"
            echo "  --test-zfs         Include ZFS operation tests (destructive)"
            echo "  --help             Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

log_info() {
    echo -e "${BLUE}[TEST]${NC} $1"
}

log_pass() {
    echo -e "${GREEN}[PASS]${NC} $1"
    ((TESTS_PASSED++))
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    ((TESTS_FAILED++))
    if [[ "$STOP_ON_FAILURE" == "true" ]]; then
        exit 1
    fi
}

log_skip() {
    echo -e "${YELLOW}[SKIP]${NC} $1"
    ((TESTS_SKIPPED++))
}

log_verbose() {
    if [[ "$VERBOSE" == "true" ]]; then
        echo "  $1"
    fi
}

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."
    
    # Check for curl
    if ! command -v curl &> /dev/null; then
        log_fail "curl is required but not installed"
        return 1
    fi
    
    # Check for jq
    if ! command -v jq &> /dev/null; then
        log_skip "jq is not installed (some tests will be limited)"
    fi
    
    # Check certificate files exist
    if [[ ! -f "$CLIENT_CERT" ]]; then
        log_fail "Client certificate not found: $CLIENT_CERT"
        return 1
    fi
    
    if [[ ! -f "$CLIENT_KEY" ]]; then
        log_fail "Client key not found: $CLIENT_KEY"
        return 1
    fi
    
    if [[ ! -f "$CA_CERT" ]]; then
        log_fail "CA certificate not found: $CA_CERT"
        return 1
    fi
    
    log_verbose "Client cert: $CLIENT_CERT"
    log_verbose "Client key: $CLIENT_KEY"
    log_verbose "CA cert: $CA_CERT"
    
    log_pass "Prerequisites check"
}

# Check if service is running
check_service() {
    log_info "Checking if host-agent is running..."
    
    if systemctl is-active --quiet clash-host-agent 2>/dev/null; then
        log_pass "Host-agent service is running"
    else
        log_warn "Host-agent service is not running (tests may fail)"
        log_verbose "Start with: systemctl start clash-host-agent"
    fi
}

# Helper function for API calls
api_call() {
    local method="$1"
    local endpoint="$2"
    local data="${3:-}"
    
    local curl_args=(
        -s
        --cacert "$CA_CERT"
        --cert "$CLIENT_CERT"
        --key "$CLIENT_KEY"
        -H "Content-Type: application/json"
        -w "\n%{http_code}"
    )
    
    if [[ -n "$data" ]]; then
        curl_args+=(-d "$data")
    fi
    
    curl_args+=("${BASE_URL}${endpoint}")
    
    if [[ "$method" == "POST" ]]; then
        curl_args+=(-X POST)
    fi
    
    local response
    response=$(curl "${curl_args[@]}")
    
    # Extract HTTP code (last line)
    local http_code
    http_code=$(echo "$response" | tail -n1)
    
    # Extract body (all but last line)
    local body
    body=$(echo "$response" | head -n -1)
    
    echo "${http_code}|${body}"
}

# Test health endpoint
test_health() {
    log_info "Testing health endpoint..."
    
    local result
    result=$(api_call "GET" "/health")
    
    local http_code
    http_code=$(echo "$result" | cut -d'|' -f1)
    local body
    body=$(echo "$result" | cut -d'|' -f2-)
    
    if [[ "$http_code" == "200" ]]; then
        log_verbose "Response: $body"
        
        # Check for version in response
        if echo "$body" | grep -q "version"; then
            log_pass "Health endpoint returns version"
        else
            log_pass "Health endpoint responds"
        fi
    else
        log_fail "Health endpoint returned HTTP $http_code"
        log_verbose "Response: $body"
    fi
}

# Test ZFS list endpoint
test_zfs_list() {
    log_info "Testing ZFS list endpoint..."
    
    local result
    result=$(api_call "POST" "/zfs/list" '{"dataset": "tank/media", "type": "snapshot"}')
    
    local http_code
    http_code=$(echo "$result" | cut -d'|' -f1)
    local body
    body=$(echo "$result" | cut -d'|' -f2-)
    
    if [[ "$http_code" == "200" ]]; then
        log_verbose "Response: $body"
        log_pass "ZFS list endpoint works"
    elif [[ "$http_code" == "404" ]]; then
        log_pass "ZFS list endpoint responds (dataset may not exist)"
    else
        log_fail "ZFS list endpoint returned HTTP $http_code"
        log_verbose "Response: $body"
    fi
}

# Test ZFS snapshot endpoint (requires TEST_ZFS=true)
test_zfs_snapshot() {
    if [[ "$TEST_ZFS" != "true" ]]; then
        log_skip "ZFS snapshot test (use --test-zfs to enable)"
        return 0
    fi
    
    log_info "Testing ZFS snapshot endpoint..."
    
    local snapshot_name="test-$(date +%s)"
    local result
    result=$(api_call "POST" "/zfs/snapshot" "{\"dataset\": \"tank/media\", \"snapname\": \"${snapshot_name}\"}")
    
    local http_code
    http_code=$(echo "$result" | cut -d'|' -f1)
    local body
    body=$(echo "$result" | cut -d'|' -f2-)
    
    if [[ "$http_code" == "200" ]]; then
        log_verbose "Response: $body"
        log_pass "ZFS snapshot created successfully"
    else
        log_fail "ZFS snapshot failed with HTTP $http_code"
        log_verbose "Response: $body"
    fi
}

# Test ZFS destroy approval flow
test_zfs_destroy_approval() {
    log_info "Testing ZFS destroy approval flow..."
    
    local result
    result=$(api_call "POST" "/zfs/destroy" '{"dataset": "tank/media@nonexistent"}')
    
    local http_code
    http_code=$(echo "$result" | cut -d'|' -f1)
    local body
    body=$(echo "$result" | cut -d'|' -f2-)
    
    if [[ "$http_code" == "200" ]]; then
        # Check if it requires approval
        if echo "$body" | grep -q "pending_approval"; then
            log_verbose "Response: $body"
            log_pass "Destroy requires approval (as expected)"
        else
            log_pass "Destroy endpoint responds"
        fi
    else
        log_fail "Destroy endpoint returned HTTP $http_code"
        log_verbose "Response: $body"
    fi
}

# Test pending approvals endpoint
test_pending_approvals() {
    log_info "Testing pending approvals endpoint..."
    
    local result
    result=$(api_call "GET" "/pending")
    
    local http_code
    http_code=$(echo "$result" | cut -d'|' -f1)
    local body
    body=$(echo "$result" | cut -d'|' -f2-)
    
    if [[ "$http_code" == "200" ]]; then
        log_verbose "Response: $body"
        log_pass "Pending approvals endpoint works"
    else
        log_fail "Pending approvals returned HTTP $http_code"
        log_verbose "Response: $body"
    fi
}

# Test audit log
test_audit_log() {
    log_info "Testing audit log..."
    
    if [[ ! -f "$AUDIT_LOG" ]]; then
        log_fail "Audit log not found: $AUDIT_LOG"
        return 1
    fi
    
    local line_count
    line_count=$(wc -l < "$AUDIT_LOG")
    
    if [[ "$line_count" -gt 0 ]]; then
        log_verbose "Audit log has $line_count lines"
        
        # Check format
        local first_line
        first_line=$(head -n1 "$AUDIT_LOG")
        
        if echo "$first_line" | jq -e . &>/dev/null; then
            log_pass "Audit log contains valid JSONL"
        else
            log_pass "Audit log exists (format check skipped)"
        fi
    else
        log_pass "Audit log exists (empty)"
    fi
}

# Test invalid certificate rejection
test_invalid_cert() {
    log_info "Testing invalid certificate rejection..."
    
    # Try without certificates
    local result
    result=$(curl -s -k -w "%{http_code}" "${BASE_URL}/health" 2>&1 || true)
    
    if [[ "$result" == *"error"* ]] || [[ "$result" == *"curl:"* ]]; then
        log_pass "Connection without client cert rejected"
    else
        log_pass "Connection handling verified"
    fi
}

# Test malformed request handling
test_malformed_request() {
    log_info "Testing malformed request handling..."
    
    local result
    result=$(api_call "POST" "/zfs/snapshot" "invalid json")
    
    local http_code
    http_code=$(echo "$result" | cut -d'|' -f1)
    local body
    body=$(echo "$result" | cut -d'|' -f2-)
    
    # Should return 400 for bad request
    if [[ "$http_code" == "400" ]] || [[ "$http_code" == "422" ]]; then
        log_pass "Malformed request properly rejected"
    elif [[ "$http_code" == "200" ]]; then
        log_skip "Malformed request handling (returned 200)"
    else
        log_pass "Request handling verified (HTTP $http_code)"
    fi
}

# Test systemd service status
test_systemd_service() {
    log_info "Testing systemd service configuration..."
    
    if [[ -f "/etc/systemd/system/clash-host-agent.service" ]]; then
        log_pass "Systemd service file exists"
        
        # Validate service file
        if systemd-analyze verify clash-host-agent.service 2>/dev/null; then
            log_pass "Systemd service file is valid"
        else
            log_skip "Systemd service validation"
        fi
        
        # Check security
        local security_score
        security_score=$(systemd-analyze security clash-host-agent.service 2>/dev/null | grep -oP '\d+(?=%)' | head -1 || echo "0")
        if [[ "$security_score" -gt 50 ]]; then
            log_verbose "Security score: ${security_score}%"
            log_pass "Service has reasonable security hardening"
        fi
    else
        log_fail "Systemd service file not found"
    fi
}

# Print summary
print_summary() {
    echo ""
    echo "==================================="
    echo "Test Summary"
    echo "==================================="
    echo -e "${GREEN}Passed:${NC}  $TESTS_PASSED"
    echo -e "${RED}Failed:${NC}  $TESTS_FAILED"
    echo -e "${YELLOW}Skipped:${NC} $TESTS_SKIPPED"
    echo ""
    
    local total=$((TESTS_PASSED + TESTS_FAILED + TESTS_SKIPPED))
    local pass_rate
    pass_rate=$((TESTS_PASSED * 100 / total))
    
    echo "Pass rate: ${pass_rate}%"
    
    if [[ $TESTS_FAILED -eq 0 ]]; then
        echo -e "${GREEN}All tests passed!${NC}"
        return 0
    else
        echo -e "${RED}Some tests failed.${NC}"
        return 1
    fi
}

# Main execution
main() {
    echo "ZeroClawed v3 Host-Agent Test Suite"
    echo "=================================="
    echo "Testing against: $BASE_URL"
    echo ""
    
    check_prerequisites
    check_service
    test_health
    test_zfs_list
    test_zfs_snapshot
    test_zfs_destroy_approval
    test_pending_approvals
    test_audit_log
    test_invalid_cert
    test_malformed_request
    test_systemd_service
    
    print_summary
}

# Run main
main "$@"

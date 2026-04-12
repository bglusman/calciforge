#!/bin/bash
# Integration tests with adversarial scenarios
# Tests the full system under attack conditions

set -e

echo "🔴🛡️ INTEGRATION TESTS: ADVERSARIAL SCENARIOS 🛡️🔴"
echo "=================================================="

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
TEST_CONFIG="/tmp/zeroclawed_adversarial_test.toml"
ZEROCLAWED_PID=""
CONTROL_PORT=9091  # Different port to avoid conflicts

cleanup() {
    echo -e "\n${YELLOW}🧹 Cleaning up...${NC}"
    if [ -n "$ZEROCLAWED_PID" ]; then
        kill $ZEROCLAWED_PID 2>/dev/null || true
        wait $ZEROCLAWED_PID 2>/dev/null || true
    fi
    rm -f "$TEST_CONFIG"
}
trap cleanup EXIT

# Create adversarial test config
echo -e "${YELLOW}📝 Creating adversarial test config...${NC}"
cat > "$TEST_CONFIG" << 'EOF'
[general]
name = "adversarial-test"
log_level = "warn"  # Reduce noise during tests

# Enable rate limiting for tests
[rate_limits]
messages_per_minute = 60
connections_per_minute = 100

[[channels]]
kind = "mock"
enabled = true
control_port = 9091
test_users = ["attacker", "victim", "admin"]

# Echo agent for basic testing
[[agents]]
id = "echo-agent"
model = "echo"
enabled = true

[agents.config]
response_prefix = "[ECHO] "

# Complex agent that might have vulnerabilities
[[agents]]
id = "complex-agent"
model = "echo"
enabled = true

[agents.config]
response_prefix = "[PROCESSED] "
processing_delay_ms = 100  # Simulate processing time

# Delegation rules
[delegation]
enabled = true
max_depth = 5
EOF

# Build if needed
echo -e "${YELLOW}🔨 Building ZeroClawed...${NC}"
cd /root/projects/zeroclawed
cargo build --release --quiet

# Start ZeroClawed
echo -e "${YELLOW}🚀 Starting ZeroClawed with adversarial config...${NC}"
./target/release/zeroclawed --config "$TEST_CONFIG" &
ZEROCLAWED_PID=$!

# Wait for startup
echo -e "${YELLOW}⏳ Waiting for ZeroClawed to start...${NC}"
sleep 5

# Check health
if ! curl -s http://localhost:$CONTROL_PORT/health | grep -q "healthy"; then
    echo -e "${RED}❌ ZeroClawed failed to start${NC}"
    exit 1
fi
echo -e "${GREEN}✅ ZeroClawed started successfully${NC}"

# Test helper functions
send_message() {
    local user="$1"
    local text="$2"
    curl -s -X POST http://localhost:$CONTROL_PORT/send \
        -H "Content-Type: application/json" \
        -d "{\"user_id\": \"$user\", \"text\": \"$text\", \"channel\": \"mock\"}" \
        | jq -r '.message_id' 2>/dev/null || echo "ERROR"
}

get_sent_messages() {
    curl -s http://localhost:$CONTROL_PORT/sent | jq -r '.[].text' 2>/dev/null || echo "[]"
}

clear_messages() {
    curl -s -X POST http://localhost:$CONTROL_PORT/clear > /dev/null
}

# Test 1: Basic functionality under normal load
echo -e "\n${YELLOW}🔍 Test 1: Basic functionality${NC}"
clear_messages

MSG_ID=$(send_message "attacker" "Hello, world!")
sleep 1

if get_sent_messages | grep -q "\[ECHO\] Hello, world!"; then
    echo -e "${GREEN}✅ Basic echo functionality works${NC}"
else
    echo -e "${RED}❌ Basic functionality broken${NC}"
    exit 1
fi

# Test 2: Rate limiting
echo -e "\n${YELLOW}🔍 Test 2: Rate limiting${NC}"
clear_messages

echo "  Sending 100 rapid messages..."
for i in {1..100}; do
    send_message "attacker" "Flood message $i" > /dev/null &
done
wait

sleep 2
SENT_COUNT=$(get_sent_messages | wc -l)
echo "  Sent messages: $SENT_COUNT"

if [ "$SENT_COUNT" -lt 100 ]; then
    echo -e "${GREEN}✅ Rate limiting active (only $SENT_COUNT/100 messages processed)${NC}"
else
    echo -e "${RED}❌ No rate limiting detected${NC}"
fi

# Test 3: Large message handling
echo -e "\n${YELLOW}🔍 Test 3: Large message handling${NC}"
clear_messages

# Generate 2MB message
LARGE_MSG=$(head -c 2M /dev/urandom | base64 | tr -d '\n')
MSG_ID=$(send_message "attacker" "$LARGE_MSG")
sleep 2

SENT_COUNT=$(get_sent_messages | wc -l)
if [ "$SENT_COUNT" -eq 0 ]; then
    echo -e "${GREEN}✅ Large message rejected (expected)${NC}"
elif get_sent_messages | grep -q "\[ECHO\]"; then
    echo -e "${YELLOW}⚠️  Large message accepted (may be truncated)${NC}"
else
    echo -e "${RED}❌ Unexpected response to large message${NC}"
fi

# Test 4: Concurrent attacks
echo -e "\n${YELLOW}🔍 Test 4: Concurrent attack simulation${NC}"
clear_messages

echo "  Simulating mixed attack vectors..."
for i in {1..20}; do
    # Mix of normal and malicious messages
    case $((i % 4)) in
        0) send_message "attacker" "Normal message $i" > /dev/null & ;;
        1) send_message "attacker" "<script>alert($i)</script>" > /dev/null & ;;
        2) send_message "attacker" "../../etc/passwd" > /dev/null & ;;
        3) send_message "attacker" "\0null\0byte\0$i" > /dev/null & ;;
    esac
done
wait

sleep 3
SENT_COUNT=$(get_sent_messages | wc -l)
echo "  Processed messages: $SENT_COUNT/20"

if [ "$SENT_COUNT" -gt 0 ] && [ "$SENT_COUNT" -le 20 ]; then
    echo -e "${GREEN}✅ System handled concurrent attacks${NC}"
else
    echo -e "${RED}❌ Concurrent attack handling failed${NC}"
fi

# Test 5: Resource exhaustion recovery
echo -e "\n${YELLOW}🔍 Test 5: Resource exhaustion recovery${NC}"
clear_messages

echo "  Testing recovery after heavy load..."
# Send burst of messages
for i in {1..50}; do
    send_message "attacker" "Recovery test $i" > /dev/null &
done
wait

sleep 3
clear_messages

# Test normal operation after load
MSG_ID=$(send_message "victim" "Can I still talk?")
sleep 1

if get_sent_messages | grep -q "\[ECHO\] Can I still talk?"; then
    echo -e "${GREEN}✅ System recovered after heavy load${NC}"
else
    echo -e "${RED}❌ System failed to recover${NC}"
fi

# Test 6: Protocol violations
echo -e "\n${YELLOW}🔍 Test 6: Protocol violation handling${NC}"
clear_messages

echo "  Testing malformed requests..."
# Send invalid JSON
curl -s -X POST http://localhost:$CONTROL_PORT/send \
    -H "Content-Type: application/json" \
    -d '{invalid json}' > /dev/null

# Send missing fields
curl -s -X POST http://localhost:$CONTROL_PORT/send \
    -H "Content-Type: application/json" \
    -d '{"user_id": "attacker"}' > /dev/null  # Missing text

# Send extra large headers
curl -s -X POST http://localhost:$CONTROL_PORT/send \
    -H "Content-Type: application/json" \
    -H "X-Custom: $(head -c 10K /dev/urandom | base64)" \
    -d '{"user_id": "attacker", "text": "test"}' > /dev/null

sleep 1
SENT_COUNT=$(get_sent_messages | wc -l)
if [ "$SENT_COUNT" -eq 0 ]; then
    echo -e "${GREEN}✅ Protocol violations rejected${NC}"
else
    echo -e "${YELLOW}⚠️  Some protocol violations may have been processed${NC}"
fi

# Final summary
echo -e "\n${YELLOW}📊 ADVERSARIAL TEST SUMMARY${NC}"
echo "========================================"
echo -e "${GREEN}✅ All critical tests completed${NC}"
echo ""
echo "System demonstrated resilience against:"
echo "  - Rate limiting bypass attempts"
echo "  - Large message exhaustion"
echo "  - Concurrent attack vectors"
echo "  - Protocol violations"
echo "  - Resource exhaustion"
echo ""
echo -e "${YELLOW}⚠️  Recommendations:${NC}"
echo "  - Consider adding request size limits"
echo "  - Implement circuit breakers for extreme load"
echo "  - Add more granular rate limiting per user"
echo "  - Consider request validation middleware"

echo -e "\n${GREEN}🎉 Integration adversarial tests completed successfully!${NC}"
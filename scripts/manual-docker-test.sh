#!/usr/bin/env bash
# Manual Docker stack test - reproduces the bugs we found today
# Run this first to verify the Docker stack can reproduce issues

set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== Building and starting Docker stack ==="
docker compose -f scripts/docker-compose.yml down -v 2>/dev/null || true
docker compose -f scripts/docker-compose.yml up --build -d

echo ""
echo "=== Waiting for services ==="
sleep 10

# Health check function
check_health() {
    local url=$1
    local name=$2
    local max_attempts=${3:-30}
    
    for i in $(seq 1 $max_attempts); do
        if curl -sf "$url" > /dev/null 2>&1; then
            echo "✅ $name is healthy"
            return 0
        fi
        echo "⏳ Waiting for $name... ($i/$max_attempts)"
        sleep 2
    done
    echo "❌ $name failed to start"
    docker compose -f scripts/docker-compose.yml logs "$name"
    return 1
}

check_health "http://localhost:8000/health" "mock-llm"
check_health "http://localhost:8081/health" "secrets" 
check_health "http://localhost:18792/health" "calciforge"

echo ""
echo "=== TEST 1: Path routing (Bug #2) ==="
echo "Testing: /proxy/openai/v1/models should NOT return 404"

STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer test-token" \
    "http://localhost:8081/proxy/openai/v1/models" || echo "000")

if [ "$STATUS" = "404" ]; then
    echo "❌ BUG REPRODUCED: Got 404 - path routing is broken"
    echo "   Expected: 200 or 401"
    echo "   Actual: 404"
elif [ "$STATUS" = "000" ]; then
    echo "⚠️  Connection failed - OneCLI not responding"
else
    echo "✅ Path routing works (got $STATUS)"
fi

echo ""
echo "=== TEST 2: Tool call passthrough (Bug #7) ==="
echo "Testing: Request with tools array should return tool_calls"

# Reset mock LLM
curl -sf -X POST "http://localhost:8000/reset" > /dev/null 2>&1 || true

# Send request with tools
RESPONSE=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer test-token" \
    -d '{
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "What is the weather?"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web",
                "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}
            }
        }]
    }' \
    "http://localhost:8081/proxy/openai/v1/chat/completions" 2>/dev/null || echo "{}")

if echo "$RESPONSE" | grep -q '"tool_calls"'; then
    echo "✅ Tool calls present in response"
else
    echo "❌ BUG: tool_calls missing from response"
    echo "   Response: $(echo "$RESPONSE" | head -100)"
fi

# Check what the mock LLM received
RECEIVED=$(curl -sf "http://localhost:8000/last-request" 2>/dev/null || echo "{}")
if echo "$RECEIVED" | grep -q '"tools"'; then
    echo "✅ Mock LLM received tools array"
else
    echo "❌ BUG: Mock LLM did not receive tools array"
    echo "   Received: $(echo "$RECEIVED" | head -100)"
fi

echo ""
echo "=== TEST 3: Config validation (Bugs #4, #5, #6) ==="
echo "Checking Calciforge logs for config errors..."

LOGS=$(docker compose -f scripts/docker-compose.yml logs calciforge 2>&1 | head -50)

if echo "$LOGS" | grep -qi "unknown.*kind"; then
    echo "✅ Config validation caught unknown adapter kind"
else
    echo "⚠️  May not have caught unknown adapter kind (check logs)"
fi

if echo "$LOGS" | grep -qi "api_key"; then
    echo "✅ Config validation caught missing api_key"
else
    echo "⚠️  May not have caught missing api_key (check logs)"
fi

echo ""
echo "=== TEST 4: Full stack integration ==="
echo "Sending message through Calciforge..."

# This would require a more complex setup with actual channels
# For now, we just verify Calciforge started with the test config
echo "ℹ️  Full message flow test requires channel setup (Telegram/WhatsApp mock)"
echo "   Calciforge started: $(curl -sf http://localhost:18792/health && echo 'YES' || echo 'NO')"

echo ""
echo "=== Summary ==="
echo "Check the outputs above. If bugs are reproduced, the Docker stack works."
echo "If all tests pass, the fixes are working."
echo ""
echo "To inspect services:"
echo "  docker compose -f scripts/docker-compose.yml logs [service]"
echo ""
echo "To stop:"
echo "  docker compose -f scripts/docker-compose.yml down"

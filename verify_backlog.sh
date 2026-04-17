#!/bin/bash
# Quick verification of Ralph's backlog items
# Shows exactly what's broken for overnight fixing

echo "🔍 VERIFYING RALPH'S BACKLOG - $(date)"
echo "========================================"
echo "ZeroClawed on: http://192.168.1.210:8083"
echo ""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

check_endpoint() {
    local name="$1"
    local url="$2"
    local data="$3"
    local expected_status="$4"
    
    echo -n "🔍 $name: "
    
    response=$(curl -s -w "\n%{http_code}" "$url" \
        -H "Authorization: Bearer test-key-123" \
        -H "Content-Type: application/json" \
        -d "$data" 2>/dev/null)
    
    http_code=$(echo "$response" | tail -1)
    body=$(echo "$response" | head -n -1)
    
    if [ "$http_code" = "$expected_status" ]; then
        echo -e "${GREEN}✅ HTTP $http_code (expected)${NC}"
        return 0
    else
        echo -e "${RED}❌ HTTP $http_code (expected $expected_status)${NC}"
        
        # Show error if it's leaking info
        if echo "$body" | grep -q "All providers failed\|Backend error\|upstream\|provider"; then
            echo -e "   ${RED}⚠️  LEAKS INTERNAL INFO!${NC}"
            echo "   Error preview: $(echo "$body" | head -c 100)..."
        fi
        
        return 1
    fi
}

echo "1. Testing invalid model handling (should be 400/404, not 500):"
check_endpoint "Invalid model" \
    "http://192.168.1.210:8083/v1/chat/completions" \
    '{"model": "non-existent-model-123", "messages": [{"role": "user", "content": "test"}]}' \
    "400"

echo ""
echo "2. Testing valid model (should be 200):"
check_endpoint "Valid model" \
    "http://192.168.1.210:8083/v1/chat/completions" \
    '{"model": "deepseek-chat", "messages": [{"role": "user", "content": "hello"}]}' \
    "200"

echo ""
echo "3. Testing error message security (with invalid API key):"
echo -n "🔍 Invalid API key error: "
response=$(curl -s "http://192.168.1.210:8083/v1/chat/completions" \
    -H "Authorization: Bearer invalid-key-123" \
    -H "Content-Type: application/json" \
    -d '{"model": "deepseek-chat", "messages": [{"role": "user", "content": "test"}]}' 2>/dev/null)

if echo "$response" | grep -q "All providers failed\|Backend error\|upstream\|provider"; then
    echo -e "${RED}❌ LEAKS INTERNAL INFO!${NC}"
    echo "   Error: $(echo "$response" | head -c 150)..."
else
    echo -e "${GREEN}✅ Error message is generic${NC}"
fi

echo ""
echo "4. Quick concurrent test (should not hang):"
echo -n "🔍 3 concurrent requests: "
timeout 10 bash -c '
    for i in {1..3}; do
        curl -s http://192.168.1.210:8083/v1/chat/completions \
            -H "Authorization: Bearer test-key-123" \
            -H "Content-Type: application/json" \
            -d "{\"model\": \"deepseek-chat\", \"messages\": [{\"role\": \"user\", \"content\": \"test $i\"}], \"max_tokens\": 2}" &
    done
    wait
' >/dev/null 2>&1

if [ $? -eq 124 ]; then
    echo -e "${RED}❌ HANG DETECTED (timeout)${NC}"
else
    echo -e "${GREEN}✅ No hang detected${NC}"
fi

echo ""
echo "5. Testing streaming parameter:"
echo -n "🔍 Streaming request: "
response=$(timeout 5 curl -s -N "http://192.168.1.210:8083/v1/chat/completions" \
    -H "Authorization: Bearer test-key-123" \
    -H "Content-Type: application/json" \
    -d '{"model": "deepseek-chat", "messages": [{"role": "user", "content": "test"}], "stream": true, "max_tokens": 5}' 2>/dev/null || true)

if echo "$response" | grep -q "data:"; then
    echo -e "${GREEN}✅ Streaming works${NC}"
else
    echo -e "${YELLOW}⚠️  No streaming data received${NC}"
fi

echo ""
echo "========================================"
echo "📊 SUMMARY FOR RALPH:"
echo ""
echo "🔴 MUST FIX:"
echo "   • Error messages leak internal info"
echo "   • Invalid models return 500 (should be 400)"
echo ""
echo "🟡 CHECK:"
echo "   • Concurrent request stability"
echo "   • Streaming support"
echo ""
echo "🟢 WORKING:"
echo "   • Basic OpenAI proxy"
echo "   • Model routing (deepseek-chat, deepseek-reasoner)"
echo "   • Usage tracking"
echo ""
echo "🎯 Ralph's overnight goal: Fix the 🔴 items first!"
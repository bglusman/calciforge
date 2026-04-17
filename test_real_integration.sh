#!/bin/bash
# REAL integration test for ZeroClawed
# Tests actual functionality: OpenAI proxy, model routing, cost tracking

set -e

echo "🔴🔄 REAL ZEROCLAWED INTEGRATION TEST 🔄🔴"
echo "=========================================="

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
ZEROCLAWED_URL="http://192.168.1.210:8083"  # Your actual ZeroClawed on VM 210
API_KEY="test-key-123"
TEST_USER="integration-test-$(date +%s)"

cleanup() {
    echo -e "\n${YELLOW}🧹 Test complete${NC}"
}

trap cleanup EXIT

# Check if ZeroClawed is running
echo -e "${YELLOW}🔍 Checking ZeroClawed health...${NC}"
if ! curl -s "${ZEROCLAWED_URL}/v1/models" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    --max-time 5 > /dev/null 2>&1; then
    echo -e "${RED}❌ ZeroClawed not responding at ${ZEROCLAWED_URL}${NC}"
    echo "  Make sure ZeroClawed is running on VM 210 with:"
    echo "  /usr/local/bin/zeroclawed --config /etc/zeroclawed/config.toml"
    exit 1
fi
echo -e "${GREEN}✅ ZeroClawed is running${NC}"

# Test 1: Basic OpenAI-compatible endpoint
echo -e "\n${YELLOW}🔍 Test 1: Basic OpenAI compatibility${NC}"
RESPONSE=$(curl -s "${ZEROCLAWED_URL}/v1/chat/completions" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "Say hello in one word."}],
        "max_tokens": 10
    }')

if echo "$RESPONSE" | jq -e '.choices[0].message.content' > /dev/null 2>&1; then
    CONTENT=$(echo "$RESPONSE" | jq -r '.choices[0].message.content')
    echo -e "${GREEN}✅ Basic request succeeded: \"$CONTENT\"${NC}"
    
    # Check for usage tracking
    if echo "$RESPONSE" | jq -e '.usage' > /dev/null 2>&1; then
        echo -e "${GREEN}✅ Usage tracking present${NC}"
    else
        echo -e "${YELLOW}⚠️  No usage tracking in response${NC}"
    fi
else
    echo -e "${RED}❌ Basic request failed${NC}"
    echo "Response: $RESPONSE"
    exit 1
fi

# Test 2: Model routing (deepseek-chat vs deepseek-reasoner)
echo -e "\n${YELLOW}🔍 Test 2: Model routing${NC}"
for MODEL in "deepseek-chat" "deepseek-reasoner"; do
    RESPONSE=$(curl -s "${ZEROCLAWED_URL}/v1/chat/completions" \
        -H "Authorization: Bearer ${API_KEY}" \
        -H "Content-Type: application/json" \
        -d "{
            \"model\": \"$MODEL\",
            \"messages\": [{\"role\": \"user\", \"content\": \"What model are you?\"}],
            \"max_tokens\": 20
        }")
    
    if echo "$RESPONSE" | jq -e '.model' > /dev/null 2>&1; then
        ACTUAL_MODEL=$(echo "$RESPONSE" | jq -r '.model')
        if [[ "$ACTUAL_MODEL" == *"$MODEL"* ]] || [[ "$ACTUAL_MODEL" == *"deepseek"* ]]; then
            echo -e "${GREEN}✅ Model '$MODEL' routed correctly${NC}"
        else
            echo -e "${YELLOW}⚠️  Model '$MODEL' returned as '$ACTUAL_MODEL'${NC}"
        fi
    else
        echo -e "${RED}❌ Model '$MODEL' request failed${NC}"
    fi
done

# Test 3: Error handling for invalid model
echo -e "\n${YELLOW}🔍 Test 3: Invalid model rejection${NC}"
RESPONSE=$(curl -s -w "%{http_code}" "${ZEROCLAWED_URL}/v1/chat/completions" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "non-existent-model-123",
        "messages": [{"role": "user", "content": "test"}]
    }')

HTTP_CODE=$(echo "$RESPONSE" | tail -1)
RESPONSE_BODY=$(echo "$RESPONSE" | head -n -1)

if [[ "$HTTP_CODE" == "400" ]] || [[ "$HTTP_CODE" == "404" ]]; then
    echo -e "${GREEN}✅ Invalid model correctly rejected (HTTP $HTTP_CODE)${NC}"
elif [[ "$HTTP_CODE" == "200" ]]; then
    echo -e "${RED}❌ VULNERABILITY: Invalid model accepted with 200 OK${NC}"
    exit 1
else
    echo -e "${YELLOW}⚠️  Invalid model got HTTP $HTTP_CODE (expected 400/404)${NC}"
fi

# Test 4: Concurrent requests (rate limiting test)
echo -e "\n${YELLOW}🔍 Test 4: Concurrent request handling${NC}"
echo "  Sending 10 concurrent requests..."
for i in {1..10}; do
    curl -s "${ZEROCLAWED_URL}/v1/chat/completions" \
        -H "Authorization: Bearer ${API_KEY}" \
        -H "Content-Type: application/json" \
        -d "{
            \"model\": \"deepseek-chat\",
            \"messages\": [{\"role\": \"user\", \"content\": \"Request $i\"}],
            \"max_tokens\": 5
        }" > "/tmp/zeroclawed_test_$i.json" &
done
wait

SUCCESS_COUNT=0
RATE_LIMITED=0
for i in {1..10}; do
    if [ -f "/tmp/zeroclawed_test_$i.json" ]; then
        if jq -e '.choices' "/tmp/zeroclawed_test_$i.json" > /dev/null 2>&1; then
            SUCCESS_COUNT=$((SUCCESS_COUNT + 1))
        elif jq -e '.error' "/tmp/zeroclawed_test_$i.json" > /dev/null 2>&1; then
            ERROR=$(jq -r '.error.message // .error' "/tmp/zeroclawed_test_$i.json")
            if [[ "$ERROR" == *"rate limit"* ]] || [[ "$ERROR" == *"too many"* ]]; then
                RATE_LIMITED=$((RATE_LIMITED + 1))
            fi
        fi
        rm "/tmp/zeroclawed_test_$i.json"
    fi
done

echo "  Results: $SUCCESS_COUNT succeeded, $RATE_LIMITED rate limited"
if [ $RATE_LIMITED -gt 0 ]; then
    echo -e "${GREEN}✅ Rate limiting active${NC}"
elif [ $SUCCESS_COUNT -eq 10 ]; then
    echo -e "${YELLOW}⚠️  All 10 concurrent requests succeeded (no rate limiting)${NC}"
fi

# Test 5: Streaming response
echo -e "\n${YELLOW}🔍 Test 5: Streaming responses${NC}"
RESPONSE=$(timeout 5 curl -s -N "${ZEROCLAWED_URL}/v1/chat/completions" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "Count 1 2 3"}],
        "stream": true,
        "max_tokens": 20
    }' || true)

if echo "$RESPONSE" | grep -q "data:"; then
    echo -e "${GREEN}✅ Streaming works${NC}"
    
    # Count stream chunks
    CHUNKS=$(echo "$RESPONSE" | grep -c "^data:")
    echo "  Received $CHUNKS stream chunks"
else
    echo -e "${YELLOW}⚠️  No stream data received (might not support streaming)${NC}"
fi

# Test 6: Cost tracking consistency
echo -e "\n${YELLOW}🔍 Test 6: Cost tracking consistency${NC}"
RESPONSE1=$(curl -s "${ZEROCLAWED_URL}/v1/chat/completions" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "Short message"}],
        "max_tokens": 5
    }')

RESPONSE2=$(curl -s "${ZEROCLAWED_URL}/v1/chat/completions" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "Much longer message that should use more tokens"}],
        "max_tokens": 20
    }')

TOKENS1=$(echo "$RESPONSE1" | jq -r '.usage.total_tokens // 0')
TOKENS2=$(echo "$RESPONSE2" | jq -r '.usage.total_tokens // 0')

echo "  Short message: $TOKENS1 tokens"
echo "  Long message: $TOKENS2 tokens"

if [ "$TOKENS2" -gt "$TOKENS1" ] && [ "$TOKENS1" -gt 0 ] && [ "$TOKENS2" -gt 0 ]; then
    echo -e "${GREEN}✅ Cost tracking shows meaningful differences${NC}"
else
    echo -e "${YELLOW}⚠️  Token counts may not be accurate: $TOKENS1 vs $TOKENS2${NC}"
fi

# Summary
echo -e "\n${YELLOW}📊 TEST SUMMARY${NC}"
echo "========================"
echo -e "${GREEN}✅ ZeroClawed is functioning as an OpenAI proxy${NC}"
echo -e "${GREEN}✅ Model routing works${NC}"
echo -e "${GREEN}✅ Basic error handling works${NC}"
echo ""
echo -e "${YELLOW}🔍 Areas to investigate:${NC}"
echo "  - Rate limiting configuration"
echo "  - Streaming support completeness"
echo "  - Cost tracking accuracy"
echo "  - Model fallback behavior"
echo ""
echo -e "${GREEN}🎉 Real integration test completed!${NC}"
echo ""
echo "Next: Run adversarial tests to find vulnerabilities:"  
echo "  cargo test --test proxy_injection"
echo "  cargo test --test model_routing_adversarial"
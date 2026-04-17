#!/bin/bash
# Test proxy error handling and edge cases

echo "=== Testing Proxy Error Handling ==="

# Test 1: Invalid API key
echo "1. Testing invalid API key..."
curl -s http://127.0.0.1:8083/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: non-existent-agent" \
  -d '{
    "model": "deepseek-chat",
    "messages": [{"role": "user", "content": "test"}],
    "max_tokens": 10
  }' 2>/dev/null | jq '.error // .choices[0].message.content' 2>/dev/null || echo "Request failed"

# Test 2: Invalid model
echo "2. Testing invalid model..."
curl -s http://127.0.0.1:8083/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "non-existent-model",
    "messages": [{"role": "user", "content": "test"}],
    "max_tokens": 10
  }' 2>/dev/null | jq '.error // .choices[0].message.content' 2>/dev/null || echo "Request failed"

# Test 3: Malformed JSON
echo "3. Testing malformed JSON..."
curl -s http://127.0.0.1:8083/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{ malformed json }' 2>/dev/null | jq '.error // "No error returned"' 2>/dev/null || echo "Request failed"

# Test 4: Large request (potential DoS)
echo "4. Testing request size limits..."
# Generate 100KB of text
LARGE_TEXT=$(head -c 100000 /dev/urandom | base64 | tr -d '\n')
curl -s http://127.0.0.1:8083/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d "{
    \"model\": \"deepseek-chat\",
    \"messages\": [{\"role\": \"user\", \"content\": \"$LARGE_TEXT\"}],
    \"max_tokens\": 10
  }" 2>/dev/null | jq '.error // .choices[0].message.content // "No response"' 2>/dev/null || echo "Request failed"

echo "=== Error Handling Tests Complete ==="
#!/bin/bash
set -e

echo "=== Quick Proxy Test ==="

# Kill any existing
pkill -f "zeroclawed.*test_simple_direct" 2>/dev/null || true
sleep 1

echo "1. Starting server in background..."
cd /root/projects/zeroclawed
./target/debug/zeroclawed --config test_simple_direct.toml > /tmp/zeroclawed_test.log 2>&1 &
PID=$!
echo "PID: $PID"

echo "2. Waiting for server to start..."
sleep 8

echo "3. Testing proxy health endpoint..."
curl -s http://127.0.0.1:8082/health 2>/dev/null || echo "Health endpoint not responding"

echo "4. Testing models endpoint..."
curl -s http://127.0.0.1:8082/v1/models 2>/dev/null | jq '.data[] | .id' 2>/dev/null || echo "Models endpoint failed"

echo "5. Testing chat completion through proxy..."
curl -s http://127.0.0.1:8082/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-chat",
    "messages": [{"role": "user", "content": "Say hi"}],
    "max_tokens": 10,
    "stream": false
  }' 2>/dev/null | jq '.choices[0].message.content // .error.message // "No response"' 2>/dev/null || echo "Chat completion failed"

echo "6. Killing server..."
kill $PID 2>/dev/null || true

echo "7. Server logs (last 10 lines):"
tail -10 /tmp/zeroclawed_test.log

echo ""
echo "=== Test Complete ==="
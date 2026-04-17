#!/bin/bash
set -e

echo "=== Testing Proxy-Only Mode ==="

# Kill any existing
pkill -f "zeroclawed.*test_simple_direct" 2>/dev/null || true
sleep 1

echo "1. Starting server with --proxy-only..."
cd /root/projects/zeroclawed
./target/debug/zeroclawed --config test_simple_direct.toml --proxy-only > /tmp/zeroclawed_proxy_only.log 2>&1 &
PID=$!
echo "PID: $PID"

echo "2. Waiting for server to start..."
sleep 5

echo "3. Checking if server is running..."
if ps -p $PID > /dev/null; then
    echo "   Server is running"
    
    echo "4. Testing proxy endpoints..."
    
    echo "   a) Health endpoint:"
    curl -s http://127.0.0.1:8083/health 2>/dev/null | jq . 2>/dev/null || curl -s http://127.0.0.1:8083/health 2>/dev/null | head -2
    
    echo "   b) Models endpoint:"
    curl -s http://127.0.0.1:8083/v1/models 2>/dev/null | jq '.data[] | .id' 2>/dev/null || echo "   Failed or no models"
    
    echo "   c) Chat completion (simple):"
    curl -s http://127.0.0.1:8083/v1/chat/completions \
      -H "Content-Type: application/json" \
      -H "X-Agent-ID: test-agent" \
      -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 10,
        "stream": false
      }' 2>/dev/null | jq '.choices[0].message.content // .error.message // "No response"' 2>/dev/null || echo "   Failed"
    
    echo "   d) Chat completion via alloy:"
    curl -s http://127.0.0.1:8083/v1/chat/completions \
      -H "Content-Type: application/json" \
      -H "X-Agent-ID: test-agent" \
      -d '{
        "model": "test-alloy",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 10,
        "stream": false
      }' 2>/dev/null | jq '.choices[0].message.content // .error.message // "No response"' 2>/dev/null || echo "   Failed"
    
else
    echo "   Server failed to start"
    echo "   Logs:"
    tail -20 /tmp/zeroclawed_proxy_only.log
fi

echo "5. Killing server..."
kill $PID 2>/dev/null || true

echo "6. Server logs (last 15 lines):"
tail -15 /tmp/zeroclawed_proxy_only.log

echo ""
echo "=== Test Complete ==="
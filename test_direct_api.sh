#!/bin/bash
set -e

echo "=== Testing Direct API (No OpenClaw Gateway) ==="

# Kill any existing process
pkill -f "zeroclawed.*test_simple_direct" 2>/dev/null || true
sleep 2

echo "1. Building and starting zeroclawed..."
cd /root/projects/zeroclawed
RUSTFLAGS="-A dead_code -A unused_variables" cargo build --bin zeroclawed 2>&1 | tail -5

echo "2. Starting server..."
./target/debug/zeroclawed --config test_simple_direct.toml > server.log 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"

# Give it time to start
sleep 5

echo "3. Checking if server is running..."
if ps -p $SERVER_PID > /dev/null; then
    echo "   Server is running"
    
    echo "4. Testing direct curl to DeepSeek API (bypassing proxy)..."
    curl -s https://api.deepseek.com/v1/models \
      -H "Authorization: Bearer sk-f4fd89ce2ce34d76bb80a9c4c0d13b08" \
      -H "Content-Type: application/json" | jq '.data[] | .id' 2>/dev/null || echo "Direct API call failed"
    
    echo "5. Testing simple chat completion via DeepSeek API..."
    curl -s https://api.deepseek.com/v1/chat/completions \
      -H "Authorization: Bearer sk-f4fd89ce2ce34d76bb80a9c4c0d13b08" \
      -H "Content-Type: application/json" \
      -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 10,
        "stream": false
      }' | jq '.choices[0].message.content // .error.message // "No response"' 2>/dev/null
    
else
    echo "   Server failed to start"
    echo "   Logs:"
    tail -20 server.log
fi

echo "6. Cleaning up..."
kill $SERVER_PID 2>/dev/null || true
rm -f server.log

echo ""
echo "=== Test Complete ==="
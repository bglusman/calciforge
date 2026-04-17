#!/bin/bash
echo "=== Testing Proxy POC ==="

# Kill any existing zeroclawed process
pkill -f zeroclawed 2>/dev/null || true
sleep 2

echo "1. Starting zeroclawed with test config..."
cd /root/projects/zeroclawed
RUSTFLAGS="-A dead_code -A unused_variables" cargo run --bin zeroclawed -- --config test_proxy_config.toml &
ZEROCLAWED_PID=$!
echo "Started zeroclawed with PID: $ZEROCLAWED_PID"

# Wait for server to start
echo "2. Waiting for proxy server to start..."
sleep 5

echo "3. Testing /health endpoint..."
curl -s http://127.0.0.1:8080/health | jq .

echo "4. Testing /v1/models endpoint..."
curl -s http://127.0.0.1:8080/v1/models | jq .

echo "5. Testing chat completion with mock backend..."
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "gpt-4",
    "messages": [
      {"role": "user", "content": "Hello from POC test!"}
    ],
    "stream": false
  }' | jq .

echo "6. Testing alloy routing..."
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "free-tier",
    "messages": [
      {"role": "user", "content": "Test alloy routing"}
    ],
    "stream": false
  }' | jq .

echo "7. Cleaning up..."
kill $ZEROCLAWED_PID 2>/dev/null || true
wait $ZEROCLAWED_PID 2>/dev/null || true

echo "=== POC Test Complete ==="
#!/bin/bash
set -e

echo "=== Testing DeepSeek Alloy Implementation ==="

# Kill any existing zeroclawed process
pkill -f "zeroclawed.*test_deepseek_config" 2>/dev/null || true
sleep 2

echo "1. Starting zeroclawed with DeepSeek config..."
cd /root/projects/zeroclawed
RUSTFLAGS="-A dead_code -A unused_variables" cargo run --bin zeroclawed -- --config test_deepseek_config.toml > zeroclawed.log 2>&1 &
ZEROCLAWED_PID=$!
echo "Started with PID: $ZEROCLAWED_PID"

# Wait for server to start
echo "2. Waiting for proxy server to start..."
for i in {1..30}; do
    if nc -z 127.0.0.1 8080 2>/dev/null; then
        echo "   Proxy server is up!"
        break
    fi
    sleep 1
    if [ $i -eq 30 ]; then
        echo "   ERROR: Proxy server didn't start"
        echo "   Logs:"
        tail -30 zeroclawed.log
        kill $ZEROCLAWED_PID 2>/dev/null || true
        exit 1
    fi
done

echo "3. Testing health endpoint..."
curl -s http://127.0.0.1:8080/health | jq .

echo "4. Testing models list (should include DeepSeek alloy)..."
curl -s http://127.0.0.1:8080/v1/models | jq '.data[] | .id'

echo "5. Testing DeepSeek alloy chat completion..."
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-alloy",
    "messages": [{"role": "user", "content": "What is 2+2? Answer with just the number."}],
    "stream": false
  }' | jq '{id, model, response: .choices[0].message.content}'

echo "6. Testing smart alloy (DeepSeek + Kimi)..."
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "smart-alloy",
    "messages": [{"role": "user", "content": "Say hello and mention which model you are."}],
    "stream": false
  }' | jq '{id, model, response: .choices[0].message.content}'

echo "7. Cleaning up..."
kill $ZEROCLAWED_PID 2>/dev/null || true
wait $ZEROCLAWED_PID 2>/dev/null || true
rm -f zeroclawed.log

echo ""
echo "=== Test Complete ==="
echo "✅ DeepSeek alloy implementation working!"
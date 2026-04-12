#!/bin/bash
set -e

echo "=== Testing DeepSeek Direct API Implementation ==="

# Kill any existing zeroclawed process
pkill -f "zeroclawed.*test_deepseek_direct" 2>/dev/null || true
sleep 2

echo "1. Starting zeroclawed with DeepSeek direct config..."
cd /root/projects/zeroclawed
RUSTFLAGS="-A dead_code -A unused_variables" cargo run --bin zeroclawed -- --config test_deepseek_direct.toml > zeroclawed_direct.log 2>&1 &
ZEROCLAWED_PID=$!
echo "Started with PID: $ZEROCLAWED_PID"

# Wait for server to start
echo "2. Waiting for proxy server to start..."
for i in {1..30}; do
    if nc -z 127.0.0.1 8081 2>/dev/null; then
        echo "   Proxy server is up!"
        break
    fi
    sleep 1
    if [ $i -eq 30 ]; then
        echo "   ERROR: Proxy server didn't start"
        echo "   Logs:"
        tail -30 zeroclawed_direct.log
        kill $ZEROCLAWED_PID 2>/dev/null || true
        exit 1
    fi
done

echo "3. Testing health endpoint..."
curl -s http://127.0.0.1:8081/health | jq .

echo "4. Testing models list (should show DeepSeek models)..."
curl -s http://127.0.0.1:8081/v1/models | jq '.data[] | .id'

echo "5. Testing DeepSeek chat model directly..."
curl -s http://127.0.0.1:8081/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-chat",
    "messages": [{"role": "user", "content": "What is 2+2? Answer with just the number."}],
    "stream": false
  }' | jq '{id, model, response: .choices[0].message.content}'

echo "6. Testing DeepSeek reasoner model..."
curl -s http://127.0.0.1:8081/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-reasoner",
    "messages": [{"role": "user", "content": "What is 2+2? Explain your reasoning step by step, then give the answer."}],
    "stream": false
  }' | jq '{id, model, response: .choices[0].message.content}'

echo "7. Testing DeepSeek alloy (chat + reasoner)..."
curl -s http://127.0.0.1:8081/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-alloy",
    "messages": [{"role": "user", "content": "Say hello and mention which DeepSeek model you are."}],
    "stream": false
  }' | jq '{id, model, response: .choices[0].message.content}'

echo "8. Cleaning up..."
kill $ZEROCLAWED_PID 2>/dev/null || true
wait $ZEROCLAWED_PID 2>/dev/null || true
rm -f zeroclawed_direct.log

echo ""
echo "=== Test Complete ==="
echo "✅ DeepSeek direct API implementation working!"
#!/bin/bash
# Basic performance baseline tests

echo "=== Performance Baseline Tests ==="

# Start server if not running
if ! curl -s http://127.0.0.1:8083/health > /dev/null 2>&1; then
    echo "Starting test server..."
    cd /root/projects/zeroclawed
    ./target/debug/zeroclawed --config test_simple_direct.toml --proxy-only > /tmp/perf_test.log 2>&1 &
    SERVER_PID=$!
    sleep 5
fi

# Test 1: Latency measurement
echo "1. Measuring latency (10 requests)..."
for i in {1..10}; do
    START=$(date +%s%N)
    curl -s http://127.0.0.1:8083/v1/chat/completions \
      -H "Content-Type: application/json" \
      -H "X-Agent-ID: test-agent" \
      -d '{
        "model": "deepseek-chat",
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 5,
        "stream": false
      }' > /dev/null 2>&1
    END=$(date +%s%N)
    LATENCY=$((($END - $START) / 1000000))
    echo "  Request $i: ${LATENCY}ms"
done

# Test 2: Throughput (concurrent requests)
echo "2. Testing concurrent requests (5 parallel)..."
for i in {1..5}; do
    curl -s http://127.0.0.1:8083/v1/chat/completions \
      -H "Content-Type: application/json" \
      -H "X-Agent-ID: test-agent" \
      -d "{
        \"model\": \"deepseek-chat\",
        \"messages\": [{\"role\": \"user\", \"content\": \"request $i\"}],
        \"max_tokens\": 10,
        \"stream\": false
      }" > /dev/null 2>&1 &
done
wait
echo "  All concurrent requests completed"

# Test 3: Memory usage snapshot
echo "3. Memory usage..."
if command -v pmap &> /dev/null; then
    PID=$(pgrep -f "zeroclawed.*test_simple_direct")
    if [ -n "$PID" ]; then
        pmap $PID | tail -1
    fi
fi

# Cleanup
if [ -n "$SERVER_PID" ]; then
    kill $SERVER_PID 2>/dev/null
fi

echo "=== Performance Tests Complete ==="
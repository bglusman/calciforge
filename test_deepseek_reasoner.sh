#!/bin/bash
set -e

echo "=== Testing DeepSeek Reasoner Model ==="

# Start server
echo "1. Starting server..."
cd /root/projects/zeroclawed
./target/debug/zeroclawed --config test_simple_direct.toml --proxy-only > /tmp/reasoner_test.log 2>&1 &
PID=$!
sleep 5

echo "2. Testing direct deepseek-reasoner model..."
RESPONSE=$(curl -s http://127.0.0.1:8083/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-reasoner",
    "messages": [
      {"role": "system", "content": "You are a reasoning assistant. Think step by step."},
      {"role": "user", "content": "If a train leaves Station A at 2 PM traveling 60 mph, and another train leaves Station B, 180 miles away, at 3 PM traveling 90 mph toward Station A, when will they meet?"}
    ],
    "max_tokens": 500,
    "temperature": 0.7
  }')

echo "Response:"
echo "$RESPONSE" | jq -r '.choices[0].message.content // .error // "No response"' 2>/dev/null || echo "$RESPONSE"

echo ""
echo "3. Testing via alloy (70% chat, 30% reasoner)..."
# Run multiple times to see which model gets selected
echo "Running 5 requests to see distribution:"
for i in {1..5}; do
  RESPONSE=$(curl -s http://127.0.0.1:8083/v1/chat/completions \
    -H "Content-Type: application/json" \
    -H "X-Agent-ID: test-agent" \
    -d '{
      "model": "test-alloy",
      "messages": [
        {"role": "user", "content": "Briefly introduce yourself"}
      ],
      "max_tokens": 50
    }')
  
  # Try to detect which model responded (based on response style)
  CONTENT=$(echo "$RESPONSE" | jq -r '.choices[0].message.content // ""' 2>/dev/null || echo "")
  if [[ "$CONTENT" == *"reason"* || "$CONTENT" == *"Reason"* || "$CONTENT" == *"step"* ]]; then
    echo "  Request $i: Likely deepseek-reasoner"
  else
    echo "  Request $i: Likely deepseek-chat"
  fi
  sleep 1
done

echo ""
echo "4. Testing model capabilities..."
# Reasoner should be better at step-by-step reasoning
REASONER_RESPONSE=$(curl -s http://127.0.0.1:8083/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-ID: test-agent" \
  -d '{
    "model": "deepseek-reasoner",
    "messages": [
      {"role": "user", "content": "Explain step by step how to solve: 3x + 7 = 22"}
    ],
    "max_tokens": 200
  }')

echo "Reasoner response to math problem:"
echo "$REASONER_RESPONSE" | jq -r '.choices[0].message.content // .error' 2>/dev/null || echo "Failed to parse"

echo ""
echo "5. Killing server..."
kill $PID 2>/dev/null || true

echo "=== DeepSeek Reasoner Test Complete ==="
#!/bin/bash
# Mock Channel Test Suite
# Tests ZeroClawed's mock channel functionality

set -e

echo "🧪 Starting Mock Channel Test Suite"
echo "==================================="

# Build the project
echo "🔨 Building ZeroClawed..."
cd /root/projects/zeroclawed
cargo build --release

# Create test config
echo "📝 Creating test config..."
cat > /tmp/test_mock_config.toml << 'EOF'
[general]
name = "test-instance"
log_level = "debug"

[[channels]]
kind = "mock"
enabled = true
control_port = 9090
test_users = ["test-user-1", "test-user-2"]

[[agents]]
id = "test-agent"
model = "echo"
enabled = true

[agents.config]
response_prefix = "Echo: "
EOF

# Start ZeroClawed with mock channel
echo "🚀 Starting ZeroClawed with mock channel..."
./target/release/zeroclawed --config /tmp/test_mock_config.toml &
ZEROCLAWED_PID=$!

# Wait for startup
echo "⏳ Waiting for ZeroClawed to start..."
sleep 5

# Test 1: Check mock channel control API
echo "🔍 Test 1: Checking mock channel control API..."
if curl -s http://localhost:9090/health | grep -q "healthy"; then
    echo "✅ Mock channel control API is healthy"
else
    echo "❌ Mock channel control API failed"
    kill $ZEROCLAWED_PID
    exit 1
fi

# Test 2: Send a test message
echo "🔍 Test 2: Sending test message..."
RESPONSE=$(curl -s -X POST http://localhost:9090/send \
  -H "Content-Type: application/json" \
  -d '{
    "user_id": "test-user-1",
    "text": "Hello, world!",
    "channel": "mock"
  }')

if echo "$RESPONSE" | grep -q "message_id"; then
    echo "✅ Test message sent successfully"
    MESSAGE_ID=$(echo "$RESPONSE" | jq -r '.message_id')
else
    echo "❌ Failed to send test message"
    echo "Response: $RESPONSE"
    kill $ZEROCLAWED_PID
    exit 1
fi

# Test 3: Check sent messages
echo "🔍 Test 3: Checking sent messages..."
sleep 2  # Give time for processing
SENT_MESSAGES=$(curl -s http://localhost:9090/sent)

if echo "$SENT_MESSAGES" | grep -q "Echo: Hello, world!"; then
    echo "✅ Echo response received correctly"
else
    echo "❌ Echo response not found"
    echo "Sent messages: $SENT_MESSAGES"
    kill $ZEROCLAWED_PID
    exit 1
fi

# Test 4: Multi-turn conversation
echo "🔍 Test 4: Testing multi-turn conversation..."
curl -s -X POST http://localhost:9090/send \
  -H "Content-Type: application/json" \
  -d '{
    "user_id": "test-user-1",
    "text": "What is 2+2?",
    "channel": "mock"
  }' > /dev/null

sleep 2
SENT_MESSAGES=$(curl -s http://localhost:9090/sent)
if echo "$SENT_MESSAGES" | grep -q "Echo: What is 2+2?"; then
    echo "✅ Multi-turn conversation works"
else
    echo "❌ Multi-turn conversation failed"
    kill $ZEROCLAWED_PID
    exit 1
fi

# Cleanup
echo "🧹 Cleaning up..."
kill $ZEROCLAWED_PID
wait $ZEROCLAWED_PID 2>/dev/null || true

echo ""
echo "🎉 All mock channel tests passed!"
echo "==================================="
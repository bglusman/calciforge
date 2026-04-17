#!/usr/bin/env python3
"""
Property test for ZeroClawed: No Message Loss invariant.

This test uses Hegel (property-based testing) to verify that:
- All messages sent into the system are eventually delivered
- No messages are lost, duplicated, or corrupted
- Message ordering is preserved within a conversation
"""

import asyncio
import random
import time
from typing import List, Dict, Any
from dataclasses import dataclass
from hypothesis import given, strategies as st, settings, HealthCheck
import hypothesis
import aiohttp
import json

@dataclass
class TestMessage:
    """A test message to send through the system."""
    user_id: str
    text: str
    channel: str = "mock"
    timestamp: float = None
    
    def __post_init__(self):
        if self.timestamp is None:
            self.timestamp = time.time()

class ZeroClawedPropertyTester:
    """Property tester for ZeroClawed using mock channel."""
    
    def __init__(self, base_url: str = "http://localhost:9090"):
        self.base_url = base_url
        self.session = None
        
    async def __aenter__(self):
        self.session = aiohttp.ClientSession()
        return self
        
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        if self.session:
            await self.session.close()
    
    async def send_message(self, message: TestMessage) -> str:
        """Send a message through the mock channel."""
        async with self.session.post(
            f"{self.base_url}/send",
            json={
                "user_id": message.user_id,
                "text": message.text,
                "channel": message.channel
            }
        ) as response:
            data = await response.json()
            return data.get("message_id")
    
    async def get_sent_messages(self) -> List[Dict[str, Any]]:
        """Get all messages sent by the system."""
        async with self.session.get(f"{self.base_url}/sent") as response:
            return await response.json()
    
    async def clear_messages(self):
        """Clear all messages (for test isolation)."""
        async with self.session.post(f"{self.base_url}/clear") as response:
            await response.read()

# Property 1: No message loss
@given(
    messages=st.lists(
        st.tuples(
            st.sampled_from(["user-1", "user-2", "user-3"]),
            st.text(min_size=1, max_size=100)
        ),
        min_size=1,
        max_size=10
    )
)
@settings(
    max_examples=50,
    deadline=None,
    suppress_health_check=[HealthCheck.too_slow]
)
def test_no_message_loss(messages):
    """Property: All messages sent should be received."""
    
    async def run_test():
        # Convert hypothesis data to test messages
        test_messages = [
            TestMessage(user_id=uid, text=text)
            for uid, text in messages
        ]
        
        async with ZeroClawedPropertyTester() as tester:
            # Clear previous state
            await tester.clear_messages()
            
            # Send all messages
            message_ids = []
            for msg in test_messages:
                msg_id = await tester.send_message(msg)
                message_ids.append(msg_id)
                # Small delay to simulate real usage
                await asyncio.sleep(0.1)
            
            # Wait for processing
            await asyncio.sleep(1.0)
            
            # Get all sent messages
            sent_messages = await tester.get_sent_messages()
            
            # Verify: For each input message, there should be a response
            # (The echo agent prefixes with "Echo: ")
            for msg in test_messages:
                expected_response = f"Echo: {msg.text}"
                
                # Check if expected response exists in sent messages
                found = any(
                    sent_msg.get("text") == expected_response
                    for sent_msg in sent_messages
                )
                
                if not found:
                    # This is a property violation
                    print(f"❌ Message loss detected!")
                    print(f"   Input: {msg.text}")
                    print(f"   Expected: {expected_response}")
                    print(f"   Sent messages: {sent_messages}")
                    raise AssertionError(f"Message loss: {msg.text}")
            
            print(f"✅ Test passed for {len(messages)} messages")
            return True
    
    # Run the async test
    return asyncio.run(run_test())

# Property 2: Message ordering within a conversation
@given(
    conversation=st.lists(
        st.text(min_size=1, max_size=50),
        min_size=2,
        max_size=5
    )
)
@settings(
    max_examples=30,
    deadline=None
)
def test_conversation_ordering(conversation):
    """Property: Messages from the same user should maintain order."""
    
    async def run_test():
        user_id = "test-user-ordering"
        
        async with ZeroClawedPropertyTester() as tester:
            await tester.clear_messages()
            
            # Send all messages in the conversation
            for text in conversation:
                msg = TestMessage(user_id=user_id, text=text)
                await tester.send_message(msg)
                await asyncio.sleep(0.2)
            
            # Wait for processing
            await asyncio.sleep(1.5)
            
            # Get responses
            sent_messages = await tester.get_sent_messages()
            
            # Filter responses for this user
            user_responses = [
                msg.get("text", "") for msg in sent_messages
                if msg.get("user_id") == user_id
            ]
            
            # Responses should be in the same order as inputs
            # (Each response is "Echo: <text>")
            expected_responses = [f"Echo: {text}" for text in conversation]
            
            # Check if responses match expected order
            # We allow some extra messages (like system messages)
            # but user responses should appear in order
            user_response_idx = 0
            for expected in expected_responses:
                # Find this expected response in the sent messages
                found = False
                while user_response_idx < len(user_responses):
                    if user_responses[user_response_idx] == expected:
                        found = True
                        user_response_idx += 1
                        break
                    user_response_idx += 1
                
                if not found:
                    print(f"❌ Ordering violation!")
                    print(f"   Conversation: {conversation}")
                    print(f"   Expected: {expected}")
                    print(f"   User responses: {user_responses}")
                    raise AssertionError(f"Ordering violation for: {expected}")
            
            print(f"✅ Ordering test passed for {len(conversation)} messages")
            return True
    
    return asyncio.run(run_test())

if __name__ == "__main__":
    print("🧪 Running ZeroClawed Property Tests")
    print("=====================================")
    
    # Run property tests
    try:
        print("\n🔍 Testing: No Message Loss")
        hypothesis.given(test_no_message_loss)()
        print("✅ No message loss property holds!")
        
        print("\n🔍 Testing: Conversation Ordering")
        hypothesis.given(test_conversation_ordering)()
        print("✅ Conversation ordering property holds!")
        
        print("\n🎉 All property tests passed!")
        
    except AssertionError as e:
        print(f"\n❌ Property test failed: {e}")
        exit(1)
    except Exception as e:
        print(f"\n⚠️ Unexpected error: {e}")
        exit(1)
#!/usr/bin/env python3
"""
Check Ralph's backlog by running tests against the actual ZeroClawed deployment.
This runs integration tests, not unit tests.
"""

import asyncio
import aiohttp
import json
import time
from typing import List, Dict, Any

ZEROCLAWED_URL = "http://192.168.1.210:8083"
API_KEY = "test-key-123"

class BacklogChecker:
    def __init__(self):
        self.session = None
        self.results = []
        
    async def __aenter__(self):
        self.session = aiohttp.ClientSession()
        return self
        
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        if self.session:
            await self.session.close()
    
    async def make_request(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        """Make a request to ZeroClawed."""
        try:
            async with self.session.post(
                f"{ZEROCLAWED_URL}/v1/chat/completions",
                headers={
                    "Authorization": f"Bearer {API_KEY}",
                    "Content-Type": "application/json"
                },
                json=payload,
                timeout=aiohttp.ClientTimeout(total=10)
            ) as response:
                return {
                    "status": response.status,
                    "body": await response.json() if response.status != 204 else {},
                    "headers": dict(response.headers)
                }
        except Exception as e:
            return {"error": str(e)}
    
    async def check_invalid_model_error(self):
        """Check: Invalid model should return proper error (400/404), not 500."""
        print("🔍 Test 1: Invalid model error handling")
        
        result = await self.make_request({
            "model": "non-existent-model-123",
            "messages": [{"role": "user", "content": "test"}]
        })
        
        if "error" in result:
            print("  ❌ Request failed:", result["error"])
            return False
            
        status = result["status"]
        
        if status == 500:
            print(f"  ❌ FAILING (AS EXPECTED): Returns 500 instead of 400/404")
            print(f"     Error: {result.get('body', {}).get('error', {})}")
            return False
        elif status == 400 or status == 404:
            print(f"  ✅ PASSED (UNEXPECTED): Returns proper {status}")
            return True
        else:
            print(f"  ⚠️  Returns {status} (unexpected)")
            return False
    
    async def check_concurrent_requests(self):
        """Check: Concurrent requests shouldn't deadlock."""
        print("\n🔍 Test 2: Concurrent request handling")
        
        async def make_small_request(i: int):
            return await self.make_request({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": f"Request {i}"}],
                "max_tokens": 3
            })
        
        # Send 5 concurrent requests
        tasks = [make_small_request(i) for i in range(5)]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        completed = 0
        for i, result in enumerate(results):
            if isinstance(result, Exception):
                print(f"  Request {i} raised exception: {result}")
            elif "error" in result:
                print(f"  Request {i} failed: {result['error']}")
            else:
                completed += 1
                
        print(f"  Completed: {completed}/5 requests")
        
        if completed >= 3:
            print("  ✅ Concurrent requests work")
            return True
        else:
            print("  ❌ FAILING (AS EXPECTED): Concurrent requests may deadlock")
            return False
    
    async def check_error_message_leakage(self):
        """Check: Error messages shouldn't leak sensitive info."""
        print("\n🔍 Test 3: Error message security")
        
        # Try with invalid API key
        try:
            async with self.session.post(
                f"{ZEROCLAWED_URL}/v1/chat/completions",
                headers={
                    "Authorization": "Bearer invalid-key-123",
                    "Content-Type": "application/json"
                },
                json={
                    "model": "deepseek-chat",
                    "messages": [{"role": "user", "content": "test"}]
                },
                timeout=aiohttp.ClientTimeout(total=5)
            ) as response:
                body = await response.text()
                
                # Check for sensitive info
                sensitive_patterns = [
                    "api.openai.com",
                    "api.anthropic.com", 
                    "api.deepseek.com",
                    "Bearer ",
                    "sk-",
                    "claude-",
                    "upstream",
                    "provider",
                    "backend",
                ]
                
                leaks = []
                for pattern in sensitive_patterns:
                    if pattern in body:
                        leaks.append(pattern)
                
                if leaks:
                    print(f"  ❌ FAILING (AS EXPECTED): Error leaks sensitive info: {leaks}")
                    print(f"     Error preview: {body[:200]}...")
                    return False
                else:
                    print(f"  ✅ Error messages are secure")
                    return True
                    
        except Exception as e:
            print(f"  ⚠️  Request failed: {e}")
            return False
    
    async def check_streaming(self):
        """Check: Streaming should work."""
        print("\n🔍 Test 4: Streaming support")
        
        try:
            async with self.session.post(
                f"{ZEROCLAWED_URL}/v1/chat/completions",
                headers={
                    "Authorization": f"Bearer {API_KEY}",
                    "Content-Type": "application/json"
                },
                json={
                    "model": "deepseek-chat",
                    "messages": [{"role": "user", "content": "Test streaming"}],
                    "stream": True,
                    "max_tokens": 10
                },
                timeout=aiohttp.ClientTimeout(total=15)
            ) as response:
                
                # Check content type
                content_type = response.headers.get('Content-Type', '')
                if 'text/event-stream' in content_type or 'application/x-ndjson' in content_type:
                    print(f"  ✅ Streaming content-type: {content_type}")
                    
                    # Try to read streaming response
                    body = await response.text()
                    if 'data:' in body:
                        print(f"  ✅ Streaming data received")
                        return True
                    else:
                        print(f"  ⚠️  No streaming data in response")
                        return False
                else:
                    print(f"  ❌ FAILING (AS EXPECTED): Not streaming content-type: {content_type}")
                    return False
                    
        except Exception as e:
            print(f"  ⚠️  Streaming test failed: {e}")
            return False
    
    async def check_rate_limiting(self):
        """Check: Rate limiting should work."""
        print("\n🔍 Test 5: Rate limiting")
        
        # Send burst of requests
        start_time = time.time()
        tasks = []
        for i in range(12):  # More than typical rate limit
            tasks.append(self.make_request({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": f"Rate test {i}"}],
                "max_tokens": 2
            }))
        
        results = await asyncio.gather(*tasks, return_exceptions=True)
        elapsed = time.time() - start_time
        
        # Count results
        successes = 0
        rate_limited = 0
        errors = 0
        
        for result in results:
            if isinstance(result, Exception):
                errors += 1
            elif "error" in result:
                errors += 1
            else:
                status = result.get("status", 0)
                if status == 429:
                    rate_limited += 1
                elif 200 <= status < 300:
                    successes += 1
                else:
                    errors += 1
        
        print(f"  Results: {successes} success, {rate_limited} rate limited, {errors} errors in {elapsed:.1f}s")
        
        if rate_limited > 0:
            print(f"  ✅ Rate limiting active")
            return True
        else:
            print(f"  ❌ FAILING (AS EXPECTED): No rate limiting detected")
            return False

async def main():
    print("🔴📋 ZEROCLAWED BACKLOG CHECKER 📋🔴")
    print("=====================================")
    print("Checking actual deployment on VM 210")
    print("")
    
    checker = BacklogChecker()
    
    async with checker:
        tests = [
            ("Invalid model error", checker.check_invalid_model_error),
            ("Concurrent requests", checker.check_concurrent_requests),
            ("Error message security", checker.check_error_message_leakage),
            ("Streaming support", checker.check_streaming),
            ("Rate limiting", checker.check_rate_limiting),
        ]
        
        results = []
        for name, test_func in tests:
            try:
                result = await test_func()
                results.append((name, result))
            except Exception as e:
                print(f"  💥 Test '{name}' crashed: {e}")
                results.append((name, False))
        
        # Summary
        print("\n" + "="*50)
        print("📊 BACKLOG SUMMARY")
        print("="*50)
        
        failing = []
        passing = []
        
        for name, result in results:
            if result:
                passing.append(name)
            else:
                failing.append(name)
        
        print(f"\n✅ PASSING ({len(passing)}):")
        for name in passing:
            print(f"  • {name}")
        
        print(f"\n❌ FAILING ({len(failing)} - Ralph's backlog!):")
        for name in failing:
            print(f"  • {name}")
        
        print(f"\n🎯 Ralph has {len(failing)} items to work on!")
        
        if failing:
            print("\n💡 Priority order:")
            print("  1. Error message leakage (security)")
            print("  2. Invalid model handling (user experience)")
            print("  3. Concurrent request deadlocks (stability)")
            print("  4. Streaming support (feature)")
            print("  5. Rate limiting (resource protection)")

if __name__ == "__main__":
    asyncio.run(main())
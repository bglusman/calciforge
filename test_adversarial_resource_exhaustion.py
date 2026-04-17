#!/usr/bin/env python3
"""
Adversarial test: Resource Exhaustion Attack

Simulates various resource exhaustion attacks on ZeroClawed:
1. Connection flood
2. Memory exhaustion via large messages
3. CPU exhaustion via complex processing
4. File descriptor exhaustion
"""

import asyncio
import aiohttp
import random
import string
import time
import psutil
import os
from typing import List, Dict, Any
from dataclasses import dataclass
from concurrent.futures import ThreadPoolExecutor
import threading

@dataclass
class AttackResult:
    """Result of an adversarial attack."""
    attack_type: str
    success: bool
    duration: float
    error: str = None
    metrics: Dict[str, Any] = None

class ResourceExhaustionAttacker:
    """Simulates resource exhaustion attacks."""
    
    def __init__(self, target_url: str = "http://localhost:9090"):
        self.target_url = target_url
        self.session = None
        self.process = psutil.Process(os.getpid())
        
    async def __aenter__(self):
        self.session = aiohttp.ClientSession()
        return self
        
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        if self.session:
            await self.session.close()
    
    async def get_system_metrics(self) -> Dict[str, Any]:
        """Get current system resource usage."""
        return {
            "cpu_percent": psutil.cpu_percent(interval=0.1),
            "memory_percent": psutil.virtual_memory().percent,
            "open_files": len(self.process.open_files()) if hasattr(self.process, 'open_files') else 0,
            "connections": len(self.process.connections()) if hasattr(self.process, 'connections') else 0,
            "threads": self.process.num_threads(),
        }
    
    async def attack_connection_flood(self, num_connections: int = 100) -> AttackResult:
        """
        Attack 1: Connection flood
        Opens many simultaneous connections to exhaust connection pool.
        """
        print(f"🔴 Starting connection flood attack ({num_connections} connections)...")
        start_time = time.time()
        
        async def make_request(req_num: int):
            try:
                async with self.session.post(
                    f"{self.target_url}/send",
                    json={
                        "user_id": f"flood-user-{req_num}",
                        "text": f"Flood message {req_num}",
                        "channel": "mock"
                    },
                    timeout=aiohttp.ClientTimeout(total=10)
                ) as response:
                    return await response.json()
            except Exception as e:
                return {"error": str(e)}
        
        # Launch all requests simultaneously
        tasks = [make_request(i) for i in range(num_connections)]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        duration = time.time() - start_time
        metrics = await self.get_system_metrics()
        
        # Count successes vs failures
        successes = sum(1 for r in results if not isinstance(r, Exception) and "error" not in r)
        failures = len(results) - successes
        
        return AttackResult(
            attack_type="connection_flood",
            success=failures < num_connections * 0.5,  # Less than 50% failure rate
            duration=duration,
            metrics={
                **metrics,
                "requests_sent": num_connections,
                "successful_responses": successes,
                "failed_responses": failures,
                "requests_per_second": num_connections / duration if duration > 0 else 0
            }
        )
    
    async def attack_memory_exhaustion(self, message_size_kb: int = 1024) -> AttackResult:
        """
        Attack 2: Memory exhaustion via large messages
        Sends very large messages to exhaust memory.
        """
        print(f"🔴 Starting memory exhaustion attack ({message_size_kb}KB messages)...")
        start_time = time.time()
        
        # Generate large message
        large_text = ''.join(random.choices(string.ascii_letters + string.digits, k=message_size_kb * 1024))
        
        metrics_before = await self.get_system_metrics()
        
        try:
            async with self.session.post(
                f"{self.target_url}/send",
                json={
                    "user_id": "memory-attacker",
                    "text": large_text,
                    "channel": "mock"
                },
                timeout=aiohttp.ClientTimeout(total=30)
            ) as response:
                result = await response.json()
            
            duration = time.time() - start_time
            metrics_after = await self.get_system_metrics()
            
            # Check memory increase
            memory_increase = metrics_after["memory_percent"] - metrics_before["memory_percent"]
            
            return AttackResult(
                attack_type="memory_exhaustion",
                success=memory_increase < 10,  # Less than 10% memory increase
                duration=duration,
                metrics={
                    "memory_before": metrics_before["memory_percent"],
                    "memory_after": metrics_after["memory_percent"],
                    "memory_increase": memory_increase,
                    "message_size_kb": message_size_kb,
                    "response_received": "error" not in result
                }
            )
            
        except Exception as e:
            duration = time.time() - start_time
            return AttackResult(
                attack_type="memory_exhaustion",
                success=False,
                duration=duration,
                error=str(e),
                metrics={"message_size_kb": message_size_kb}
            )
    
    async def attack_cpu_exhaustion(self, num_complex_messages: int = 50) -> AttackResult:
        """
        Attack 3: CPU exhaustion via many complex messages
        Sends many messages that require complex processing.
        """
        print(f"🔴 Starting CPU exhaustion attack ({num_complex_messages} complex messages)...")
        start_time = time.time()
        
        metrics_before = await self.get_system_metrics()
        
        # Generate messages with complex patterns
        complex_messages = []
        for i in range(num_complex_messages):
            # Create message with various patterns that might trigger complex processing
            patterns = [
                "Calculate " + " ".join(str(random.randint(1, 1000)) for _ in range(20)),
                "Parse this JSON: " + json.dumps({f"key{j}": f"value{random.randint(1, 100)}" for j in range(10)}),
                "Evaluate expression: " + " + ".join(str(random.randint(1, 100)) for _ in range(15)),
                "Process list: [" + ", ".join(f'"{random.choice(string.ascii_letters)}"' for _ in range(20)) + "]"
            ]
            complex_messages.append(random.choice(patterns))
        
        async def send_complex_message(msg: str, idx: int):
            try:
                async with self.session.post(
                    f"{self.target_url}/send",
                    json={
                        "user_id": f"cpu-attacker-{idx}",
                        "text": msg,
                        "channel": "mock"
                    },
                    timeout=aiohttp.ClientTimeout(total=10)
                ) as response:
                    return await response.json()
            except Exception as e:
                return {"error": str(e)}
        
        # Send all complex messages
        tasks = [send_complex_message(msg, i) for i, msg in enumerate(complex_messages)]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        duration = time.time() - start_time
        metrics_after = await self.get_system_metrics()
        
        # Count successes
        successes = sum(1 for r in results if not isinstance(r, Exception) and "error" not in r)
        
        # Check CPU usage
        cpu_increase = metrics_after["cpu_percent"] - metrics_before["cpu_percent"]
        
        return AttackResult(
            attack_type="cpu_exhaustion",
            success=cpu_increase < 50 and successes > num_complex_messages * 0.7,
            duration=duration,
            metrics={
                "cpu_before": metrics_before["cpu_percent"],
                "cpu_after": metrics_after["cpu_percent"],
                "cpu_increase": cpu_increase,
                "messages_sent": num_complex_messages,
                "successful_responses": successes,
                "messages_per_second": num_complex_messages / duration if duration > 0 else 0
            }
        )
    
    async def attack_slowloris(self, num_connections: int = 10, duration_seconds: int = 30) -> AttackResult:
        """
        Attack 4: Slowloris attack
        Opens connections and sends data very slowly to keep them open.
        """
        print(f"🔴 Starting Slowloris attack ({num_connections} connections for {duration_seconds}s)...")
        start_time = time.time()
        
        metrics_before = await self.get_system_metrics()
        
        async def slowloris_connection(conn_id: int):
            try:
                # Open connection
                async with self.session.post(
                    f"{self.target_url}/send",
                    json={
                        "user_id": f"slowloris-{conn_id}",
                        "text": "S" * 1024,  # 1KB of data
                        "channel": "mock"
                    },
                    timeout=aiohttp.ClientTimeout(total=duration_seconds + 5)
                ) as response:
                    # Read response slowly
                    await asyncio.sleep(duration_seconds)
                    return await response.json()
            except Exception as e:
                return {"error": str(e)}
        
        # Start all slowloris connections
        tasks = [slowloris_connection(i) for i in range(num_connections)]
        
        # Wait for attack duration
        await asyncio.sleep(duration_seconds)
        
        # Check metrics during attack
        metrics_during = await self.get_system_metrics()
        
        # Cancel remaining tasks
        for task in tasks:
            task.cancel()
        
        duration = time.time() - start_time
        
        return AttackResult(
            attack_type="slowloris",
            success=metrics_during["connections"] < num_connections * 2,  # Not too many connections
            duration=duration,
            metrics={
                "connections_before": metrics_before["connections"],
                "connections_during": metrics_during["connections"],
                "target_connections": num_connections,
                "attack_duration": duration_seconds
            }
        )

async def run_adversarial_tests():
    """Run all adversarial resource exhaustion tests."""
    print("🔴🔴🔴 ADVERSARIAL TEST: RESOURCE EXHAUSTION 🔴🔴🔴")
    print("===================================================")
    
    async with ResourceExhaustionAttacker() as attacker:
        results = []
        
        # 1. Connection flood
        print("\n" + "="*50)
        result = await attacker.attack_connection_flood(num_connections=50)
        results.append(result)
        print(f"Connection Flood: {'✅ RESILIENT' if result.success else '❌ VULNERABLE'}")
        print(f"  Duration: {result.duration:.2f}s")
        print(f"  Metrics: {result.metrics}")
        
        # 2. Memory exhaustion
        print("\n" + "="*50)
        result = await attacker.attack_memory_exhaustion(message_size_kb=512)
        results.append(result)
        print(f"Memory Exhaustion: {'✅ RESILIENT' if result.success else '❌ VULNERABLE'}")
        print(f"  Duration: {result.duration:.2f}s")
        print(f"  Metrics: {result.metrics}")
        
        # 3. CPU exhaustion
        print("\n" + "="*50)
        result = await attacker.attack_cpu_exhaustion(num_complex_messages=30)
        results.append(result)
        print(f"CPU Exhaustion: {'✅ RESILIENT' if result.success else '❌ VULNERABLE'}")
        print(f"  Duration: {result.duration:.2f}s")
        print(f"  Metrics: {result.metrics}")
        
        # 4. Slowloris (commented out by default - very aggressive)
        # print("\n" + "="*50)
        # result = await attacker.attack_slowloris(num_connections=5, duration_seconds=10)
        # results.append(result)
        # print(f"Slowloris: {'✅ RESILIENT' if result.success else '❌ VULNERABLE'}")
        # print(f"  Duration: {result.duration:.2f}s")
        # print(f"  Metrics: {result.metrics}")
        
        # Summary
        print("\n" + "="*50)
        print("📊 TEST SUMMARY")
        print("="*50)
        
        resilient_count = sum(1 for r in results if r.success)
        vulnerable_count = len(results) - resilient_count
        
        print(f"Resilient: {resilient_count}/{len(results)}")
        print(f"Vulnerable: {vulnerable_count}/{len(results)}")
        
        if vulnerable_count == 0:
            print("🎉 SYSTEM IS RESILIENT TO RESOURCE EXHAUSTION ATTACKS!")
        else:
            print("⚠️ SYSTEM SHOWS VULNERABILITIES TO RESOURCE EXHAUSTION")
            print("   Consider implementing:")
            print("   - Rate limiting")
            print("   - Request size limits")
            print("   - Connection timeouts")
            print("   - Resource quotas")
        
        return results

if __name__ == "__main__":
    print("⚠️ WARNING: This test performs aggressive resource exhaustion attacks.")
    print("Only run against test instances. Do not run against production!")
    print("")
    
    confirm = input("Continue with adversarial tests? (yes/no): ")
    if confirm.lower() != "yes":
        print("Test cancelled.")
        exit(0)
    
    asyncio.run(run_adversarial_tests())
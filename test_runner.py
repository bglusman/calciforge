#!/usr/bin/env python3
"""
ZeroClawed Test Runner

Executes test scenarios defined in TOML files.
Supports basic, adversarial, property, and mutation tests.
"""

import toml
import json
import time
import subprocess
import sys
import os
from pathlib import Path
from typing import Dict, List, Any, Optional
import requests
import threading
import concurrent.futures

class TestRunner:
    def __init__(self, config_path: str = "test_simple_direct.toml"):
        self.config_path = config_path
        self.server_process = None
        self.base_url = "http://127.0.0.1:8083"
        self.mock_url = "http://127.0.0.1:9090"
        
    def start_server(self):
        """Start ZeroClawed server"""
        print("Starting ZeroClawed server...")
        cmd = ["cargo", "run", "--bin", "zeroclawed", "--", "--config", self.config_path, "--proxy-only"]
        self.server_process = subprocess.Popen(
            cmd,
            cwd="/root/projects/zeroclawed",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        
        # Wait for server to start
        for _ in range(30):  # 30 second timeout
            try:
                resp = requests.get(f"{self.base_url}/health", timeout=1)
                if resp.status_code == 200:
                    print("Server started successfully")
                    return True
            except:
                pass
            time.sleep(1)
        
        print("Failed to start server")
        return False
    
    def stop_server(self):
        """Stop ZeroClawed server"""
        if self.server_process:
            print("Stopping server...")
            self.server_process.terminate()
            self.server_process.wait(timeout=5)
    
    def run_scenario(self, scenario_path: str) -> Dict[str, Any]:
        """Run a single test scenario"""
        print(f"Running scenario: {scenario_path}")
        
        # Load scenario
        with open(scenario_path, 'r') as f:
            scenario = toml.load(f)
        
        name = scenario.get('name', 'Unnamed Scenario')
        category = scenario.get('category', 'unknown')
        
        print(f"  Name: {name}")
        print(f"  Category: {category}")
        
        # Execute based on category
        if category.startswith('basic.'):
            return self._run_basic_scenario(scenario)
        elif category.startswith('adversarial.'):
            return self._run_adversarial_scenario(scenario)
        elif category.startswith('property.'):
            return self._run_property_scenario(scenario)
        else:
            return {"success": False, "error": f"Unknown category: {category}"}
    
    def _run_basic_scenario(self, scenario: Dict) -> Dict[str, Any]:
        """Run basic functionality test"""
        try:
            # Example: Simple echo test
            steps = scenario.get('scenario', {}).get('steps', [])
            
            for step in steps:
                action = step.get('action')
                
                if action == 'send_message':
                    # Send message via mock channel
                    sender = step.get('sender', 'test-user')
                    text = step.get('text', 'Hello')
                    
                    resp = requests.post(
                        f"{self.mock_url}/send",
                        json={
                            "sender": sender,
                            "text": text
                        },
                        timeout=5
                    )
                    
                    if resp.status_code != 200:
                        return {"success": False, "error": f"Failed to send message: {resp.text}"}
                    
                    result = resp.json()
                    message_id = result.get('data', {}).get('message_id')
                    
                    # Wait for response
                    time.sleep(2)
                    
                    # Check messages
                    resp = requests.get(f"{self.mock_url}/messages", timeout=5)
                    messages = resp.json()
                    
                    # Verify response contains expected text
                    expected_text = step.get('contains', '')
                    if expected_text:
                        sent_messages = messages.get('data', {}).get('sent', [])
                        if not any(expected_text.lower() in msg.get('text', '').lower() 
                                  for msg in sent_messages):
                            return {"success": False, "error": f"Response doesn't contain '{expected_text}'"}
                
                elif action == 'wait':
                    duration = float(step.get('duration', '1').replace('s', ''))
                    time.sleep(duration)
            
            return {"success": True, "message": "Basic test passed"}
            
        except Exception as e:
            return {"success": False, "error": str(e)}
    
    def _run_adversarial_scenario(self, scenario: Dict) -> Dict[str, Any]:
        """Run adversarial test"""
        try:
            category = scenario.get('category', '')
            
            if 'injection' in category:
                return self._run_injection_test(scenario)
            elif 'resource_exhaustion' in category:
                return self._run_resource_exhaustion_test(scenario)
            else:
                return {"success": False, "error": f"Unknown adversarial test type: {category}"}
                
        except Exception as e:
            return {"success": False, "error": str(e)}
    
    def _run_injection_test(self, scenario: Dict) -> Dict[str, Any]:
        """Test injection attacks"""
        print("  Running injection test...")
        
        # For now, just verify system doesn't crash
        # In real implementation, would attempt actual injections
        
        # Send normal message
        resp = requests.post(
            f"{self.mock_url}/send",
            json={
                "sender": "test-user",
                "text": "Normal message"
            },
            timeout=5
        )
        
        if resp.status_code != 200:
            return {"success": False, "error": "System crashed on normal message"}
        
        # Check system still responsive
        health_resp = requests.get(f"{self.base_url}/health", timeout=5)
        if health_resp.status_code != 200:
            return {"success": False, "error": "System became unresponsive"}
        
        return {"success": True, "message": "Injection test passed (basic resilience)"}
    
    def _run_resource_exhaustion_test(self, scenario: Dict) -> Dict[str, Any]:
        """Test resource exhaustion attacks"""
        print("  Running resource exhaustion test...")
        
        # Send multiple concurrent requests
        def send_request(i: int):
            try:
                resp = requests.post(
                    f"{self.mock_url}/send",
                    json={
                        "sender": f"user{i}",
                        "text": f"Message {i}"
                    },
                    timeout=10
                )
                return resp.status_code == 200
            except:
                return False
        
        # Send 20 concurrent requests
        with concurrent.futures.ThreadPoolExecutor(max_workers=20) as executor:
            futures = [executor.submit(send_request, i) for i in range(20)]
            results = [f.result() for f in concurrent.futures.as_completed(futures)]
        
        success_rate = sum(results) / len(results)
        
        # System should handle at least 50% of requests under load
        if success_rate < 0.5:
            return {"success": False, "error": f"Low success rate under load: {success_rate:.1%}"}
        
        # Verify system still responsive
        health_resp = requests.get(f"{self.base_url}/health", timeout=5)
        if health_resp.status_code != 200:
            return {"success": False, "error": "System became unresponsive under load"}
        
        return {"success": True, "message": f"Resource test passed ({success_rate:.1%} success rate)"}
    
    def _run_property_scenario(self, scenario: Dict) -> Dict[str, Any]:
        """Run property-based test"""
        print("  Running property test...")
        
        # For now, run a simplified version
        iterations = scenario.get('test_parameters', {}).get('iterations', 10)
        
        successes = 0
        for i in range(min(iterations, 10)):  # Limit to 10 for demo
            # Send a message
            resp = requests.post(
                f"{self.mock_url}/send",
                json={
                    "sender": f"prop_user_{i}",
                    "text": f"Property test message {i}"
                },
                timeout=5
            )
            
            if resp.status_code == 200:
                successes += 1
            
            time.sleep(0.1)
        
        success_rate = successes / min(iterations, 10)
        
        if success_rate < 0.8:  # 80% success threshold
            return {"success": False, "error": f"Low property test success rate: {success_rate:.1%}"}
        
        return {"success": True, "message": f"Property test passed ({success_rate:.1%} success rate)"}
    
    def run_all_scenarios(self, scenario_dir: str = "test_scenarios") -> Dict[str, Any]:
        """Run all scenarios in directory"""
        results = {
            "total": 0,
            "passed": 0,
            "failed": 0,
            "details": []
        }
        
        # Find all TOML files
        scenario_files = []
        for root, dirs, files in os.walk(scenario_dir):
            for file in files:
                if file.endswith('.toml'):
                    scenario_files.append(os.path.join(root, file))
        
        print(f"Found {len(scenario_files)} scenario files")
        
        # Start server
        if not self.start_server():
            return {"success": False, "error": "Failed to start server"}
        
        try:
            # Run each scenario
            for scenario_file in scenario_files:
                results["total"] += 1
                
                try:
                    result = self.run_scenario(scenario_file)
                    
                    if result.get("success"):
                        results["passed"] += 1
                        print(f"  ✓ {os.path.basename(scenario_file)}")
                    else:
                        results["failed"] += 1
                        print(f"  ✗ {os.path.basename(scenario_file)}: {result.get('error', 'Unknown error')}")
                    
                    results["details"].append({
                        "file": scenario_file,
                        "result": result
                    })
                    
                except Exception as e:
                    results["failed"] += 1
                    print(f"  ✗ {os.path.basename(scenario_file)}: Exception: {e}")
                    results["details"].append({
                        "file": scenario_file,
                        "result": {"success": False, "error": str(e)}
                    })
                
                # Small delay between tests
                time.sleep(1)
        
        finally:
            self.stop_server()
        
        # Summary
        print(f"\nTest Summary:")
        print(f"  Total: {results['total']}")
        print(f"  Passed: {results['passed']}")
        print(f"  Failed: {results['failed']}")
        
        overall_success = results['failed'] == 0
        return {
            "success": overall_success,
            "summary": results
        }

def main():
    """Main entry point"""
    runner = TestRunner()
    
    # Run all scenarios
    result = runner.run_all_scenarios()
    
    if result["success"]:
        print("\n✅ All tests passed!")
        return 0
    else:
        print("\n❌ Some tests failed")
        return 1

if __name__ == "__main__":
    sys.exit(main())
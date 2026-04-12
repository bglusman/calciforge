#!/usr/bin/env python3
"""
ZeroClawed Test Suite Orchestrator

Orchestrates the complete test suite:
1. Mock channel integration tests
2. Property tests (Hegel)
3. Adversarial tests
4. Mutation testing analysis
"""

import asyncio
import subprocess
import time
import json
import sys
from typing import List, Dict, Any
from dataclasses import dataclass
from enum import Enum
import threading

class TestCategory(Enum):
    UNIT = "unit"
    INTEGRATION = "integration"
    PROPERTY = "property"
    ADVERSARIAL = "adversarial"
    MUTATION = "mutation"

class TestResult(Enum):
    PASSED = "passed"
    FAILED = "failed"
    SKIPPED = "skipped"
    ERROR = "error"

@dataclass
class TestCase:
    name: str
    category: TestCategory
    command: List[str]
    timeout: int = 60
    dependencies: List[str] = None
    
    def __post_init__(self):
        if self.dependencies is None:
            self.dependencies = []

@dataclass
class TestExecution:
    test: TestCase
    result: TestResult
    duration: float
    output: str = ""
    error: str = None
    metrics: Dict[str, Any] = None

class TestOrchestrator:
    """Orchestrates the execution of all test suites."""
    
    def __init__(self):
        self.tests = self._load_test_suite()
        self.results = []
        self.zeroclawed_process = None
        
    def _load_test_suite(self) -> List[TestCase]:
        """Load the complete test suite."""
        return [
            # Mock Channel Integration Tests
            TestCase(
                name="mock_channel_basic",
                category=TestCategory.INTEGRATION,
                command=["bash", "/root/projects/zeroclawed/test_mock_channel.sh"],
                timeout=30,
                dependencies=["zeroclawed_running"]
            ),
            
            # Property Tests
            TestCase(
                name="property_no_message_loss",
                category=TestCategory.PROPERTY,
                command=["python3", "/root/projects/zeroclawed/test_property_no_message_loss.py"],
                timeout=120,
                dependencies=["zeroclawed_running"]
            ),
            
            # Adversarial Tests (Resource Exhaustion)
            TestCase(
                name="adversarial_resource_exhaustion",
                category=TestCategory.ADVERSARIAL,
                command=["python3", "/root/projects/zeroclawed/test_adversarial_resource_exhaustion.py"],
                timeout=180,
                dependencies=["zeroclawed_running"]
            ),
            
            # Unit Tests (Rust)
            TestCase(
                name="rust_unit_tests",
                category=TestCategory.UNIT,
                command=["cargo", "test", "--workspace", "--lib", "--bins"],
                timeout=300
            ),
            
            # Integration Tests (Rust)
            TestCase(
                name="rust_integration_tests",
                category=TestCategory.INTEGRATION,
                command=["cargo", "test", "--workspace", "--test", "*"],
                timeout=300
            ),
            
            # Loom Tests (Concurrency)
            TestCase(
                name="loom_concurrency_tests",
                category=TestCategory.UNIT,
                command=["cargo", "test", "--package", "loom-tests"],
                timeout=120
            ),
            
            # Config Validator Tests
            TestCase(
                name="config_validator_tests",
                category=TestCategory.UNIT,
                command=["bash", "/root/projects/zeroclawed/test_config_validator.sh"],
                timeout=60
            ),
            
            # Adversarial Test Scenarios
            TestCase(
                name="adversarial_scenarios",
                category=TestCategory.ADVERSARIAL,
                command=["bash", "/root/projects/zeroclawed/run_adversarial_tests.sh"],
                timeout=180,
                dependencies=["zeroclawed_running"]
            ),
        ]
    
    async def start_zeroclawed(self):
        """Start ZeroClawed for integration tests."""
        print("🚀 Starting ZeroClawed for integration tests...")
        
        # Create test config
        config_content = """
[general]
name = "test-suite"
log_level = "info"

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

[[agents]]
id = "complex-agent"
model = "echo"
enabled = true

[agents.config]
response_prefix = "Processed: "
"""
        
        with open("/tmp/zeroclawed_test_config.toml", "w") as f:
            f.write(config_content)
        
        # Start ZeroClawed
        self.zeroclawed_process = subprocess.Popen(
            ["/root/projects/zeroclawed/target/release/zeroclawed",
             "--config", "/tmp/zeroclawed_test_config.toml"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE
        )
        
        # Wait for startup
        await asyncio.sleep(5)
        
        # Check if it's running
        try:
            result = subprocess.run(
                ["curl", "-s", "http://localhost:9090/health"],
                capture_output=True,
                text=True,
                timeout=5
            )
            if "healthy" in result.stdout:
                print("✅ ZeroClawed started successfully")
                return True
            else:
                print("❌ ZeroClawed failed to start")
                return False
        except:
            print("❌ ZeroClawed health check failed")
            return False
    
    def stop_zeroclawed(self):
        """Stop ZeroClawed."""
        if self.zeroclawed_process:
            print("🛑 Stopping ZeroClawed...")
            self.zeroclawed_process.terminate()
            try:
                self.zeroclawed_process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.zeroclawed_process.kill()
            self.zeroclawed_process = None
    
    async def run_test(self, test: TestCase) -> TestExecution:
        """Run a single test case."""
        print(f"\n🔍 Running test: {test.name} ({test.category.value})")
        print(f"   Command: {' '.join(test.command)}")
        
        start_time = time.time()
        
        try:
            # Check dependencies
            for dep in test.dependencies:
                if dep == "zeroclawed_running" and not self.zeroclawed_process:
                    return TestExecution(
                        test=test,
                        result=TestResult.SKIPPED,
                        duration=0,
                        error=f"Dependency not met: {dep}"
                    )
            
            # Run the test
            process = await asyncio.create_subprocess_exec(
                *test.command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            try:
                stdout, stderr = await asyncio.wait_for(
                    process.communicate(),
                    timeout=test.timeout
                )
                
                duration = time.time() - start_time
                output = stdout.decode() + stderr.decode()
                
                if process.returncode == 0:
                    print(f"   ✅ PASSED ({duration:.2f}s)")
                    return TestExecution(
                        test=test,
                        result=TestResult.PASSED,
                        duration=duration,
                        output=output[:1000]  # Truncate output
                    )
                else:
                    print(f"   ❌ FAILED ({duration:.2f}s)")
                    return TestExecution(
                        test=test,
                        result=TestResult.FAILED,
                        duration=duration,
                        output=output[:1000],
                        error=f"Exit code: {process.returncode}"
                    )
                    
            except asyncio.TimeoutError:
                process.kill()
                duration = time.time() - start_time
                print(f"   ⏰ TIMEOUT ({duration:.2f}s)")
                return TestExecution(
                    test=test,
                    result=TestResult.FAILED,
                    duration=duration,
                    error=f"Timeout after {test.timeout}s"
                )
                
        except Exception as e:
            duration = time.time() - start_time
            print(f"   💥 ERROR ({duration:.2f}s)")
            return TestExecution(
                test=test,
                result=TestResult.ERROR,
                duration=duration,
                error=str(e)
            )
    
    async def run_test_suite(self, categories: List[TestCategory] = None):
        """Run the complete test suite."""
        print("🧪 ZEROCLAWED COMPREHENSIVE TEST SUITE")
        print("=======================================")
        
        # Filter tests by category if specified
        tests_to_run = self.tests
        if categories:
            tests_to_run = [t for t in self.tests if t.category in categories]
        
        print(f"Running {len(tests_to_run)} tests...")
        
        # Start ZeroClawed if needed
        needs_zeroclawed = any("zeroclawed_running" in t.dependencies for t in tests_to_run)
        if needs_zeroclawed:
            if not await self.start_zeroclawed():
                print("❌ Failed to start ZeroClawed, skipping dependent tests")
                tests_to_run = [t for t in tests_to_run if "zeroclawed_running" not in t.dependencies]
        
        # Run tests
        self.results = []
        for test in tests_to_run:
            result = await self.run_test(test)
            self.results.append(result)
        
        # Stop ZeroClawed if started
        if needs_zeroclawed:
            self.stop_zeroclawed()
        
        # Generate report
        self._generate_report()
    
    def _generate_report(self):
        """Generate a comprehensive test report."""
        print("\n" + "="*60)
        print("📊 TEST SUITE REPORT")
        print("="*60)
        
        # Summary by category
        categories = {}
        for result in self.results:
            cat = result.test.category.value
            if cat not in categories:
                categories[cat] = {"total": 0, "passed": 0, "failed": 0, "skipped": 0, "error": 0}
            
            categories[cat]["total"] += 1
            categories[cat][result.result.value] += 1
        
        # Print category summary
        print("\n📈 CATEGORY SUMMARY:")
        for cat, stats in categories.items():
            passed_pct = (stats["passed"] / stats["total"] * 100) if stats["total"] > 0 else 0
            print(f"  {cat.upper():15} {stats['passed']:3}/{stats['total']:3} ({passed_pct:5.1f}%)")
        
        # Print detailed results
        print("\n📝 DETAILED RESULTS:")
        for result in self.results:
            status_icon = {
                TestResult.PASSED: "✅",
                TestResult.FAILED: "❌",
                TestResult.SKIPPED: "⏭️",
                TestResult.ERROR: "💥"
            }[result.result]
            
            print(f"  {status_icon} {result.test.name:30} {result.result.value:8} {result.duration:6.2f}s")
            if result.error:
                print(f"     Error: {result.error}")
        
        # Overall statistics
        total_tests = len(self.results)
        passed_tests = sum(1 for r in self.results if r.result == TestResult.PASSED)
        failed_tests = sum(1 for r in self.results if r.result == TestResult.FAILED)
        error_tests = sum(1 for r in self.results if r.result == TestResult.ERROR)
        skipped_tests = sum(1 for r in self.results if r.result == TestResult.SKIPPED)
        
        pass_rate = (passed_tests / total_tests * 100) if total_tests > 0 else 0
        
        print("\n" + "="*60)
        print(f"📊 OVERALL: {passed_tests}/{total_tests} tests passed ({pass_rate:.1f}%)")
        
        if failed_tests == 0 and error_tests == 0:
            print("🎉 ALL TESTS PASSED!")
        else:
            print(f"⚠️  {failed_tests} tests failed, {error_tests} errors, {skipped_tests} skipped")
        
        # Save report to file
        report_data = {
            "timestamp": time.time(),
            "total_tests": total_tests,
            "passed": passed_tests,
            "failed": failed_tests,
            "errors": error_tests,
            "skipped": skipped_tests,
            "pass_rate": pass_rate,
            "categories": categories,
            "results": [
                {
                    "name": r.test.name,
                    "category": r.test.category.value,
                    "result": r.result.value,
                    "duration": r.duration,
                    "error": r.error
                }
                for r in self.results
            ]
        }
        
        with open("/tmp/zeroclawed_test_report.json", "w") as f:
            json.dump(report_data, f, indent=2)
        
        print(f"\n📄 Full report saved to: /tmp/zeroclawed_test_report.json")
        
        # Exit code based on test results
        if failed_tests > 0 or error_tests > 0:
            sys.exit(1)

async def main():
    """Main entry point."""
    import argparse
    
    parser = argparse.ArgumentParser(description="ZeroClawed Test Suite Orchestrator")
    parser.add_argument("--category", action="append", choices=[c.value for c in TestCategory],
                       help="Run only specific test categories")
    parser.add_argument("--list", action="store_true",
                       help="List available tests without running them")
    
    args = parser.parse_args()
    
    orchestrator = TestOrchestrator()
    
    if args.list:
        print("Available tests:")
        for test in orchestrator.tests:
            print(f"  {test.name:30} [{test.category.value:12}] {test.command[0]}")
        return
    
    # Convert string categories to enum
    categories = None
    if args.category:
        categories = [TestCategory(c) for c in args.category]
    
    await orchestrator.run_test_suite(categories)

if __name__ == "__main__":
    asyncio.run(main())
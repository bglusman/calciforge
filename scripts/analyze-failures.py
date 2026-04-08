#!/usr/bin/env python3
"""
Analyze test failures and suggest fixes.
Part of the Ralph Loop automated testing system.
"""

import sys
import re
from typing import List, Dict

def analyze(log_path: str) -> None:
    """Analyze test log and suggest fixes."""
    
    try:
        with open(log_path) as f:
            content = f.read()
    except FileNotFoundError:
        print(f"Error: Log file not found: {log_path}")
        sys.exit(1)
    
    print("## Failure Analysis Report")
    print()
    
    # Find all failed tests
    failed_tests = re.findall(r'test (\S+) \.\.\. FAILED', content)
    
    if not failed_tests:
        print("No failed tests found in log.")
        return
    
    print(f"Found {len(failed_tests)} failed test(s):")
    for test in failed_tests:
        print(f"  - {test}")
    print()
    
    # Categorize failures
    patterns: Dict[str, List[str]] = {
        "404 errors": [],
        "401 auth errors": [],
        "builder errors": [],
        "TOML parse errors": [],
        "unknown adapter kind": [],
        "compilation errors": [],
        "assertion failures": [],
    }
    
    for test in failed_tests:
        # Get the failure context for this test
        test_pattern = rf'test {re.escape(test)}.*?FAILED.*?\n((?:[^\n]*\n){{0,20}})'
        match = re.search(test_pattern, content, re.DOTALL)
        
        if not match:
            continue
            
        context = match.group(1)
        
        # Categorize
        if "404" in context:
            patterns["404 errors"].append((test, context))
        elif "401" in context or "Unauthorized" in context:
            patterns["401 auth errors"].append((test, context))
        elif "builder error" in context:
            patterns["builder errors"].append((test, context))
        elif "TOML" in context or "toml" in context:
            patterns["TOML parse errors"].append((test, context))
        elif "unknown agent kind" in context:
            patterns["unknown adapter kind"].append((test, context))
        elif "compilation" in context or "error\[" in context:
            patterns["compilation errors"].append((test, context))
        else:
            patterns["assertion failures"].append((test, context))
    
    # Print categorized analysis
    for category, failures in patterns.items():
        if not failures:
            continue
            
        print(f"### {category}")
        print()
        
        for test, context in failures[:3]:  # Show first 3 of each
            print(f"**{test}**")
            
            if category == "404 errors":
                print("- **Likely cause**: Path routing bug in OneCLI proxy")
                print("  - Check that `/proxy/{provider}/path` strips prefix correctly")
                print("  - Check that `target_url` construction doesn't add double slashes")
                
            elif category == "401 auth errors":
                print("- **Likely cause**: Credential injection failed")
                print("  - Check OneCLI vault lookup for the provider")
                print("  - Verify the credential exists in VaultWarden")
                print("  - Check auth header format (Bearer vs X-Subscription-Token)")
                
            elif category == "builder errors":
                print("- **Likely cause**: Invalid URL construction")
                print("  - Check endpoint URL format")
                print("  - Verify no invalid characters in URL")
                
            elif category == "TOML parse errors":
                print("- **Likely cause**: Config file syntax error")
                print("  - Check for duplicate sections")
                print("  - Verify proper TOML formatting")
                
            elif category == "unknown adapter kind":
                print("- **Likely cause**: Invalid adapter kind in config")
                print("  - Check adapter kind is one of: cli, acp, acpx, zeroclaw,")
                print("    openclaw-http, openclaw-channel, openclaw-native")
                
            elif category == "compilation errors":
                print("- **Likely cause**: Code doesn't compile")
                print("  - Run `cargo check` to see errors")
                print("  - Check for type mismatches or missing imports")
                
            elif category == "assertion failures":
                print("- **Likely cause**: Test expectation not met")
                print("  - Review test logic")
                print("  - Check if code behavior changed")
            
            # Show relevant context snippet
            lines = context.strip().split('\n')[:5]
            if lines:
                print("- **Context**:")
                for line in lines:
                    if line.strip():
                        print(f"    {line.strip()[:100]}")
            
            print()
    
    # General recommendations
    print("### General Recommendations")
    print()
    
    if any(patterns.values()):
        print("1. **Run specific test:**")
        print(f"   cargo test -p zeroclawed {failed_tests[0]} -- --nocapture")
        print()
        print("2. **Enable backtrace:**")
        print("   RUST_BACKTRACE=1 cargo test -p zeroclawed")
        print()
        print("3. **Check for recent changes:**")
        print("   git diff HEAD~5 -- '*.rs' '*.toml'")
        print()

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <test-log-file>")
        sys.exit(1)
    
    analyze(sys.argv[1])

#!/usr/bin/env python3
import os
import sys
import argparse
import re

# Configuration
FORBIDDEN_WORDS = ["nexus"]
IGNORE_DIRS = [".git", "node_modules", "target", "logs", "dist", "pkg", "stress_results", "performance"]
IGNORE_FILES = ["check_branding.py"]

def check_branding(root_dir):
    found_violations = 0
    
    # Pre-compile patterns
    patterns = [(word, re.compile(rf"\b{re.escape(word)}\b", re.IGNORECASE)) for word in FORBIDDEN_WORDS]
    
    for root, dirs, files in os.walk(root_dir):
        # Filter ignored directories
        dirs[:] = [d for d in dirs if d not in IGNORE_DIRS]
        
        for file in files:
            if file in IGNORE_FILES:
                continue
                
            file_path = os.path.join(root, file)
            
            # Skip basic binary files
            if file_path.endswith((".wasm", ".png", ".jpg", ".jpeg", ".ico", ".aeb", ".lock")):
                continue
                
            try:
                with open(file_path, "r", encoding="utf-8") as f:
                    # Read line by line to avoid memory issues with huge files
                    for line_num, line in enumerate(f, 1):
                        for word, pattern in patterns:
                            if pattern.search(line):
                                print(f"❌ Violation found: '{word}' in {file_path}:{line_num}")
                                found_violations += 1
                                # If we found one violation in this file, we can optionally continue to next file
                                # or keep counting. Let's keep counting for now but break inner loop.
                                break 
            except (UnicodeDecodeError, PermissionError, OSError):
                # Skip files that cannot be read as text or are inaccessible
                continue
                
    return found_violations

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Aetheris Branding Guard — Check for forbidden internal terms.")
    parser.add_argument("path", nargs="?", default=".", help="Root directory to scan (default: current)")
    args = parser.parse_args()
    
    violations = check_branding(args.path)
    
    if violations > 0:
        print(f"\n🚨 Total branding violations: {violations}")
        sys.exit(1)
    else:
        print("✅ Branding check passed. No internal terms found.")
        sys.exit(0)

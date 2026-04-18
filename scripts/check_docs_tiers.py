import os
import sys
import re

DESIGN_DOCS_DIR = "docs/design/"

def check_file(path):
    if not os.path.isfile(path):
        return True
    
    with open(path, 'r') as f:
        content = f.read()
        
    # Check if Tier is in frontmatter
    match = re.search(r"^Tier: (.*)$", content, re.MULTILINE)
    if not match:
        print(f"Error: {path} is missing 'Tier' frontmatter.")
        return False
    
    tier = match.group(1).strip()
    valid_tiers = ["1", "2", "3", "4", "5", "shared", "internal"]
    if tier not in valid_tiers:
        print(f"Error: {path} has invalid Tier '{tier}'.")
        return False
        
    return True

def main():
    all_passed = True
    if not os.path.exists(DESIGN_DOCS_DIR):
        print(f"Dir {DESIGN_DOCS_DIR} not found.")
        sys.exit(1)
        
    for filename in os.listdir(DESIGN_DOCS_DIR):
        if filename.endswith(".md"):
            if not check_file(os.path.join(DESIGN_DOCS_DIR, filename)):
                all_passed = False
                
    if not all_passed:
        sys.exit(1)
    
    print("All design documents have valid Tier frontmatter.")

if __name__ == "__main__":
    main()

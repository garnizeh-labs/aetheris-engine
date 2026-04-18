#!/usr/bin/env python3
import os
import re
import sys

def check_links_in_file(filepath, all_files):
    errors = []
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()

    # Find [text](link) or [text]: link
    links = re.findall(r'\[.*?\]\((.*?)\)', content)
    links += re.findall(r'^\[.*?\]:\s*(.*?)$', content, re.MULTILINE)

    for link in links:
        link = link.strip().split(" ")[0].strip(" <>")
        if link.startswith('http') or link.startswith('#') or link.startswith('mailto:'):
            continue
        
        # Remove anchor
        link_path = link.split('#')[0]
        if not link_path:
            continue

        # Resolve relative to current file
        current_dir = os.path.dirname(filepath)
        target_path = os.path.normpath(os.path.join(current_dir, link_path))
        
        if not os.path.exists(target_path):
            errors.append(f"Broken link: {link} (Target: {target_path})")

    return errors

def main():
    docs_root = "." # Check from root
    failed = False
    
    all_files = []
    for root, dirs, files in os.walk(docs_root):
        # Prune excluded directories in-place to avoid descending into them
        dirs[:] = [d for d in dirs if d not in {'.git', 'target', 'jemalloc', 'node_modules', 'pkg'}]
        for f in files:
            all_files.append(os.path.normpath(os.path.join(root, f)))

    md_files = [f for f in all_files if f.endswith('.md')]

    for md_file in md_files:
        errors = check_links_in_file(md_file, all_files)
        if errors:
            print(f"❌ {md_file}:")
            for err in errors:
                print(f"  - {err}")
            failed = True
        else:
            # print(f"✅ {md_file}: OK")
            pass

    if failed:
        sys.exit(1)
    else:
        print("✅ All internal links are valid.")

if __name__ == "__main__":
    main()

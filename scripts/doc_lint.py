#!/usr/bin/env python3
import os
import sys
import re

REQUIRED_FIELDS = ["Version", "Status", "Phase", "Last Updated", "Authors"]
REQUIRED_SECTIONS = ["## Executive Summary", "## Appendix A — Glossary", "## Appendix B — Decision Log"]

def lint_file(filepath):
    errors = []
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()

    # Check for frontmatter
    frontmatter_match = re.search(r'^---\n(.*?)\n---', content, re.DOTALL)
    if not frontmatter_match:
        errors.append("Missing frontmatter (YAML block starting and ending with ---)")
    else:
        fm_content = frontmatter_match.group(1)
        for field in REQUIRED_FIELDS:
            if f"{field}:" not in fm_content:
                errors.append(f"Missing required frontmatter field: {field}")

    # Check for required sections (only for design files)
    if "design" in filepath.split(os.sep):
        for section in REQUIRED_SECTIONS:
            if section not in content:
                errors.append(f"Missing required section: {section}")

    return errors

def main():
    docs_root = "docs"
    design_dir = os.path.join(docs_root, "design")
    failed = False

    files_to_check = []
    for root, dirs, files in os.walk(docs_root):
        for f in files:
            if f.endswith(".md") and f != "README.md":
                files_to_check.append(os.path.join(root, f))

    for filepath in files_to_check:
        errors = lint_file(filepath)
        if errors:
            print(f"❌ {filepath}:")
            for err in errors:
                print(f"  - {err}")
            failed = True
        else:
            print(f"✅ {filepath}: OK")

    if failed:
        sys.exit(1)

if __name__ == "__main__":
    main()

#!/usr/bin/env python3
import sys
import os
import re

def toggle_patch(cargo_toml_path, enable=True):
    if not os.path.exists(cargo_toml_path):
        print(f"Error: {cargo_toml_path} not found.")
        return

    with open(cargo_toml_path, 'r') as f:
        lines = f.readlines()

    new_lines = []
    in_patch_block = False
    
    # Simple regex to find the [patch.crates-io] line or its commented version
    patch_pattern = re.compile(r'^(\s*#\s*)?\[patch\.crates-io\]')
    
    for line in lines:
        if patch_pattern.match(line):
            if enable:
                new_lines.append("[patch.crates-io]\n")
            else:
                new_lines.append("# [patch.crates-io]\n")
            in_patch_block = True
        elif in_patch_block and line.strip() == "":
            in_patch_block = False
            new_lines.append(line)
        elif in_patch_block:
            # Inside the block, we either comment or uncomment lines starting with specific crates
            # For this script, we assume anything inside the block should be toggled
            clean_line = line.lstrip('# ').lstrip()
            if enable:
                new_lines.append(clean_line)
            else:
                new_lines.append(f"# {clean_line}")
        else:
            new_lines.append(line)

    with open(cargo_toml_path, 'w') as f:
        f.writelines(new_lines)
    
    status = "ENABLED (local paths)" if enable else "DISABLED (crates.io)"
    print(f"Patch state for {cargo_toml_path}: {status}")

if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description="Toggle local path patches in Cargo.toml")
    parser.add_argument("--enable", action="store_true", help="Enable local patches")
    parser.add_argument("--disable", action="store_true", help="Disable local patches")
    parser.add_argument("--path", required=True, help="Path to Cargo.toml")
    
    args = parser.parse_args()
    
    if args.enable:
        toggle_patch(args.path, enable=True)
    elif args.disable:
        toggle_patch(args.path, enable=False)
    else:
        parser.print_help()

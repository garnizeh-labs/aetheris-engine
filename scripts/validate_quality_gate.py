#!/usr/bin/env python3
import json
import re
import sys
import os

def parse_report(report_path):
    metrics = {}
    if not os.path.exists(report_path):
        return metrics
    
    with open(report_path, 'r', encoding='utf-8') as f:
        content = f.read()
        
    # Extract values from the table
    # Format: | **ECS Extract (1k Entities)** | `322.95 µs` | < 2.5 ms | ✅ PASSED |
    table_pattern = r'\| \*\*([^*]+)\*\* \| `([^`]+)` \|'
    matches = re.findall(table_pattern, content)
    for name, value in matches:
        metrics[name.strip()] = value.strip()
        
    return metrics

def to_ms(value_str):
    if 'µs' in value_str:
        return float(value_str.replace('µs', '').strip()) / 1000.0
    if 'ms' in value_str:
        return float(value_str.replace('ms', '').strip())
    if 'ns' in value_str:
        return float(value_str.replace('ns', '').strip()) / 1000000.0
    if 's' in value_str:
        return float(value_str.replace('s', '').strip()) * 1000.0
    return 0.0

def validate():
    gate_path = 'PHASE-1-QUALITY-GATE.json'
    if not os.path.exists(gate_path):
        print(f"Error: {gate_path} not found.")
        sys.exit(1)

    with open(gate_path, 'r', encoding='utf-8') as f:
        gate = json.load(f)
    
    # Find the latest report
    benchmarks_dir = 'benchmarks'
    if not os.path.exists(benchmarks_dir):
        print("No benchmarks directory found.")
        return

    reports = sorted([d for d in os.listdir(benchmarks_dir) if os.path.isdir(os.path.join(benchmarks_dir, d)) and d != '_template'])
    if not reports:
        print("No benchmark reports found in benchmarks/.")
        return
    
    latest_report_dir = os.path.join(benchmarks_dir, reports[-1])
    report_path = os.path.join(latest_report_dir, 'REPORT.md')
    if not os.path.exists(report_path):
        print(f"Error: {report_path} not found.")
        sys.exit(1)

    print(f"Validating quality gate against report: {report_path}")
    
    metrics = parse_report(report_path)
    
    # Thresholds
    perf_thresholds = gate['thresholds']['performance']
    max_tick_ms = perf_thresholds['max_tick_duration_ms']
    
    total_bench_ms = 0.0
    found_any = False
    
    # Mapping JSON bench keys to REPORT.md display names
    key_map = {
        "ecs_extract_dirty_7_of_1000": "ECS Extract (1k Entities)",
        "tick_scheduler_step_noop": "Tick Scheduler Overhead",
        "encode_rmp_serde_transform": "MessagePack Encoding"
    }
    
    for bench_key in gate['benchmarks']:
        display_name = key_map.get(bench_key, bench_key)
            
        if display_name in metrics:
            val_ms = to_ms(metrics[display_name])
            total_bench_ms += val_ms
            print(f"  - {display_name}: {metrics[display_name]} -> {val_ms:.4f} ms")
            found_any = True
        else:
            print(f"  - Warning: Benchmark '{bench_key}' ({display_name}) not found in report.")
            
    if not found_any:
        print("Error: No benchmarks found in report to validate aggregate budget.")
        sys.exit(1)

    print(f"Aggregate Benchmark Time: {total_bench_ms:.4f} ms")
    print(f"Max Tick Duration Budget: {max_tick_ms} ms")
    
    if total_bench_ms > max_tick_ms:
        print(f"❌ QUALITY GATE BREACH: Aggregate benchmark time ({total_bench_ms:.4f} ms) exceeds budget ({max_tick_ms} ms)")
        sys.exit(1)
    else:
        print("✅ QUALITY GATE PASSED: Aggregate benchmark time is within budget.")

if __name__ == "__main__":
    validate()

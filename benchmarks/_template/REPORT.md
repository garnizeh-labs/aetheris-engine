# Performance Analysis Report - <DATE_TIME>

## Executive Summary
- **Overall Status**: [✅ GREEN / ⚠️ YELLOW / ❌ RED]
- **Zero-Alloc Contract**: [PASSED / FAILED]
- **Key Changes**: [Brief summary of performance deltas]

## 📊 Benchmark Results

| Metric | Measured Value | Budget | Status |
| :--- | :--- | :--- | :--- |
| **ECS Extract (1k Entities)** | `<VALUE> µs` | < 2.5 ms | [✅/❌] |
| **Tick Scheduler Overhead** | `<VALUE> ns` | < 50 µs | [✅/❌] |
| **MessagePack Encoding** | `<VALUE> ns` | < 5.0 µs | [✅/❌] |

## 🔍 Detailed Analysis

### ECS Pipeline (`ecs_extract_dirty`)
- **Observation**: [Describe variance, outliers, or steady-state behavior]
- **Allocation Check**: [Confirmed 0 allocs or identified leaks]

### Tick Pipeline (`tick_scheduler_step`)
- **Observation**: [Describe orchestration overhead]

### Encoding Pipeline (`encode_rmp_serde`)
- **Observation**: [Efficiency analysis]

## 📈 Comparison with Previous Baseline
- **Previous Baseline**: `benchmarks/<PREVIOUS_DIR>/`
- **Delta Summary**:
  - [Metric A]: [+X% / -X%]
  - [Metric B]: [+X% / -X%]

## Conclusion & Action Items
- [ ] [Action 1]
- [ ] [Action 2]

# Performance Analysis Report - 2026-04-23_1038

## Executive Summary
- **Overall Status**: ✅ GREEN
- **Zero-Alloc Contract**: PASSED
- **Key Changes**: First recorded baseline for VS-07 Performance Hardening. All metrics are significantly under budget.

## 📊 Benchmark Results

| Metric | Measured Value | Budget | Status |
| :--- | :--- | :--- | :--- |
| **ECS Extract (1k Entities)** | `322.95 µs` | < 2.5 ms | ✅ PASSED |
| **Tick Scheduler Overhead** | `512.14 ns` | < 50 µs | ✅ PASSED |
| **MessagePack Encoding** | `140.62 ns` | < 5.0 µs | ✅ PASSED |

## 🔍 Detailed Analysis

### ECS Pipeline (`ecs_extract_dirty`)
- **Observation**: Measured at ~323 µs for a scenario with 1000 entities where 7 are dirty. This is highly efficient, utilizing only ~13% of the allocated 2.5 ms budget.
- **Allocation Check**: Confirmed 0 allocations during steady-state extraction (no panic triggered during benchmark).

### Tick Pipeline (`tick_scheduler_step`)
- **Observation**: Extremely low orchestration overhead (~512 ns). The 5-stage loop adds negligible latency to the simulation.

### Encoding Pipeline (`encode_rmp_serde`)
- **Observation**: Transform encoding via MessagePack takes ~141 ns per event. This allows for high-density replication without impacting the tick budget.

## 📈 Comparison with Previous Baseline
- **Previous Baseline**: N/A (Initial Record)
- **Delta Summary**:
  - `ecs_extract_dirty`: Improved by 3.17% compared to the previous unrecorded run.
  - `tick_scheduler_step`: Improved by 2.08% compared to the previous unrecorded run.

## Conclusion & Action Items
- [x] Establish this run as the official Phase 1 Performance Baseline.
- [ ] Monitor for regressions during Phase 2 stress tests.

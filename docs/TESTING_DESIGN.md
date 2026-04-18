---Version: 0.2.0-draft
Status: Phase 1 — MVP / Phase 2 — Specified
Phase: P1 | P2 | P3
Last Updated: 2026-04-15
Authors: Team (Antigravity)
Spec References: [QA-1000]
Tier: 2
---

# Aetheris Testing — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Testing Philosophy](#2-testing-philosophy)
3. [Test Pyramid](#3-test-pyramid)
4. [Unit Tests — Crate-Level Isolation](#4-unit-tests--crate-level-isolation)
5. [Integration Tests — Cross-Crate Contracts](#5-integration-tests--cross-crate-contracts)
6. [Smoke Tests — End-to-End Verification](#6-smoke-tests--end-to-end-verification)
7. [Stress Tests — Capacity & Degradation](#7-stress-tests--capacity--degradation)
8. [Benchmark Suite — Performance Regression](#8-benchmark-suite--performance-regression)
9. [Test Doubles — Mock Infrastructure](#9-test-doubles--mock-infrastructure)
10. [CI Pipeline — Automated Quality Gate](#10-ci-pipeline--automated-quality-gate)
11. [WASM Testing — Browser-Specific Validation](#11-wasm-testing--browser-specific-validation)
12. [Security Testing](#12-security-testing)
13. [Performance Contracts](#13-performance-contracts)
14. [Open Questions](#14-open-questions)
15. [Appendix A — Glossary](#appendix-a--glossary)
16. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

Aetheris follows a **test-driven, measurement-gated development model**. Every phase transition (P1 → P2 → P3) is justified by telemetry data gathered from the test suite, not by intuition. The testing architecture is designed to validate three properties at every scale:

1. **Correctness:** Does the engine produce the expected output for a given input? (Unit + Integration tests)
2. **Contract compliance:** Do trait implementations satisfy the invariants specified in the OpenSpec? (Contract tests)
3. **Performance:** Does the engine meet its 16.6 ms tick budget under load? (Benchmarks + Stress tests)

### Test Infrastructure Summary

| Layer | Tool | Scope | Frequency |
|---|---|---|---|
| **Unit** | `cargo nextest` | Single crate, single function | Every commit |
| **Integration** | `cargo nextest` | Cross-crate, full tick pipeline | Every commit |
| **Smoke** | `aetheris-smoke-test` CLI | Live server, WebTransport roundtrip | Pre-merge, nightly |
| **Stress** | Docker Compose + headless bots | 500–5,000 concurrent clients | Nightly, milestone gate |
| **Benchmark** | `criterion 0.8` | Microbenchmarks, regression detection | Every commit (compare), milestone (baseline) |
| **Lint** | `clippy`, `cargo deny`, `cargo audit` | Code quality, license, CVE | Every commit |
| **Doc** | `scripts/doc_lint.py`, `scripts/check_links.py` | Design doc integrity | Every commit |

---

## 2. Testing Philosophy

### 2.1 The "Measure Before You Optimize" Principle

From [ENGINE_DESIGN.md](ENGINE_DESIGN.md):

> **Every phase transition is justified by telemetry, not intuition. Libraries are only replaced when metrics prove they are the bottleneck.**

The testing suite generates the data that gates these transitions:

- **P1 → P2:** Integration tests prove correctness. Stress tests prove that 100 clients work.
- **P2 → P3:** Benchmark baselines prove which subsystem (ECS, encoder, transport) is the bottleneck. Only the bottleneck is replaced.
- **P3 regression:** The same integration tests run unchanged against the P3 implementations. If any test fails, the swap is blocked.

### 2.2 Test Doubles as First-Class Citizens

The `aetheris-protocol` crate ships test doubles (mock transport, mock world state, mock encoder) behind the `test-utils` feature flag. These are not afterthoughts — they are specified in [PROTOCOL_DESIGN.md](PROTOCOL_DESIGN.md) and must satisfy the same trait contracts as the real implementations.

 ```texttoml
# In any test crate's Cargo.toml:
[dev-dependencies]
aetheris-protocol = { path = "../aetheris-protocol", features = ["test-utils"] }
 ```text

---

## 3. Test Pyramid

 ```text
        ╱ ╲
       ╱   ╲         Smoke/Stress Tests (few, expensive, high-fidelity)
      ╱ E2E ╲        Live server + headless bots
     ╱───────╲
    ╱         ╲       Integration Tests (moderate, cross-crate)
   ╱Integration╲     Full tick pipeline with mocks
  ╱─────────────╲
 ╱               ╲    Unit Tests (many, fast, isolated)
╱     Unit        ╲   Single function, pure logic
╱───────────────────╲
 ```text

### 3.1 Distribution Targets

| Layer | Target Count (P1) | Execution Time | Isolation |
|---|---|---|---|
| Unit | 200+ tests | < 30s total | No I/O, no network, no filesystem |
| Integration | 20–50 tests | < 60s total | Mock transports, in-memory ECS |
| Smoke | 5–10 scenarios | < 120s total | Real server, real network |
| Stress | 3–5 scenarios | 5–30 min | Docker, concurrent load |

---

## 4. Unit Tests — Crate-Level Isolation

### 4.1 Protocol Crate Tests

The `aetheris-protocol` crate tests type conversions, error construction, and `NetworkIdAllocator` correctness:

 ```textrust
#[test]
fn network_id_allocator_is_monotonic() {
    let alloc = NetworkIdAllocator::new();
    let a = alloc.allocate();
    let b = alloc.allocate();
    assert!(b.0 > a.0);
    assert_eq!(a.0, 1); // 0 is reserved as null sentinel
}

#[test]
fn component_kind_round_trip() {
    let kind = ComponentKind(42);
    let bytes = kind.0.to_le_bytes();
    let decoded = ComponentKind(u16::from_le_bytes(bytes));
    assert_eq!(kind, decoded);
}
 ```text

### 4.2 Encoder Crate Tests

Each encoder implementation tests the encode/decode round-trip for all known component types:

 ```textrust
#[test]
fn serde_encoder_round_trip_position() {
    let encoder = SerdeEncoder;
    let event = ReplicationEvent {
        network_id: NetworkId(42),
        component_kind: ComponentKind(1),
        payload: vec![/* position bytes */],
        tick: 5024,
    };
    let mut buf = [0u8; 1200];
    let len = encoder.encode(&event, &mut buf).unwrap();
    let decoded = encoder.decode(&buf[..len]).unwrap();
    assert_eq!(decoded.network_id, event.network_id);
    assert_eq!(decoded.tick, event.tick);
}
 ```text

### 4.3 ECS Adapter Tests

The `BevyWorldAdapter` tests verify all invariants from the LC-0400 spec:

- **B1 — Bijection:** Every `NetworkId` maps to exactly one `LocalId`, and vice versa.
- **B2 — Atomicity:** `spawn_networked` atomically inserts into both the ECS and the bimap.
- **B3 — Immutability:** Once inserted, the NetworkId ↔ LocalId mapping never changes.
- **B4 — No recycling:** Despawned `NetworkId`s are never reused.
- **D3 — Idempotent extraction:** Two consecutive calls to `extract_deltas()` with no mutations return `[]` for the second.

---

## 5. Integration Tests — Cross-Crate Contracts

### 5.1 Server Integration Suite

Located in `crates/aetheris-server/tests/server_integration.rs`, these tests exercise the full tick pipeline with mock and real components:

| Test | What It Validates |
|---|---|
| `test_grpc_auth_flow` | Full gRPC auth roundtrip: success + invalid credentials |
| `test_server_loop_1000_ticks` | 1,000-tick burn-in with mock transport/ECS/encoder — no panics, no leaks |
| `test_client_connect_and_replication` | Client connects → entity spawned → delta queued → broadcast verified |
| `test_full_integration_suite` | Connect, spawn, despawn, reliable+unreliable message injection |
| `test_consecutive_dropped_packets_interpolation` | 10 ticks of deltas verify packet tick ordering |
| `test_wasm_mtu_handling_simulation` | Oversized packet handling — server does not crash |

### 5.2 Tick Pipeline Contract Test

The most critical integration test verifies that the five-stage tick pipeline produces the correct output:

 ```text
Given: 1 connected client, 1 spawned entity, entity Position changed
When:  1 tick executes (poll → apply → simulate → extract → send)
Then:  Client receives exactly 1 unreliable packet containing the Position delta
 ```text

### 5.3 Phase-Swap Regression Test

Before the P1 → P3 swap, the entire integration suite runs against both implementations:

 ```text
cargo nextest run --features phase1   # All integration tests pass against P1
cargo nextest run --features phase3   # All integration tests pass against P3
 ```text

If any test fails against P3, the swap is blocked until the P3 implementation is fixed. The game loop code is identical in both runs — only the selected trait implementations change.

---

## 6. Smoke Tests — End-to-End Verification

### 6.1 The `aetheris-smoke-test` Crate

The smoke test crate is a standalone binary with `clap`-based CLI:

 ```textbash
# Test single WebTransport connection (Ping/Pong roundtrip)
cargo run -p aetheris-smoke-test -- webtransport

# Test game client flow
cargo run -p aetheris-smoke-test -- client

# Concurrent stress with 50 bots for 30 seconds
cargo run -p aetheris-smoke-test -- stress --count 50 --duration 30
 ```text

### 6.2 WebTransport Smoke Test

The `webtransport` subcommand validates end-to-end connectivity:

1. Opens a WebTransport connection to `https://localhost:4433`.
2. Sends a Ping datagram with the current tick.
3. Waits for a Pong response.
4. Measures RTT.
5. Reports success/failure.

### 6.3 Stress Smoke Test

The `stress` subcommand validates concurrent client handling:

1. Spawns `--count` concurrent clients with staggered ramp-up (100 ms between each).
2. Each client performs a Ping/Pong roundtrip per second.
3. Measures per-round RTT and reports statistics.
4. Fails if any client cannot connect or if median RTT exceeds threshold.

---

## 7. Stress Tests — Capacity & Degradation

### 7.1 Docker-Based Load Testing

The stress test infrastructure runs in Docker for reproducibility:

 ```textbash
just stress-docker  # Launches docker-compose.stress.yml
 ```text

The `docker-compose.stress.yml` defines:

- **aetheris-server** container (built from `Dockerfile.server`)
- **stress-client** container(s) (built from `Dockerfile.stress`, runs headless bots)
- **Prometheus** + **Grafana** for live metrics during the test

### 7.2 Stress Test Scenarios

| Scenario | Clients | Duration | Success Criteria |
|---|---|---|---|
| **Ramp-Up** | 0 → 500 over 60s | 5 min | p99 tick < 16.6 ms throughout |
| **Steady State** | 500 constant | 10 min | Zero tick overruns, zero client disconnects |
| **Spike** | 500 → 2,000 in 5s | 5 min | Graceful degradation, no crash |
| **Churn** | 500, 50% reconnect/sec | 5 min | No memory leak, entity count stable |
| **Worst Case** | 5,000 bots | 30 min | Identify bottleneck for P3 justification |

### 7.3 Success Metrics

Stress tests are evaluated against the following thresholds:

| Metric | Threshold | Source |
|---|---|---|
| `aetheris_tick_duration_seconds` p99 | < 16.6 ms | Prometheus |
| `aetheris_connected_clients` | Matches expected bot count | Prometheus |
| `aetheris_ecs_extract_duration_ms` p99 | < 2.5 ms | Prometheus |
| Memory (RSS) growth rate | < 1 MB/min at steady state | System metrics |
| Crash count | 0 | Process exit code |

---

## 8. Benchmark Suite — Performance Regression

### 8.1 Criterion Benchmarks

Located in `crates/aetheris-benches/`, the benchmark suite measures the hot path:

| Benchmark | What It Measures |
|---|---|
| `encode_position` | `Encoder::encode()` for a Position component |
| `decode_position` | `Encoder::decode()` for a Position component |
| `extract_deltas_100` | `WorldState::extract_deltas()` with 100 entities |
| `extract_deltas_1000` | `WorldState::extract_deltas()` with 1,000 entities |
| `full_tick_100` | Complete 5-stage tick with 100 entities |
| `full_tick_1000` | Complete 5-stage tick with 1,000 entities |

### 8.2 Baseline Management

Baselines are recorded and compared via Python scripts:

 ```textbash
just bench-record   # Runs benchmarks, saves to benches/baseline.json
just bench-check    # Compares current run against baseline, 15% threshold
 ```text

The `scripts/bench_baseline.py` collects Criterion estimates and records metadata (rustc version, CPU model, OS, timestamp). The `scripts/bench_compare.py` compares current benchmarks against the baseline and exits non-zero if any benchmark regresses by more than 15%.

### 8.3 CI Benchmark Gate

On every PR, the CI pipeline:

1. Runs `just bench-check` against the committed `benches/baseline.json`.
2. If any benchmark regresses by > 15%, the PR is blocked.
3. If the PR intentionally changes performance characteristics, the author runs `just bench-record` and commits the new baseline.

---

## 9. Test Doubles — Mock Infrastructure

### 9.1 `MockTransport`

Located in `aetheris-protocol/src/test_doubles.rs` (behind `#[cfg(feature = "test-utils")]`):

- Records per-client sent packets for assertion.
- Provides an inbound event queue for injecting `NetworkEvent`s.
- Implements full `GameTransport` trait.

### 9.2 `MockWorldState`

- Uses a `BiHashMap<NetworkId, LocalId>` for identity mapping.
- Provides `queue_delta()` helper to pre-load `ReplicationEvent`s for `extract_deltas()`.
- Tracks all `apply_updates()` calls for assertion.

### 9.3 `MockEncoder`

- Uses a sentinel header byte (`0xAE`) for identification.
- Dummy encode/decode that passes through component data.
- Validates buffer bounds.

### 9.4 Design Principle

Test doubles satisfy the same trait invariants as real implementations. A test that passes with `MockTransport` must also pass with `RenetTransport` — the mock is a simplification, not a behavioral divergence.

---

## 10. CI Pipeline — Automated Quality Gate

### 10.1 The `just check` Gate

Every PR must pass `just check`, which runs the complete quality gate:

 ```textbash
just check
# Equivalent to:
#   cargo fmt --all -- --check
#   cargo clippy --workspace --all-targets -- -D warnings
#   cargo deny check
#   cargo audit
#   cargo nextest run --workspace
#   python3 scripts/doc_lint.py
#   python3 scripts/check_links.py
 ```text

### 10.2 Gate Stages

| Stage | Tool | Blocks PR If... |
|---|---|---|
| **Format** | `rustfmt` (edition 2024, max_width 100) | Any file is not formatted |
| **Lint** | `clippy` (`-D warnings`, wildcard import warnings) | Any lint warning |
| **License** | `cargo deny` | Unapproved license in dependency tree |
| **Advisory** | `cargo audit` | Known CVE in dependency (RUSTSEC) |
| **Unit + Integration** | `cargo nextest` | Any test failure |
| **Doc Lint** | `scripts/doc_lint.py` | Missing frontmatter, missing required sections |
| **Link Check** | `scripts/check_links.py` | Broken internal markdown links |
| **Benchmark** | `just bench-check` | > 15% regression vs. baseline |

### 10.3 Nightly Pipeline

The nightly build adds:

- Full stress test suite (Docker-based, 500 bots × 10 min).
- WASM build + smoke test (`just build-wasm && just smoke`).
- Dependency update check (`cargo update --dry-run`).

---

## 11. WASM Testing — Browser-Specific Validation

### 11.1 WASM Build Verification

The WASM client (`aetheris-client-wasm`) builds with a pinned nightly toolchain (`nightly-2025-07-01`) and specific target features:

 ```textbash
RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals,+shared-memory' \
  cargo build -p aetheris-client-wasm --target wasm32-unknown-unknown --release
 ```text

The CI pipeline verifies this build succeeds on every commit.

### 11.2 SharedArrayBuffer Compatibility

The `SharedArrayBuffer` double-buffer scheme requires:

- `Cross-Origin-Opener-Policy: same-origin` header
- `Cross-Origin-Embedder-Policy: require-corp` header

These headers are set in the Vite dev server (`playground/vite.config.ts`) and verified by the smoke test.

### 11.3 Binary Size Gate

| Target | Max Size | Measurement |
|---|---|---|
| `aetheris_client_wasm_bg.wasm` (gzip) | 1.2 MB | `wc -c` after `wasm-opt` + gzip |

The WASM binary is optimized with `opt-level = 'z'`, LTO, symbol stripping, `panic = "abort"`, and a `wasm-opt` pass. If the gzipped binary exceeds 1.2 MB, the build is flagged for size reduction.

---

## 12. Security Testing

### 12.1 Dependency Auditing

`cargo audit` checks the dependency tree against the RustSec advisory database. `cargo deny` validates licenses and bans known-problematic crates. Both run on every commit.

### 12.2 Fuzzing (P2 Target)

Planned fuzz targets for the encoder:

- `fuzz_decode`: Random bytes → `Encoder::decode()` must never panic.
- `fuzz_encode_decode`: Random `ReplicationEvent` → encode → decode → compare.
- `fuzz_network_event`: Random bytes → `Encoder::decode_event()` must never panic.

### 12.3 Invariant Violation Testing

Integration tests deliberately inject invalid data to verify the security layer:

- Oversized packets (violate MTU).
- Unknown `ComponentKind` values.
- Packets with `NetworkId`s not owned by the sending client.

---

## 13. Performance Contracts

| Operation | Budget | Target |
|---|---|---|
| Full CI gate (`just check`) | < 5 min | Developer feedback loop |
| Unit + integration tests | < 90 s | Fast iteration |
| Single benchmark run | < 60 s | Reasonable PR overhead |
| Nightly stress test | < 30 min | Overnight completion |
| WASM build | < 120 s | Acceptable CI time |

---

## 14. Open Questions

| Question | Context | Impact |
|---|---|---|
| **Smoke Test Coverage** | What is the minimum set of features to be covered by automated smoke tests? | Release stability. |
| **Fuzz Corpus Management** | Where should fuzz corpora be stored? Committed or generated? | Reproducibility vs. repository size. |
| **Browser Automation** | Should smoke tests include headless Chrome/Firefox via Playwright? | WebTransport validation in real browsers. |
| **Load Test Baseline** | At what client count does P1 consistently fail the 16.6 ms contract? | P2 → P3 transition data. |

---

## Appendix A — Glossary

### Mini-Glossary (Quick Reference)

- **Smoke Test**: A fast, end-to-end test verifying the engine can start, accept connections, and replicate state.
- **Stress Test**: A long-running load test with hundreds or thousands of concurrent clients.
- **Benchmark Baseline**: A recorded set of performance measurements used as the regression reference.
- **Test Double**: A mock, stub, or fake implementation of a trait used for testing.
- **Nextest**: A faster Rust test runner that parallelizes test execution and provides better output.
- **Criterion**: A statistics-driven benchmarking framework for Rust.
- **Fuzz Target**: An entry point fed random data to discover panics and undefined behavior.

[Full Glossary Document](../GLOSSARY.md)

---

## Appendix B — Decision Log

| # | Decision | Rationale | Revisit If... | Date |
|---|---|---|---|---|
| D1 | `cargo nextest` over `cargo test` | Faster parallel execution, better failure output.  | `cargo test` improves significantly. | 2026-04-15 |
| D2 | Criterion for benchmarks | Statistics-driven, supports baselines, CI-friendly. | A Rust-native benchmark framework with better ergonomics emerges. | 2026-04-15 |
| D3 | 15% regression threshold | Balances noise tolerance against real regression detection. | Too many false positives or missed regressions. | 2026-04-15 |
| D4 | Test doubles in `aetheris-protocol` | Co-located with traits: mocks evolve when traits evolve. | Mock complexity warrants a separate crate. | 2026-04-15 |
| D5 | Docker for stress tests | Reproducible, isolated, matches production topology. | Bare-metal tests needed for kernel-level profiling. | 2026-04-15 |
| D6 | Doc lint in CI | Design docs are code — they must pass quality gates. | Doc lint false positives exceed value. | 2026-04-15 |

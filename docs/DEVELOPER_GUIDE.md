---Version: 0.2.0-draft
Status: Phase 1 — MVP
Phase: All
Last Updated: 2026-04-15
Authors: Team (Antigravity)
Spec References: [LC-0100]
Tier: 2
---

# Aetheris Developer Guide — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Prerequisites](#2-prerequisites)
3. [Repository Structure](#3-repository-structure)
4. [Local Setup](#4-local-setup)
5. [Building the Project](#5-building-the-project)
6. [Running the Server](#6-running-the-server)
7. [Running the Client](#7-running-the-client)
8. [Development Workflow](#8-development-workflow)
9. [Observability Setup](#9-observability-setup)
10. [Testing](#10-testing)
11. [Benchmarking](#11-benchmarking)
12. [Common Tasks](#12-common-tasks)
13. [Contribution Process](#13-contribution-process)
14. [Troubleshooting](#14-troubleshooting)
15. [Open Questions](#15-open-questions)
16. [Appendix A — Glossary](#appendix-a--glossary)
17. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

This guide covers everything needed to set up a local Aetheris development environment, build and run the server and client, run tests, and contribute code. The project uses Rust (Edition 2024) with a multi-crate workspace and a TypeScript/Vite frontend for the browser client.

**Fastest path to a running system:**

```bash
git clone https://github.com/garnizeh/aetheris.git
cd aetheris
just dev
# → Server running, WASM built, Vite at http://localhost:5173
```

---

## 2. Prerequisites

### 2.1 Required Tools

| Tool | Version | Installation |
|---|---|---|
| Rust | 1.94.1+ (Edition 2024) | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Rust nightly | `nightly-2025-07-01` | Auto-installed via `rust-toolchain.toml` for WASM builds |
| `wasm32-unknown-unknown` target | — | `rustup target add wasm32-unknown-unknown` |
| Just | Latest | `cargo install just` or via package manager |
| Node.js | 18+ | For Vite dev server |
| Docker & Docker Compose | v24+ / v2+ | For observability stack |

### 2.2 Recommended Tools

| Tool | Purpose | Installation |
|---|---|---|
| `cargo-deny` | Dependency license/advisory checks | `cargo install cargo-deny` |
| `cargo-nextest` | Faster test runner | `cargo install cargo-nextest` |
| `cargo-audit` | Security vulnerability scan | `cargo install cargo-audit` |
| `git-cliff` | Changelog generation | `cargo install git-cliff` |
| `wasm-bindgen-cli` | WASM binding generation | `cargo install wasm-bindgen-cli` |

### 2.3 Rust Toolchain

The project pins the Rust toolchain via `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.94.1"
components = ["rustfmt", "clippy"]
targets = ["wasm32-unknown-unknown"]
```

WASM builds require nightly for `atomics`, `bulk-memory`, `mutable-globals`, and `shared-memory` features.

---

## 3. Repository Structure

### 3.1 Crate Map

```text
crates/
├── aetheris-protocol/          # Core traits, types, events, errors
├── aetheris-server/            # Authoritative game server binary
├── aetheris-transport-renet/   # Phase 1 UDP transport (Renet)
├── aetheris-transport-webtransport/  # WebTransport (browser clients)
├── aetheris-transport-quinn/   # Phase 3 QUIC transport (Quinn)
├── aetheris-ecs-bevy/          # Phase 1 ECS (Bevy)
├── aetheris-ecs-custom/        # Phase 3 ECS (custom archetype)
├── aetheris-encoder-serde/     # Phase 1 encoder (MessagePack)
├── aetheris-encoder-bitpack/   # Phase 3 encoder (custom bitpack)
├── aetheris-client-wasm/       # WASM browser client
├── aetheris-client-native/     # Native desktop client (P3)
├── aetheris-benches/           # Criterion benchmarks
└── aetheris-smoke-test/        # Smoke, stress, and integration tests
```

### 3.2 Key Directories

| Directory | Purpose |
|---|---|
| `playground/` | TypeScript/Vite frontend — official engine playground |
| `docker/` | Dockerfiles, Compose files, observability config |
| `docs/` | Design documents, getting started, roadmap |
| `openspec/` | OpenSpec governance documents |
| `scripts/` | Python utilities (bench comparison, link checking) |
| `benches/` | Benchmark baseline JSON files |

### 3.3 Phase Architecture

The Trait Triad (GameTransport, WorldState, Encoder) has two implementations selected by Cargo feature flags:

| Component | Phase 1 (`--features phase1`) | Phase 3 (`--features phase3`) |
|---|---|---|
| Transport | Renet + WebTransport | Quinn QUIC |
| ECS | Bevy `bevy_ecs 0.18` | Custom archetype store |
| Encoder | `rmp-serde` (MessagePack) | Custom bitpack |

Default is `phase1`. The flags are mutually exclusive (`compile_error!` enforced).

---

## 4. Local Setup

### 4.1 Clone and Build

```bash
git clone https://github.com/garnizeh/aetheris.git
cd aetheris

# Install Node.js dependencies for the playground
just client-install    # or: cd playground && npm install

# Build the server (Phase 1)
cargo build -p aetheris-server

# Build everything
cargo build --workspace
```

### 4.2 WebTransport Certificates

WebTransport requires TLS even for local development. Self-signed certificates are auto-generated on first server start and saved to `target/dev-certs/`:

```
target/dev-certs/
├── cert.pem    # Valid for 13 days
└── key.pem
```

The SHA-256 certificate hash is injected into the WASM client at build time via `VITE_SERVER_CERT_HASH`. If the certificate expires:

```bash
just clean-certs   # Delete old certificates
just server        # Regenerates certificates on startup
just wasm-dev && just client-build   # Rebuild client with new hash
```

---

## 5. Building the Project

### 5.1 Just Recipes

| Command | Description |
|---|---|
| `just build` | Build entire workspace |
| `just wasm` | Build WASM client (release, nightly) |
| `just wasm-dev` | Build WASM client (debug, nightly) |
| `just client-build` | Build TypeScript frontend |
| `just docker-build` | Build Docker image for server |

### 5.2 WASM Build Details

The WASM build uses nightly Rust with specific target features:

```bash
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals,+shared-memory" \
  cargo +nightly-2025-07-01 build \
    --target wasm32-unknown-unknown \
    -p aetheris-client-wasm \
    --release
```

After compilation, `wasm-bindgen` generates JavaScript bindings in `pkg/`.

### 5.3 Release Build

```bash
cargo build -p aetheris-server --release
# Binary at: target/release/aetheris-server
```

The release profile is configured for minimum size: `opt-level = 'z'`, LTO, symbol stripping, `panic = "abort"`.

---

## 6. Running the Server

### 6.1 Quick Start

```bash
just server          # Debug build, default config
just server-release  # Release build
just server-obs      # With JSON logging + OTLP tracing
```

### 6.2 Server Endpoints

| Port | Protocol | Service |
|---|---|---|
| `5000/udp` | Renet | Native client transport |
| `4433/udp` | WebTransport/QUIC | Browser client transport |
| `50051/tcp` | gRPC | Auth service (Control Plane) |
| `9000/tcp` | HTTP | Prometheus metrics |

### 6.3 Environment Variables

```bash
AETHERIS_TICK_RATE=60           # Tick rate in Hz (default: 60)
AETHERIS_METRICS_PORT=9000      # Prometheus port (default: 9000)
RUST_LOG=info                   # Log level filter
LOG_FORMAT=json                 # json or text (default: text)
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317  # Jaeger OTLP
```

---

## 7. Running the Client

### 7.1 Browser Client

```bash
just dev    # Server + WASM + Vite all-in-one
# OR individually:
just server &
just wasm-dev
just client-dev
# → Open http://localhost:5173
```

### 7.2 Client Architecture

The browser client uses a Web Worker architecture:

```
Main Thread (UI/DOM) ←→ Game Worker (WASM) ←→ Render Worker (WebGPU)
```

- **Game Worker**: Runs the WASM binary, handles network (WebTransport), manages game state.
- **Render Worker**: Receives entity data via `SharedArrayBuffer`, renders via `wgpu`/WebGPU on `OffscreenCanvas`.

---

## 8. Development Workflow

### 8.1 Typical Development Cycle

```bash
# 1. Start the full dev environment
just dev

# 2. Make code changes

# 3. Run the quality gate
just check   # format + clippy + test + wasm + security + docs + bench + udeps

# 4. Commit (Conventional Commits)
git commit -m "feat(transport): add MTU probing support"
```

### 8.2 `just check` — The PR Gate

```bash
just check
# Runs in order:
#   1. fmt: cargo fmt --all --check
#   2. clippy: cargo clippy --workspace --all-targets -- -D warnings
#   3. test: cargo nextest run --workspace
#   4. wasm: cargo +nightly build --target wasm32-unknown-unknown
#   5. security: cargo deny check + cargo audit
#   6. docs-check: doc_lint.py + check_links.py + codespell
#   7. docs-strict: cargo doc -D warnings
#   8. bench-check: performance regression validator
#   9. udeps: unused dependency check
```

Every PR must pass `just check` before merge.

### 8.3 Code Style

- **Rust Edition 2024**, stable channel (except WASM).
- **Clippy**: `clippy::all` + `clippy::pedantic`, zero warnings.
- **Formatting**: `rustfmt` with project-specific config (`rustfmt.toml`).
- **Comments**: "Professorial Comments" — explain **why**, not what. Complex algorithms get detailed explanations.
- **Naming**: Snake_case for functions/variables, PascalCase for types, SCREAMING_SNAKE for constants.

---

## 9. Observability Setup

### 9.1 Start Observability Stack

```bash
just infra-up
# Starts: Prometheus, Grafana, Jaeger, Loki, Promtail
```

### 9.2 Access Points

| Service | URL | Purpose |
|---|---|---|
| Grafana | `http://localhost:3000` | Dashboards (no login needed) |
| Jaeger | `http://localhost:16686` | Distributed tracing |
| Prometheus | `http://localhost:9090` | Metrics queries |

### 9.3 Server with Observability

```bash
just server-obs
# Starts server with LOG_FORMAT=json and OTEL_EXPORTER_OTLP_ENDPOINT pointed at Jaeger
```

### 9.4 Key Metrics

| Metric | Description |
|---|---|
| `aetheris_tick_duration_seconds` | Histogram of total tick time |
| `aetheris_tick_stage_duration_seconds{stage}` | Per-stage tick timing |
| `aetheris_connected_clients` | Current client count |
| `aetheris_transport_errors_total{kind}` | Transport errors by type |

### 9.5 Teardown

```bash
just infra-down     # Stop observability stack
just infra-reset    # Stop + remove volumes + restart
```

---

## 10. Testing

### 10.1 Test Commands

```bash
just test             # Run all tests with nextest
cargo nextest run     # Equivalent
cargo test            # Standard test runner (fallback)
```

### 10.2 Test Categories

| Category | Location | Runner |
|---|---|---|
| Unit tests | `#[cfg(test)]` in each crate | `cargo nextest` |
| Integration tests | `crates/aetheris-server/tests/` | `cargo nextest` |
| Smoke tests | `crates/aetheris-smoke-test/` | Binary: `just smoke` |
| Stress tests | `crates/aetheris-smoke-test/` | `just stress [count] [duration]` |

### 10.3 Test Doubles

The `test-utils` feature on `aetheris-protocol` enables mocks:

```rust
// In test files:
use aetheris_protocol::test_doubles::{MockTransport, MockWorldState, MockEncoder};
```

### 10.4 Stress Testing

```bash
# Local (binary)
just stress 1000 60      # 1000 clients, 60 seconds

# Docker (isolated, with observability)
just observe-stress 25000 400   # 25000 clients, 400 seconds, with Grafana
```

---

## 11. Benchmarking

### 11.1 Benchmark Commands

```bash
just bench                # Run Criterion benchmarks
just bench-record         # Record new baseline
just bench-check          # Compare against baseline
```

### 11.2 Benchmark Baselines

- `benches/baseline.json` — CI baseline (committed).
- `benches/baseline.local.json` — Local machine baseline (gitignored).

### 11.3 Python Comparison Script

```bash
python scripts/bench_compare.py benches/baseline.json target/criterion/
```

---

## 12. Common Tasks

### 12.1 Adding a New Component

1. Define a new `ComponentKind` constant (range depends on namespace).
2. Implement serialization in the encoder crate.
3. Register in the component decode table.
4. Add ECS component struct in the world state crate.
5. Write unit test for encode/decode roundtrip.

### 12.2 Adding a New Transport

1. Create a new crate: `crates/aetheris-transport-{name}/`.
2. Implement the `GameTransport` trait from `aetheris-protocol`.
3. Add to workspace `Cargo.toml`.
4. Add an optional dependency in `aetheris-server/Cargo.toml`.
5. Add a feature flag and wire it into `main.rs`.

### 12.3 Adding a gRPC Service

1. Add `.proto` file in `crates/aetheris-protocol/proto/`.
2. Update `build.rs` to compile the new proto.
3. Implement the service in `aetheris-server`.
4. Register in the gRPC server builder in `main.rs`.

### 12.4 Release Process

```bash
just release 0.2.0
# 1. Bumps version in all Cargo.toml files
# 2. Generates CHANGELOG.md via git-cliff
# 3. Creates annotated git tag
```

---

## 13. Contribution Process

### 13.1 Governance

Aetheris uses the **OpenSpec** process. Before writing code for a new feature:

1. Draft a Technical Intent in `openspec/`.
2. Get review approval.
3. Implement code traceable to the spec.

### 13.2 Branch Naming

| Type | Pattern | Example |
|---|---|---|
| Feature | `task/ID-description` | `task/42-add-quinn-transport` |
| Bug fix | `fix/ID-description` | `fix/13-tick-overflow` |

### 13.3 Commit Messages

Conventional Commits format:

```
feat(transport): add MTU probing to Quinn transport
fix(encoder): handle zero-length payload in bitpack decoder
docs(design): flesh out ERROR_HANDLING_DESIGN.md
chore(deps): bump tokio to 1.52
```

### 13.4 PR Checklist

- [ ] `just check` passes
- [ ] Traceable to Active Spec ID
- [ ] Observability verified (metrics and traces work)
- [ ] Complex logic has "Professorial Comments"
- [ ] ROADMAP.md updated if milestone affected

---

## 14. Troubleshooting

### 14.1 WASM Build Fails

**Symptom**: `error[E0463]: can't find crate for 'std'`

**Fix**: Ensure the nightly toolchain and WASM target are installed:

```bash
rustup toolchain install nightly-2025-07-01
rustup target add wasm32-unknown-unknown --toolchain nightly-2025-07-01
```

### 14.2 WebTransport Connection Refused

**Symptom**: Browser client shows "connection error"

**Causes**:

1. Certificate expired (> 13 days old): `just clean-certs && just server`
2. Certificate hash mismatch: rebuild WASM client `just wasm-dev && just client-build`
3. Server not running or wrong port

### 14.3 `just check` Fails on `cargo deny`

**Symptom**: License or advisory violation

**Fix**: Check `deny.toml` for the allow-list. If a new dependency has a disallowed license, either find an alternative or add an exception with justification.

### 14.4 Observability Stack Not Receiving Data

**Symptom**: Grafana shows "No data"

**Causes**:

1. Server not started with `just server-obs` (needs `LOG_FORMAT=json` + OTLP endpoint)
2. Prometheus can't reach `host.docker.internal:9000` — check Docker networking
3. Jaeger not running: `just infra-up`

### 14.5 Port Already in Use

```bash
just stop       # Kill running server processes
# Or manually:
lsof -i :5000 -i :4433 -i :50051 -i :9000 | grep LISTEN
```

---

## 15. Open Questions

| Question | Context | Impact |
|---|---|---|
| **Onboarding Flow** | What is the ideal "first hour" experience for a new developer? | Developer adoption and speed. |
| **Dev Container** | Should we provide a VS Code Dev Container for guaranteed setup? | Eliminates "works on my machine" issues. |
| **Example Projects** | Should we ship example games built on Aetheris? | Learning by example for third-party developers. |
| **API Documentation** | Should `cargo doc` output be published to a website? | Discoverability for implementers. |

---

## Appendix A — Glossary

### Mini-Glossary (Quick Reference)

- **Local Setup**: The steps required to run Aetheris on a developer workstation.
- **Trait Facade**: The three core traits (GameTransport, WorldState, Encoder) that define the engine API.
- **Phase**: A named set of trait implementations (P1 = Bevy+Renet+Serde, P3 = Custom+Quinn+Bitpack).
- **just**: A command runner (like `make`) used for project automation.
- **nextest**: A faster Rust test runner that replaces `cargo test`.

[Full Glossary Document](../GLOSSARY.md)

---

## Appendix B — Decision Log

| # | Decision | Rationale | Revisit If... | Date |
|---|---|---|---|---|
| D1 | `just` as task runner (not Make) | Cross-platform, simpler syntax, no tab sensitivity. | Majority of contributors prefer Make. | 2026-04-15 |
| D2 | Nightly Rust for WASM only | Atomics + shared memory require nightly. Server uses stable. | Rust stabilizes WASM threading features. | 2026-04-15 |
| D3 | Conventional Commits | Enables automated changelog via `git-cliff`. | Team finds the convention too rigid. | 2026-04-15 |
| D4 | OpenSpec governance | Ensures design precedes implementation. | Process becomes a bottleneck for small changes. | 2026-04-15 |
| D5 | `cargo-nextest` over `cargo test` | Faster execution, better output, retry support. | nextest has compatibility issues with new Rust versions. | 2026-04-15 |

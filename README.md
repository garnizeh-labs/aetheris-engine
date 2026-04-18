# Aetheris Engine

Authoritative, tick-based, deterministic simulation engine — spatial hashing, interest management, and priority-based replication.

## The Authoritative Heart

**Aetheris Engine** is designed for browser-native multiplayer environments that demand sub-millisecond precision. By decoupling the simulation from the rendering and networking workers, Aetheris maintains a rock-solid 60Hz tick rate even under extreme network jitter. It enforces a zero-trust model where every state change is validated against authoritative rules before being replicated via optimized priority channels.

> **[Read the Engine Design Document](ENGINE_DESIGN.md)** — spatial partitioning, replication, and scaling.
>
> 🚀 **Latest Milestone:** **Architecture Extraction (M10145) in progress.** Decoupling the simulation core from the legacy monorepo.

[![CI](https://github.com/garnizeh-labs/aetheris-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/garnizeh-labs/aetheris-engine/actions/workflows/ci.yml)
[![Rust Version](https://img.shields.io/badge/rust-1.94%2B-blue.svg?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Quickstart

```bash
# 1. Run quality gate (fmt, clippy, tests, docs)
just check

# 2. Build local documentation
just docs
```

### 🛠️ Common Tasks

| Command | Category | Description |
| :--- | :--- | :--- |
| `just check` | **Quality** | Complete PR validation: Linters, tests, and documentation audit. |
| `just docs` | **Doc** | Generate technical design and API documentation. |

For a full list of commands, run `just --list`.

## Documentation Entry Points

- **[ENGINE_DESIGN.md](ENGINE_DESIGN.md):** Core simulation architecture.
- **[INTEREST_MANAGEMENT_DESIGN.md](INTEREST_MANAGEMENT_DESIGN.md):** Bandwidth optimization and visibility rules.
- **[SPATIAL_PARTITIONING_DESIGN.md](SPATIAL_PARTITIONING_DESIGN.md):** Native spatial hash implementation.

## Design Philosophy

1. **Deterministic Execution:** Simulation results are identical regardless of platform or transport.
2. **Bandwidth Efficiency:** Tick-based delta compression and interest management by default.
3. **Worker-Native:** Architected to run in isolated threads (WASM Web Workers) or native pods.

---
License: MIT / Apache-2.0

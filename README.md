# Aetheris Engine

The deterministic heart and authoritative simulation core of the Aetheris multiplayer platform.

## The Heart of the Simulation — Authoritative Determinism

In modern multiplayer architecture, the server is more than a message relay — it is the absolute source of truth. **Aetheris Engine** provides the sub-millisecond precision, high-frequency tick scheduling, and deterministic ECS bridging required to synchronize complex worlds across unreliable networks. It is designed to scale from rapid prototyping to production-grade, high-density environments using a decoupled, phase-based infrastructure.

This repository implements the authoritative tick scheduler that drives the Aetheris simulation. It bridges the gap between the wire protocol and the deep simulation state, ensuring every entity, component, and interaction is validated and replicated with cryptographic integrity.

> For more details, see the [Architecture Design Document](docs/ENGINE_DESIGN.md).
>
> 🚀 **Latest Milestone:** **Dependency Stabilization (M1019) complete!** Successfully migrated to Tonic 0.14, Axum 0.8, and OpenTelemetry 0.31. Fixed fragmentation regressions in the Phase 1 SerdeEncoder and aligned traits with Aetheris Protocol v0.2.1.

[![CI](https://github.com/garnizeh-labs/aetheris-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/garnizeh-labs/aetheris-engine/actions/workflows/ci.yml)
[![Rust Version](https://img.shields.io/badge/rust-1.95.0%2B-blue.svg?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![DeepMind: Advanced Agentic Coding](https://img.shields.io/badge/DeepMind-Agentic--Coding-purple.svg)](https://google.com)

## Workspace Components

The engine is built on modular, specialized crates for maximum reuse and testing isolation:

- **[`aetheris-server`](crates/aetheris-server)**: The authoritative heart. Handles tick scheduling, delta extraction, and multi-transport orchestration.
- **[`aetheris-ecs-bevy`](crates/aetheris-ecs-bevy)**: The primary simulation adapter. Bridges the Aetheris Protocol traits to the Bevy ECS ecosystem.
- **[`aetheris-transport-renet`](crates/aetheris-transport-renet)**: Phase 1 UDP transport using the `renet` protocol. Optimized for raw performance and low latency.
- **[`aetheris-transport-webtransport`](crates/aetheris-transport-webtransport)**: Phase 3 browser-native transport. Enables sub-millisecond latency for web-based clients.

## Quickstart

```bash
# 1. Run the quality gate (fmt, clippy, tests, security)
#    MUST PASS BEFORE OPENING ANY PR
just check

# 2. Automatically synchronize formatting and apply clippy fixes
just fix

# 3. Build documentation for the entire workspace
just docs
```

### 🛠️ Common Tasks

| Command | Category | Description |
| :--- | :--- | :--- |
| `just check` | **Quality** | Fast local validation: fmt, clippy, integration tests, and security audit. |
| `just check-all`| **CI** | Full validation: includes `udeps` and strict rustdoc checks. |
| `just fix` | **Lint** | Forces formatting and applies legal clippy suggestions. |
| `just test` | **Test** | Runs the full integration suite using `nextest`. |

For a full list of commands, run `just --list`.

## The Three Pillars

Aetheris Engine architecture resolves around three primary responsibilities:

1.  **Authoritative Scheduling**: A high-precision 60Hz loop that governs the six stages of a tick (Poll, Authorize, Simulate, Extract, Encode, Send).
2.  **Simulation Abstraction**: A trait-driven bridge that allows the engine to drive any ECS (Bevy, Custom, or Headless) without changing the network logic.
3.  **Infrastructure Bridge**: Integrated gRPC control plane and OIDC authentication that connects the real-time simulation to world services.

## Design Philosophy

1.  **Deterministic Mastery**: Every tick is a unit of absolute state, validated and hashed for cryptographic security.
2.  **Transport Agnosticism**: The engine drives the transport, never the other way around. Swap renet for quinn without touching game logic.
3.  **Observability First**: Deeply instrumented with OpenTelemetry and Prometheus to ensure production visibility.

---

License: MIT / Apache-2.0

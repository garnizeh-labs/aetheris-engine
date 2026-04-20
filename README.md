# Aetheris Engine

The deterministic heart and authoritative simulation core of the Aetheris multiplayer platform.

## The Heart of the Simulation — Authoritative Determinism

In modern multiplayer architecture, the server is more than a message relay — it is the absolute source of truth. **Aetheris Engine** provides the sub-millisecond precision, high-frequency tick scheduling, and deterministic ECS bridging required to synchronize complex worlds across unreliable networks.

This repository implements the authoritative tick scheduler that drives the Aetheris simulation. It bridges the gap between the wire protocol and the deep simulation state, ensuring every entity, component, and interaction is validated and replicated with cryptographic integrity.

> [!IMPORTANT]
> 🚀 **Current State:** **Milestone M1020** — Ship Classes & ECS Synchronization (Implemented).
> 
> Features introduced in this phase:
> - **Protocol Hardening:** Strict `InputCommand` validation and non-zero `ShipStats` initialization safety.
> - **2D Newtonian Flight:** Enforced Z-clamping (`z = 0.0`, `dz = 0.0`) within the Bevy simulation adapter.
> - **Server-Side Sovereignty:** `NetworkOwner` and `Visibility` logic moved strictly to the server to prevent snitching/cheating.

[![Build Status](https://github.com/garnizeh-labs/aetheris-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/garnizeh-labs/aetheris-engine/actions)
[![aetheris-server](https://img.shields.io/crates/v/aetheris-server?label=aetheris-server)](https://crates.io/crates/aetheris-server) [![docs](https://img.shields.io/docsrs/aetheris-server?label=docs)](https://docs.rs/aetheris-server)
[![aetheris-ecs-bevy](https://img.shields.io/crates/v/aetheris-ecs-bevy?label=aetheris-ecs-bevy)](https://crates.io/crates/aetheris-ecs-bevy) [![docs](https://img.shields.io/docsrs/aetheris-ecs-bevy?label=docs)](https://docs.rs/aetheris-ecs-bevy)
[![aetheris-ecs-custom](https://img.shields.io/crates/v/aetheris-ecs-custom?label=aetheris-ecs-custom)](https://crates.io/crates/aetheris-ecs-custom) [![docs](https://img.shields.io/docsrs/aetheris-ecs-custom?label=docs)](https://docs.rs/aetheris-ecs-custom)
[![aetheris-transport-quinn](https://img.shields.io/crates/v/aetheris-transport-quinn?label=aetheris-transport-quinn)](https://crates.io/crates/aetheris-transport-quinn) [![docs](https://img.shields.io/docsrs/aetheris-transport-quinn?label=docs)](https://docs.rs/aetheris-transport-quinn)
[![aetheris-transport-renet](https://img.shields.io/crates/v/aetheris-transport-renet?label=aetheris-transport-renet)](https://crates.io/crates/aetheris-transport-renet) [![docs](https://img.shields.io/docsrs/aetheris-transport-renet?label=docs)](https://docs.rs/aetheris-transport-renet)
[![aetheris-transport-webtransport](https://img.shields.io/crates/v/aetheris-transport-webtransport?label=aetheris-transport-webtransport)](https://crates.io/crates/aetheris-transport-webtransport) [![docs](https://img.shields.io/docsrs/aetheris-transport-webtransport?label=docs)](https://docs.rs/aetheris-transport-webtransport)
[![Rust Version](https://img.shields.io/badge/rust-1.95.0%2B-blue.svg?logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%2FApache--2.0-green.svg)](LICENSE-MIT)
[![DeepMind: Advanced Agentic Coding](https://img.shields.io/badge/DeepMind-Agentic--Coding-purple.svg)](https://google.com)

## Workspace Components

The engine is built on modular, specialized crates for maximum reuse and testing isolation:

- **[`aetheris-server`](crates/aetheris-server)**: The authoritative heartbeat. Handles tick scheduling (60Hz), delta extraction, and multi-transport orchestration.
- **[`aetheris-ecs-bevy`](crates/aetheris-ecs-bevy)**: The primary simulation adapter. Bridges Aetheris Protocol traits to the Bevy ECS ecosystem with zero-cost abstractions.
- **[`aetheris-transport-renet`](crates/aetheris-transport-renet)**: Phase 1 UDP transport using the `renet` protocol. Optimized for raw performance and low latency.
- **[`aetheris-transport-webtransport`](crates/aetheris-transport-webtransport)**: Phase 3 browser-native transport. Enables sub-millisecond latency for web-based clients.

## Quickstart

```bash
# 1. Run the quality gate (fmt, clippy, tests, security, docs)
just check

# 2. Automatically apply formatting and clippy fixes
just fix

# 3. List all specialized maintenance and run commands
just --list
```

### 🛠️ Common Tasks

| Command | Category | Description |
| :--- | :--- | :--- |
| `just check` | **Quality** | Fast local validation: fmt, clippy, integration tests, and security audit. |
| `just fix` | **Lint** | Forces formatting and applies legal clippy suggestions. |
| `just test` | **Test** | Runs the full integration suite using `nextest`. |
| `just server` | **Run** | Boots the game server in debug mode with auth bypass enabled. |

## The Three Pillars

1.  **Authoritative Scheduling**: A high-precision 60Hz loop governing the six stages of a tick: **Poll**, **Authorize**, **Simulate**, **Extract**, **Encode**, and **Send**.
2.  **Simulation Abstraction**: A trait-driven bridge allowing the engine to drive any ECS (Bevy or custom) without modifying networking logic.
3.  **Hardened Integrity**: Every input is validated, every state is replicated, and every vital is protected against division-by-zero or out-of-bounds corruption.

---

License: MIT / Apache-2.0

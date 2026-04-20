<div align="center">
  <h1>Aetheris Engine</h1>
  <p>The deterministic heart and authoritative simulation core of the Aetheris multiplayer platform.</p>

  [![CI](https://img.shields.io/github/actions/workflow/status/garnizeh-labs/aetheris-engine/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/garnizeh-labs/aetheris-engine/actions)
  [![Rust Version](https://img.shields.io/badge/rust-1.95.0%2B-blue?style=flat-square&logo=rust)](https://www.rust-lang.org/)
  [![Conventional Commits](https://img.shields.io/badge/Conventional%20Commits-1.0.0-yellow.svg?style=flat-square)](https://conventionalcommits.org)
</div>

---

## ⚙️ Authoritative Determinism

In modern multiplayer architecture, the server is more than a message relay — it is the absolute source of truth. **Aetheris Engine** provides the sub-millisecond precision, high-frequency tick scheduling, and deterministic ECS bridging required to synchronize complex worlds across unreliable networks.

> [!IMPORTANT]
> 🚀 **Current State:** **Milestone M1020** — Ship Classes & ECS Synchronization (Implemented).

### 📦 Workspace Components

| Crate | Link | Documentation |
| :--- | :--- | :--- |
| **`aetheris-server`** | [![Crates.io](https://img.shields.io/crates/v/aetheris-server?style=flat-square)](https://crates.io/crates/aetheris-server) | [![Docs.rs](https://img.shields.io/docsrs/aetheris-server?style=flat-square&logo=docs.rs&label=docs)](https://docs.rs/aetheris-server) |
| **`aetheris-ecs-bevy`** | [![Crates.io](https://img.shields.io/crates/v/aetheris-ecs-bevy?style=flat-square)](https://crates.io/crates/aetheris-ecs-bevy) | [![Docs.rs](https://img.shields.io/docsrs/aetheris-ecs-bevy?style=flat-square&logo=docs.rs&label=docs)](https://docs.rs/aetheris-ecs-bevy) |
| **`aetheris-ecs-custom`** | [![Crates.io](https://img.shields.io/crates/v/aetheris-ecs-custom?style=flat-square)](https://crates.io/crates/aetheris-ecs-custom) | [![Docs.rs](https://img.shields.io/docsrs/aetheris-ecs-custom?style=flat-square&logo=docs.rs&label=docs)](https://docs.rs/aetheris-ecs-custom) |
| **`aetheris-transport-quinn`** | [![Crates.io](https://img.shields.io/crates/v/aetheris-transport-quinn?style=flat-square)](https://crates.io/crates/aetheris-transport-quinn) | [![Docs.rs](https://img.shields.io/docsrs/aetheris-transport-quinn?style=flat-square&logo=docs.rs&label=docs)](https://docs.rs/aetheris-transport-quinn) |
| **`aetheris-transport-renet`** | [![Crates.io](https://img.shields.io/crates/v/aetheris-transport-renet?style=flat-square)](https://crates.io/crates/aetheris-transport-renet) | [![Docs.rs](https://img.shields.io/docsrs/aetheris-transport-renet?style=flat-square&logo=docs.rs&label=docs)](https://docs.rs/aetheris-transport-renet) |
| **`aetheris-transport-webtransport`** | [![Crates.io](https://img.shields.io/crates/v/aetheris-transport-webtransport?style=flat-square)](https://crates.io/crates/aetheris-transport-webtransport) | [![Docs.rs](https://img.shields.io/docsrs/aetheris-transport-webtransport?style=flat-square&logo=docs.rs&label=docs)](https://docs.rs/aetheris-transport-webtransport) |

---

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

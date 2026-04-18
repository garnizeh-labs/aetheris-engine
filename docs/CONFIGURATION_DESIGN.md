---Version: 0.2.0-draft
Status: Phase 1 — MVP / Phase 2 — Specified
Phase: P1 | P2
Last Updated: 2026-04-15
Authors: Team (Antigravity)
Spec References: [LC-0100, LC-0500]
Tier: 2
---

# Aetheris Configuration — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Configuration Architecture](#2-configuration-architecture)
3. [Environment Variables — Current Runtime Config](#3-environment-variables--current-runtime-config)
4. [Cargo Feature Flags — Compile-Time Config](#4-cargo-feature-flags--compile-time-config)
5. [Docker & Container Configuration](#5-docker--container-configuration)
6. [Observability Stack Configuration](#6-observability-stack-configuration)
7. [Logging & Tracing Configuration](#7-logging--tracing-configuration)
8. [TLS / Certificate Configuration](#8-tls--certificate-configuration)
9. [P2 — Configuration File Support (Planned)](#9-p2--configuration-file-support-planned)
10. [P2 — Hot Reload (Planned)](#10-p2--hot-reload-planned)
11. [Configuration Validation](#11-configuration-validation)
12. [Performance Contracts](#12-performance-contracts)
13. [Open Questions](#13-open-questions)
14. [Appendix A — Glossary](#appendix-a--glossary)
15. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

Aetheris uses a **two-layer configuration model**:

1. **Compile-time**: Cargo feature flags (`phase1` / `phase3`) select which implementations of the Trait Triad are compiled into the binary. These are mutually exclusive — a `compile_error!` macro prevents both from being active simultaneously.

2. **Runtime**: Environment variables configure operational parameters (ports, tick rate, log format, OTLP endpoint). `ServerConfig::load()` reads env vars with safe defaults, requiring zero configuration for local development.

This design is intentionally minimal for P1. No config file, no CLI flags, no hot-reload. These are planned for P2 once the core runtime is stable.

### Configuration Hierarchy (Current — P1)

```
Cargo Feature Flags (compile-time)
        ↓
Environment Variables (process start)
        ↓
Hardcoded Defaults (in ServerConfig::load())
```

---

## 2. Configuration Architecture

### 2.1 `ServerConfig` Struct

Defined in `aetheris-server/src/config.rs`:

```rust
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub metrics_port: u16,    // Prometheus scrape endpoint
    pub tick_rate: u64,       // Authoritative Hz (typical: 60)
}
```

### 2.2 Loading Strategy

```rust
impl ServerConfig {
    pub fn load() -> Self {
        let metrics_port = std::env::var("AETHERIS_METRICS_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(9000);

        let tick_rate = std::env::var("AETHERIS_TICK_RATE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);

        Self { metrics_port, tick_rate }
    }
}
```

The pattern is simple and intentional:

- **Parse failure = use default.** A malformed env var (e.g., `AETHERIS_TICK_RATE=abc`) is treated as "not set" and the default is used. This is a deliberate choice: a typo in a non-critical env var should not prevent the server from starting.
- **No panics.** `load()` is infallible — it always returns a valid config.
- **No external dependencies.** Pure `std::env` — no config crate, no TOML parser, no YAML.

---

## 3. Environment Variables — Current Runtime Config

### 3.1 Server Runtime Variables

| Variable | Type | Default | Description |
|---|---|---|---|
| `AETHERIS_METRICS_PORT` | `u16` | `9000` | Port for Prometheus metrics HTTP endpoint |
| `AETHERIS_TICK_RATE` | `u64` | `60` | Authoritative tick rate in Hz |

### 3.2 Observability Variables

| Variable | Type | Default | Description |
|---|---|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `String` | `http://localhost:4317` | OTLP gRPC endpoint for Jaeger spans |
| `RUST_LOG` | `String` | `info` | `tracing-subscriber` `EnvFilter` directive |
| `LOG_FORMAT` | `String` | `text` | `json` for structured JSON output; anything else for plain text |

### 3.3 TLS Variables

| Variable | Type | Default | Description |
|---|---|---|---|
| `AETHERIS_CERT_DIR` | `String` | `target/dev-certs` | Directory containing TLS certificates for WebTransport |

### 3.4 Hardcoded Constants (Not Yet Configurable)

| Constant | Value | Location | TODO |
|---|---|---|---|
| gRPC Auth address | `0.0.0.0:50051` | `main.rs` | Make configurable |
| Renet UDP address | `0.0.0.0:5000` | `main.rs` | Make configurable |
| WebTransport address | `0.0.0.0:4433` | `main.rs` | Make configurable |
| Prometheus scrape target | `host.docker.internal:9000` | `prometheus.yml` | TODO-OBS-001 |

---

## 4. Cargo Feature Flags — Compile-Time Config

### 4.1 Phase Selection

The `aetheris-server` crate defines two mutually exclusive feature groups:

```toml
[features]
default = ["phase1"]
phase1 = [
    "dep:aetheris-transport-renet",
    "dep:aetheris-transport-webtransport",
    "dep:aetheris-ecs-bevy",
    "dep:aetheris-encoder-serde",
]
phase3 = [
    "dep:aetheris-transport-quinn",
    "dep:aetheris-ecs-custom",
    "dep:aetheris-encoder-bitpack",
]
```

The binary enforces mutual exclusion at compile time:

```rust
#[cfg(all(feature = "phase1", feature = "phase3"))]
compile_error!("Features 'phase1' and 'phase3' are mutually exclusive.");
```

### 4.2 Protocol Features

The `aetheris-protocol` crate has its own feature flags:

| Feature | Purpose | Used By |
|---|---|---|
| `grpc` | Enables gRPC service implementations | `aetheris-server` |
| `test-utils` | Enables `MockTransport`, `MockWorldState`, `MockEncoder` | `aetheris-server` (dev) |

### 4.3 Build Commands

```bash
# Phase 1 (default)
cargo build -p aetheris-server

# Phase 3 (explicit)
cargo build -p aetheris-server --no-default-features --features phase3

# Docker build (always Phase 1 for now)
docker build -f docker/Dockerfile.server -t aetheris-server .
```

---

## 5. Docker & Container Configuration

### 5.1 Dockerfile Environment

The production Dockerfile (`docker/Dockerfile.server`) sets these defaults:

```dockerfile
ENV RUST_LOG=info
ENV LOG_FORMAT=json
ENV AETHERIS_CERT_DIR=/app/certs
ENV OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:4317
```

### 5.2 Exposed Ports

| Port | Protocol | Service |
|---|---|---|
| `9000/tcp` | HTTP | Prometheus metrics scrape endpoint |
| `4433/udp` | QUIC | WebTransport data plane |
| `50051/tcp` | gRPC | Control Plane auth service |

### 5.3 Docker Compose Override

Operators can override any env var in `docker-compose.yml`:

```yaml
services:
  aetheris-server:
    environment:
      AETHERIS_METRICS_PORT: "9500"
      AETHERIS_TICK_RATE: "30"
      RUST_LOG: "debug,hyper=warn"
```

---

## 6. Observability Stack Configuration

The observability stack is configured entirely via Docker Compose and its configuration files:

| Component | Config File | Key Settings |
|---|---|---|
| Prometheus | `docker/prometheus.yml` | 1s scrape interval, 30d retention, 5GB size limit |
| Grafana | `docker/grafana/provisioning/` | Auto-provisioned datasources, anonymous admin access (dev only) |
| Jaeger | Docker env vars | OTLP gRPC enabled, in-memory span storage |
| Loki | `docker/loki-config.yaml` | 14d retention |
| Promtail | `docker/promtail-config.yaml` | Docker log driver collection → Loki |

### 6.1 Resource Limits

| Service | CPU Limit | Memory Limit |
|---|---|---|
| Prometheus | 0.5 | 512 MB |
| Grafana | 0.5 | 512 MB |
| Jaeger | 1.0 | 2 GB |
| Loki | — | — |
| Promtail | — | — |

---

## 7. Logging & Tracing Configuration

### 7.1 Log Format Selection

The server selects JSON or plain text format at startup based on `LOG_FORMAT`:

```rust
let use_json = std::env::var("LOG_FORMAT")
    .map(|v| v == "json")
    .unwrap_or(false);
```

- **Local development**: Plain text (default) — human-readable.
- **Docker / Production**: `LOG_FORMAT=json` — machine-parseable for Loki/Promtail.

### 7.2 `RUST_LOG` Filter

Uses `tracing-subscriber`'s `EnvFilter` for per-module log levels:

```bash
# All info, but debug for the tick scheduler
RUST_LOG="info,aetheris_server::tick=debug"

# Quiet hyper/h2 noise during development
RUST_LOG="debug,hyper=warn,h2=warn"
```

### 7.3 OTLP Trace Export

Distributed trace spans are exported to Jaeger via OTLP gRPC. The endpoint is configurable via `OTEL_EXPORTER_OTLP_ENDPOINT`. When Jaeger is not reachable, the OTLP exporter fails silently (batch exporter drops spans) — it does not block the server.

---

## 8. TLS / Certificate Configuration

### 8.1 Development Certificates

WebTransport requires TLS. For local development, self-signed certificates are auto-generated:

```bash
# Generated by the build/startup process
target/dev-certs/
├── cert.pem
└── key.pem
```

### 8.2 Production Certificates

In production, mount real certificates into the container at `AETHERIS_CERT_DIR`:

```yaml
services:
  aetheris-server:
    volumes:
      - ./certs:/app/certs:ro
    environment:
      AETHERIS_CERT_DIR: /app/certs
```

---

## 9. P2 — Configuration File Support (Planned)

### 9.1 TOML Configuration

Phase 2 will introduce an optional TOML config file to reduce the number of environment variables:

```toml
# aetheris.toml (planned)
[server]
tick_rate = 60
metrics_port = 9000

[network]
renet_addr = "0.0.0.0:5000"
webtransport_addr = "0.0.0.0:4433"
grpc_addr = "0.0.0.0:50051"

[tls]
cert_dir = "/app/certs"

[observability]
otlp_endpoint = "http://jaeger:4317"
log_format = "json"
```

### 9.2 Precedence Order (Planned)

```
CLI flags (highest priority)
    ↓
Environment Variables
    ↓
Config File (aetheris.toml)
    ↓
Compiled Defaults (lowest)
```

Environment variables will always override the config file. CLI flags will override both.

---

## 10. P2 — Hot Reload (Planned)

### 10.1 Reloadable Parameters

Some parameters can safely change at runtime without restarting:

| Parameter | Hot-Reloadable? | Reason |
|---|---|---|
| `tick_rate` | No | Changes tick budget, requires pipeline restart |
| `metrics_port` | No | TCP listener is bound at startup |
| `RUST_LOG` | Yes | `tracing-subscriber` supports runtime filter change |
| Network addresses | No | Listeners are bound at startup |
| Rate limits (P2) | Yes | Read from `Arc<RwLock<Config>>` each tick |

### 10.2 Signal-Based Reload

On `SIGHUP`, the server will re-read the config file and update hot-reloadable parameters. Non-reloadable changes will be logged as warnings and ignored.

---

## 11. Configuration Validation

### 11.1 Current Validation

P1 performs minimal validation — parse failures fall back to defaults:

```rust
// Invalid string → None → default 9000
"abc".parse::<u16>().ok()  // None
```

### 11.2 P2 Planned Validation

| Check | Description |
|---|---|
| Port range | 1–65535 |
| Tick rate range | 1–240 Hz |
| Endpoint reachability | Warn if OTLP endpoint is unreachable at startup |
| Feature consistency | Warn if `phase3` features are enabled but persistence is not configured |
| Certificate existence | Error if `AETHERIS_CERT_DIR` does not contain valid files |

---

## 12. Performance Contracts

| Operation | Budget | Target |
|---|---|---|
| `ServerConfig::load()` | < 1 ms | Environment variable reads only, no file I/O |
| Config file parse (P2) | < 10 ms | Single TOML file, < 1 KB |
| Hot reload (P2) | < 1 ms | Atomic pointer swap, no allocations on hot path |

---

## 13. Open Questions

| Question | Context | Impact |
|---|---|---|
| **Hot Reloading** | Should the engine support hot-reloading configurations without a restart? | Operational flexibility vs complexity. Planned for P2 with `SIGHUP`. |
| **CLI Flags** | Should the server binary accept `--tick-rate` etc.? `clap` adds 200 KB to binary size. | Developer ergonomics vs. binary size. |
| **Secrets Management** | How should production JWT signing keys be injected? Env var? Vault? | Security posture for production deployment. |
| **Per-Transport Config** | Should each transport have its own config section? | Fine-grained tuning of Renet vs. WebTransport. |
| **Config Crate Extraction** | Should `ServerConfig` move to its own crate for reuse by tooling? | Monorepo ergonomics. |

---

## Appendix A — Glossary

### Mini-Glossary (Quick Reference)

- **Config Layer**: The hierarchy of configuration sources (Env, YAML, CLI).
- **Feature Flag**: Cargo compile-time conditional compilation (`#[cfg(feature = "...")]`).
- **Phase Selection**: Compile-time choice between P1 (Bevy+Renet+Serde) and P3 (Custom+Quinn+Bitpack).
- **Hot Reload**: Ability to update configuration at runtime without restarting the server.
- **OTLP**: OpenTelemetry Protocol — the gRPC-based protocol for exporting trace spans.

[Full Glossary Document](../GLOSSARY.md)

---

## Appendix B — Decision Log

| # | Decision | Rationale | Revisit If... | Date |
|---|---|---|---|---|
| D1 | Env vars only for P1 | Simplest possible config for MVP. No dependencies, no parsing failures. | Operators need more complex config. | 2026-04-15 |
| D2 | Parse failure = default | Prevents startup failures from typos. Logged as warning in P2. | Silent misconfiguration causes production incidents. | 2026-04-15 |
| D3 | No config crate dependency | Zero-dep config keeps compile times fast and WASM-compatible. | Config files are added in P2 (will use `toml` crate). | 2026-04-15 |
| D4 | Mutual exclusion via `compile_error!` | Prevents accidental activation of both Trait Triad implementations. | A hybrid mode is needed (e.g., migration testing). | 2026-04-15 |
| D5 | JSON logs in Docker, plain text locally | Structured JSON for machine parsing; plain text for developer readability. | Operators prefer a different format (e.g., logfmt). | 2026-04-15 |

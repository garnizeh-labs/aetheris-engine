# Aetheris Server

The authoritative heart and high-precision orchestration layer of the Aetheris Engine.

## The Heartbeat of the World — The Authoritative Scheduler

`aetheris-server` is the core executable crate that drives the Aetheris simulation. It implements the high-performance **Tick Scheduler** — a deterministic loop designed to govern the authoritative simulation state at 60Hz. It is responsible for pollinating transport events, authenticating sessions, driving the ECS simulation stages, and broadcasting delta-compressed world updates.

This crate serves as the primary infrastructure bridge, connecting the high-speed real-time simulation to world services via an integrated gRPC control plane.

## The Three Pillars of the Server

1.  **Deterministic Tick Scheduling**: Governs the six-stage simulation lifecycle (Poll, Auth, Simulate, Extract, Encode, Send) with sub-millisecond precision.
2.  **Infrastructure Orchestration**: Bridges multiple network transports (renet, quinn, wtransport) into a unified event pipeline.
3.  **Observability & Telemetry**: First-class support for OpenTelemetry tracing and Prometheus metrics, providing deep visibility into simulation performance.

## Architecture Highlights

- **Auth-Stage Integration**: Integrated OIDC and PASETO session management gating the simulation pollinator.
- **Dynamic Encoding**: Bridges the Aetheris Protocol traits to both rapid-iteration `rmp-serde` and Phase 3 bit-packed encoders.
- **Telemetry Bridge**: Native integration with the Aetheris Observability stack for real-time monitoring of tick density and packet overhead.

## Usage

This crate is typically run as the main entry point for the Aetheris Cluster.

```bash
# Run the server in development mode
cargo run -p aetheris-server

# Run with Prometheus metrics enabled
AETHERIS_PROMETHEUS_ADDR=0.0.0.0:9091 cargo run -p aetheris-server
```

For more details, see the [Architecture Design Document](../../docs/ENGINE_DESIGN.md).

---

License: MIT / Apache-2.0

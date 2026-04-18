---Version: 0.2.0-draft
Status: Phase 1 — MVP / Phase 2 — Specified
Phase: P1 | P2 | P4
Last Updated: 2026-04-15
Authors: Team (Antigravity)
Spec References: [LC-0100, LC-0600, INF-0200]
Tier: 2
---

# Aetheris Deployment — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Deployment Topology](#2-deployment-topology)
3. [Docker Images](#3-docker-images)
4. [Local Development Deployment](#4-local-development-deployment)
5. [Observability Stack Deployment](#5-observability-stack-deployment)
6. [Stress Test Deployment](#6-stress-test-deployment)
7. [Production Deployment (P2)](#7-production-deployment-p2)
8. [Container Port Map](#8-container-port-map)
9. [Resource Sizing](#9-resource-sizing)
10. [TLS & Certificate Management](#10-tls--certificate-management)
11. [Health Checks & Readiness](#11-health-checks--readiness)
12. [Scaling Strategy](#12-scaling-strategy)
13. [CI/CD Pipeline](#13-cicd-pipeline)
14. [Rollback & Recovery](#14-rollback--recovery)
15. [Open Questions](#15-open-questions)
16. [Appendix A — Glossary](#appendix-a--glossary)
17. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

Aetheris follows a **container-first deployment model**. Every deployable artifact is a Docker image built via a multi-stage Dockerfile (builder + minimal runtime). For P1, deployment targets are:

| Target | Method | Audience |
|---|---|---|
| Local dev | `just dev` (no Docker) | Developers |
| Local observability | `docker compose -f docker/docker-compose.yml` | Developers / QA |
| Stress testing | `docker compose -f docker/docker-compose.stress.yml` | Performance team |
| Production (P2+) | Kubernetes / managed container service | Operations |

The server binary is a single statically-linked executable (< 50 MB) that requires no runtime dependencies beyond `ca-certificates`. All configuration is via environment variables (see [CONFIGURATION_DESIGN.md](CONFIGURATION_DESIGN.md)).

---

## 2. Deployment Topology

### 2.1 Single-Node (P1)

```
┌─────────────────────────────────────────────────────┐
│                  Host Machine                       │
│                                                     │
│  ┌─────────────────────────────────────────────┐    │
│  │         aetheris-server                     │    │
│  │  ┌─────────┬──────────┬──────────────┐      │    │
│  │  │ Renet   │ WebTrans │ gRPC Auth    │      │    │
│  │  │ :5000   │ :4433    │ :50051       │      │    │
│  │  └─────────┴──────────┴──────────────┘      │    │
│  │  ┌────────────────────────────┐              │    │
│  │  │ Prometheus metrics :9000   │              │    │
│  │  └────────────────────────────┘              │    │
│  └─────────────────────────────────────────────┘    │
│                                                     │
│  ┌──────────────── Observability ───────────────┐   │
│  │ Prometheus :9090 │ Grafana :3000 │ Jaeger    │   │
│  │                  │               │ :16686    │   │
│  │ Loki :3100       │ Promtail      │           │   │
│  └──────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

### 2.2 Multi-Region (P4 — Federation)

Each region is an independent Aetheris deployment behind a Global Coordinator (CockroachDB). See [FEDERATION_DESIGN.md](FEDERATION_DESIGN.md).

---

## 3. Docker Images

### 3.1 Server Image (`Dockerfile.server`)

Multi-stage build:

| Stage | Base | Purpose |
|---|---|---|
| **builder** | `rust:1.85-slim-bookworm` | Full Rust toolchain + protobuf compiler |
| **runtime** | `debian:bookworm-slim` | Minimal runtime with `ca-certificates` |

```dockerfile
# Build
FROM rust:1.85-slim-bookworm AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev protobuf-compiler
COPY . .
RUN cargo build -p aetheris-server --release --features phase1

# Runtime
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates
COPY --from=builder /app/target/release/aetheris-server .
```

**Image size target:** < 50 MB compressed.

### 3.2 Stress Test Image (`Dockerfile.stress`)

Same multi-stage pattern. Builds the `aetheris-smoke-test` binary. Runtime includes `curl` for health checks. Entrypoint: `stress-entrypoint.sh` which waits for the server to be healthy before launching the stress client.

### 3.3 Build Commands

```bash
just docker-build                    # Build server image
just stress-docker-build             # Build stress image
docker build -f docker/Dockerfile.server -t aetheris-server .  # Manual
```

---

## 4. Local Development Deployment

### 4.1 Zero-Docker Quick Start

```bash
just dev
```

This single command:

1. Builds the WASM client with nightly Rust (atomics + shared memory).
2. Starts the server binary at `127.0.0.1:5000` (Renet) / `127.0.0.1:4433` (WebTransport) / `127.0.0.1:50051` (gRPC).
3. Launches Vite dev server at `http://localhost:5173`.

### 4.2 With Observability

```bash
just infra-up     # Docker Compose: Prometheus, Grafana, Jaeger, Loki, Promtail
just server-obs   # Server with LOG_FORMAT=json + OTLP endpoint
```

Access points:

- Grafana: `http://localhost:3000` (no login required in dev)
- Jaeger UI: `http://localhost:16686`
- Prometheus: `http://localhost:9090`

### 4.3 Teardown

```bash
just infra-down   # Stop observability stack
just stop         # Kill server processes
just clean        # Cargo clean
just reset-all    # Full reset: stop + clean + infra-down
```

---

## 5. Observability Stack Deployment

### 5.1 Stack Composition (`docker-compose.yml`)

| Service | Image | Ports | Resource Limits |
|---|---|---|---|
| Prometheus | `prom/prometheus:v2.52.0` | `9090` | 0.5 CPU / 512 MB |
| Grafana | `grafana/grafana-oss:10.4.2` | `3000` | 0.5 CPU / 512 MB |
| Jaeger | `jaegertracing/all-in-one:1.57` | `4317`, `16686` | 1.0 CPU / 2 GB |
| Loki | `grafana/loki:3.0.0` | `3100` | — |
| Promtail | `grafana/promtail:3.0.0` | — | — |

### 5.2 Data Persistence

| Service | Volume | Retention |
|---|---|---|
| Prometheus | `prometheus-data` | 30 days / 5 GB |
| Grafana | `grafana-data` | Indefinite |
| Loki | `loki-data` | 14 days |

### 5.3 Network

All observability services run on the `aetheris-obs` Docker bridge network. The server runs on the host and is accessed via `host.docker.internal`.

---

## 6. Stress Test Deployment

### 6.1 Configuration (`docker-compose.stress.yml`)

Full isolated environment with server + stress client + observability on the `aetheris-bench` network.

| Service | Resources | Purpose |
|---|---|---|
| `aetheris-server` | 1 CPU / 8 GB | Server under test |
| `aetheris-stress` | 4 CPUs / 8 GB | Simulated clients |

### 6.2 Running Stress Tests

```bash
# Quick stress test (25000 clients, 400s)
just stress-docker

# Custom parameters
just stress-docker 50000 600

# Full stress with observability dashboards
just observe-stress 25000 400
```

### 6.3 Stress Client Parameters

| Variable | Default | Description |
|---|---|---|
| `STRESS_COUNT` | `1000` (Docker default) / `25000` (just recipe) | Number of simulated clients |
| `STRESS_DURATION` | `60` (Docker default) / `400` (just recipe) | Test duration in seconds |

### 6.4 Shared Volumes

The `certs` volume is shared between the server and stress client containers to ensure TLS certificate trust for WebTransport.

---

## 7. Production Deployment (P2)

### 7.1 Container Orchestration

Phase 2 will target Kubernetes with the following resources per game server pod:

| Resource | Request | Limit |
|---|---|---|
| CPU | 1.0 | 2.0 |
| Memory | 2 GB | 4 GB |

### 7.2 Pod Architecture

```yaml
# Planned Kubernetes deployment (P2)
apiVersion: apps/v1
kind: Deployment
metadata:
  name: aetheris-server
spec:
  replicas: 1  # Each pod = 1 game instance
  template:
    spec:
      containers:
        - name: aetheris-server
          image: aetheris-server:latest
          ports:
            - containerPort: 4433
              protocol: UDP
            - containerPort: 50051
              protocol: TCP
            - containerPort: 9000
              protocol: TCP
          env:
            - name: RUST_LOG
              value: "info"
            - name: LOG_FORMAT
              value: "json"
          resources:
            requests:
              cpu: "1"
              memory: "2Gi"
            limits:
              cpu: "2"
              memory: "4Gi"
          readinessProbe:
            httpGet:
              path: /metrics
              port: 9000
            initialDelaySeconds: 5
            periodSeconds: 10
```

### 7.3 Service Mesh Considerations

Game traffic (QUIC/UDP) bypasses the service mesh — it requires direct client-to-pod connectivity. The gRPC control plane can use the mesh for mTLS and load balancing.

---

## 8. Container Port Map

| Port | Protocol | Service | Configurable? |
|---|---|---|---|
| `9000/tcp` | HTTP | Prometheus metrics | Yes (`AETHERIS_METRICS_PORT`) |
| `4433/udp` | QUIC/WebTransport | Data Plane | Not yet |
| `5000/udp` | Renet/UDP | Data Plane (P1 native) | Not yet |
| `50051/tcp` | gRPC | Control Plane | Not yet |
| `9090/tcp` | HTTP | Prometheus UI | Docker Compose only |
| `3000/tcp` | HTTP | Grafana UI | Docker Compose only |
| `16686/tcp` | HTTP | Jaeger UI | Docker Compose only |
| `4317/tcp` | gRPC | OTLP ingest (Jaeger) | Docker Compose only |

---

## 9. Resource Sizing

### 9.1 Server Resource Guidelines

| Client Count | CPU | Memory | Network Bandwidth |
|---|---|---|---|
| 100 | 0.5 cores | 512 MB | ~10 Mbps |
| 1,000 | 1.0 core | 1 GB | ~100 Mbps |
| 10,000 | 2.0 cores | 4 GB | ~1 Gbps |
| 25,000+ | 4.0 cores | 8 GB | ~2.5 Gbps |

### 9.2 Memory Profile

- ECS world state: ~100 bytes per entity (Position + Velocity + NetworkId bimap entry)
- Per-client buffer: ~2 KB (send buffer + receive buffer + session state)
- Baseline overhead: ~50 MB (Tokio runtime, tracing subscriber, metrics registry)

---

## 10. TLS & Certificate Management

### 10.1 Development

Self-signed certificates auto-generated to `target/dev-certs/`. Valid for 13 days. SHA-256 hash injected into the WASM client via `VITE_SERVER_CERT_HASH` at build time.

```bash
just clean-certs   # Force certificate regeneration
```

### 10.2 Production

Mount trusted certificates into the container:

```bash
docker run -v /path/to/certs:/app/certs:ro \
  -e AETHERIS_CERT_DIR=/app/certs \
  aetheris-server
```

Certificate rotation requires a container restart in P1. P2 will explore hot-rotation via inotify.

---

## 11. Health Checks & Readiness

### 11.1 Current (P1)

The Prometheus metrics endpoint (`/metrics` on port 9000) serves as a basic health check. If the endpoint responds with HTTP 200, the server is alive and the metrics exporter is functional.

### 11.2 Planned (P2)

| Endpoint | Port | Response | Purpose |
|---|---|---|---|
| `/health` | 9000 | `200 OK` or `503` | Kubernetes liveness probe |
| `/ready` | 9000 | `200 OK` or `503` | Kubernetes readiness probe |
| `/metrics` | 9000 | Prometheus text format | Metrics scraping |

Readiness will check: tick loop is running, at least one transport is listening, metrics exporter is functional.

---

## 12. Scaling Strategy

### 12.1 Vertical (P1–P2)

Each game server instance is single-threaded (tick pipeline) with async I/O (Tokio). Scaling vertically means giving more CPU to reduce tick jitter and more memory for larger worlds.

### 12.2 Horizontal (P4 — Federation)

Multiple independent server instances, each managing a shard (region/zone). A Global Coordinator handles cross-shard entity hand-off. See [FEDERATION_DESIGN.md](FEDERATION_DESIGN.md).

### 12.3 Auto-Scaling Signals

| Signal | Source | Action |
|---|---|---|
| `aetheris_connected_clients` | Prometheus | Scale up when clients > 80% capacity |
| `aetheris_tick_duration_seconds` | Prometheus | Scale up when p99 > 14ms (84% budget) |
| Client queue depth | Matchmaker | Spin up new instance when queue > threshold |

---


> **Pro:** This section is continued in the private companion document available to Nexus Plus customers.

## 13. CI/CD Pipeline

### 13.1 PR Gate

```bash
just check
# Runs: fmt → clippy → deny → audit → nextest → doc_lint → check_links
```

### 13.2 Release Pipeline

```bash
just release <version>
# 1. Bumps version in all Cargo.toml files
# 2. Generates CHANGELOG.md via git-cliff
# 3. Creates git tag
# 4. (Future) Triggers Docker image build + push
```

### 13.3 Image Tagging Strategy (Planned)

| Tag | Content |
|---|---|
| `v0.1.14` | Immutable release |
| `latest` | Most recent release |
| `sha-abc1234` | Specific commit (for staging) |

---

## 14. Rollback & Recovery

### 14.1 Docker Compose (Dev)

```bash
just infra-reset   # Stop, remove volumes, restart
just reset-all     # Full environment reset
```

### 14.2 Production (P2+)

- **Blue-Green**: Deploy new version alongside old. Route matchmaker to new. Drain old.
- **Canary**: Route 5% of new connections to new version. Monitor error rates. Promote or rollback.
- **Instant Rollback**: Point container tag back to previous image. Restart pods.

Game state is not persisted across server restarts in P1. Clients reconnect and receive fresh state. P2+ persistence layer enables state restoration from snapshots.

---

## 15. Open Questions

| Question | Context | Impact |
|---|---|---|
| **K8s Operator** | Do we need a custom Kubernetes Operator for managing clusters? | Operational automation vs. complexity. |
| **UDP Load Balancing** | How do we load-balance QUIC/UDP traffic in Kubernetes? | Requires IPVS or dedicated UDP LB. |
| **Zero-Downtime Deploys** | How do we drain active game sessions during deployment? | Player experience during updates. |
| **Multi-Arch Images** | Should we build `linux/arm64` images for ARM-based cloud instances? | Cost savings on Graviton/Ampere. |
| **Image Registry** | GHCR, Docker Hub, or self-hosted? | Supply chain security and pull limits. |

---

## Appendix A — Glossary

### Mini-Glossary (Quick Reference)

- **Cluster Deployment**: The process of bringing up a complete regional instance of Aetheris.
- **Multi-Stage Build**: Docker build pattern separating the build toolchain from the runtime image.
- **Blue-Green Deploy**: Running two identical production environments and switching traffic between them.
- **Readiness Probe**: Kubernetes health check that determines if a pod should receive traffic.
- **OTLP**: OpenTelemetry Protocol for trace/metric export.

[Full Glossary Document](../GLOSSARY.md)

---

## Appendix B — Decision Log

| # | Decision | Rationale | Revisit If... | Date |
|---|---|---|---|---|
| D1 | Multi-stage Docker builds | Minimal image size, no build tools in runtime. | Buildkit caching becomes a bottleneck. | 2026-04-15 |
| D2 | `debian:bookworm-slim` runtime base | Broader compatibility than Alpine (musl issues with OpenSSL). | Image size becomes critical; switch to `scratch` or `distroless`. | 2026-04-15 |
| D3 | No Kubernetes in P1 | Over-engineering for MVP. Docker Compose is sufficient for dev/test. | Production deployment is attempted. | 2026-04-15 |
| D4 | Prometheus metrics as health check | Reuses existing endpoint; no additional code. | Dedicated health check with deeper liveness semantics is needed. | 2026-04-15 |
| D5 | Single-process server (no sidecar) | Simplest deployment unit. Observability via in-process SDKs. | Log shipping or metric collection requires a sidecar. | 2026-04-15 |

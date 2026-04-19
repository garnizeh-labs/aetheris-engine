<!--
  Live document — update this file whenever:
    • a new crate is added to the workspace
    • a new trait impl is introduced or removed
    • the tick pipeline stages change
    • a new transport backend is added
    • the ECS adapter or component registry API changes
    • a new gRPC service is implemented
    • environment variable names change
  Version history is tracked via git. Bump the Version field on every edit.
-->

---
Version: 1.0.1
Last Updated: 2026-04-19
Rust Edition: 2024
MSRV: 1.95.0
Workspace Version: 0.3.0
Phase: 1 (in progress)
Protocol Dependency: aetheris-protocol 0.2.5
---

# Copilot Instructions — aetheris-engine

This is the **authoritative simulation core** of the Aetheris platform. It owns the
five-stage tick loop, all server-side trait implementations (ECS, transport), and
the gRPC control-plane services.

---

## Repository Layout

```
crates/
  aetheris-server/              # Tick scheduler, multi-transport, auth, matchmaking
  aetheris-ecs-bevy/            # Phase 1: WorldState impl via bevy_ecs 0.18
  aetheris-ecs-custom/          # Phase 3: custom SoA ECS (stub)
  aetheris-transport-renet/     # Phase 1: GameTransport impl via renet (UDP)
  aetheris-transport-quinn/     # Phase 3: GameTransport impl via quinn (native QUIC, stub)
  aetheris-transport-webtransport/ # GameTransport impl via wtransport (browser clients)
docs/
  ENGINE_DESIGN.md              # Five-stage pipeline, priority channels, AoI
  ECS_DESIGN.md                 # Component schema, dirty-bit tracking, replication
  CONFIGURATION_DESIGN.md       # ServerConfig, env vars, TOML support (P4)
  SECURITY_DESIGN.md            # Four-layer model: TLS, invariants, Merkle, replay
  OBSERVABILITY_DESIGN.md       # tracing + metrics + OpenTelemetry OTLP
  PERSISTENCE_DESIGN.md         # Three-tier storage (Warm/Cold/Ice), off-tick flush
  INTEREST_MANAGEMENT_DESIGN.md # Spatial hash grid AoI, 4-filter pipeline
  PRIORITY_CHANNELS_DESIGN.md   # P0–P5 channel definitions, shedding policy
  DEPLOYMENT_DESIGN.md          # Docker Compose, K8s, Prometheus + Grafana + Loki
  FEDERATION_DESIGN.md          # P3+ cross-region sharding via jump gates
  SPATIAL_PARTITIONING_DESIGN.md # Spatial hash grid, O(1) insert, O(K) query
```

---

## The Five-Stage Tick Pipeline

Every tick runs exactly these five stages in order. The entire pipeline must
complete within **16.6 ms** (60 Hz). No blocking I/O is permitted inside any stage.

```
Stage 1 — POLL    (~1.0 ms): transport.poll_events()        → Vec<NetworkEvent>
Stage 2 — APPLY   (~2.0 ms): world.apply_updates(inputs)    → inject client inputs
Stage 3 — SIMULATE (~8.0 ms): world.simulate()              → physics, AI, rules
Stage 4 — EXTRACT (~2.5 ms): world.extract_deltas()         → Vec<ReplicationEvent>
Stage 5 — SEND    (~2.0 ms): encoder.encode() + send_*()    → dispatch to clients
```

### `TickScheduler` — Usage

```rust
// crates/aetheris-server/src/tick.rs
use aetheris_server::tick::TickScheduler;
use aetheris_server::auth::AuthServiceImpl;
use tokio_util::sync::CancellationToken;

let auth = AuthServiceImpl::new(/* ... */);
let mut scheduler = TickScheduler::new(60, auth); // 60 Hz

let shutdown = CancellationToken::new();
scheduler.run(
    Box::new(transport),   // impl GameTransport
    Box::new(world),       // impl WorldState
    Box::new(encoder),     // impl Encoder
    shutdown.clone(),
).await;
```

The scheduler pre-allocates the encode buffer once via `encoder.max_encoded_size()`
and reuses it across all ticks. Zero heap allocation on the hot path.

### `ServerConfig` — Environment Variables

```rust
// crates/aetheris-server/src/config.rs
let config = ServerConfig::load();
// AETHERIS_TICK_RATE   → config.tick_rate    (default: 60)
// AETHERIS_METRICS_PORT → config.metrics_port (default: 9000)
```

---

## Phase 1 ECS Adapter — `BevyWorldAdapter`

Implements `WorldState` using `bevy_ecs 0.18`. Maintains a bidirectional map
between `NetworkId` and Bevy's `Entity` handle via `bimap`.

```rust
// crates/aetheris-ecs-bevy/src/adapter.rs
use aetheris_ecs_bevy::BevyWorldAdapter;
use aetheris_ecs_bevy::registry::BoxedReplicator;

let mut adapter = BevyWorldAdapter::new(World::new());

// Register a component replicator (required for each replicated component type)
adapter.register_replicator(my_transform_replicator);

// The adapter is now ready to be passed to TickScheduler as Box<dyn WorldState>
let world: Box<dyn WorldState> = Box::new(adapter);
```

### Key Bevy Components (added to networked entities)

```rust
/// Marks an entity as network-replicated. Holds its global NetworkId.
#[derive(Component)]
pub struct Networked(pub NetworkId);

/// Marks the authoritative owner of an entity. Used to reject spoofed updates.
#[derive(Component)]
pub struct Ownership(pub ClientId);
```

### Delta Extraction (how `extract_deltas` works)

```rust
// Bevy's change detection detects modified components since last tick.
// BevyWorldAdapter queries each replicator for changed components per entity.
// Only entities with changed components produce ReplicationEvent items.
fn extract_deltas(&mut self) -> Vec<ReplicationEvent> {
    let mut deltas = Vec::new();
    let current_tick = self.world.change_tick();

    for (&network_id, &entity) in &self.bimap {
        for replicator in self.replicators.values() {
            if let Some(event) = replicator.extract(
                &self.world, entity, network_id, tick, self.last_extraction_tick,
            ) {
                deltas.push(event);
            }
        }
    }

    self.last_extraction_tick = Some(current_tick);
    deltas
}
```

---

## Phase 1 Transport — `RenetTransport`

Implements `GameTransport` using `renet` (UDP with reliability channels).

```rust
// crates/aetheris-transport-renet/src/lib.rs
use aetheris_transport_renet::{RenetTransport, RenetServerConfig};

let config = RenetServerConfig {
    protocol_id: 0xAE7H,
    max_clients: 1000,
    authentication: renet_netcode::ServerAuthentication::Unsecure,
    max_new_connections_per_second: 5,  // token-bucket DoS protection
    max_payload_size: aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE, // 1200
    ..Default::default()
};

let transport = RenetTransport::new("0.0.0.0:4433".parse()?, config).await?;
let transport: Box<dyn GameTransport> = Box::new(transport);
```

**IpRateLimiter** is built-in: 5 new connections/second per source IP by default.
Exceeding the limit causes `ClientNotConnected` silently (DoS mitigation).

---

## Browser Transport — `WebTransportBridge`

Implements `GameTransport` for browser clients via `wtransport` (QUIC/HTTP3).
Generates a self-signed certificate in memory on startup; logs its SHA-256 hash
for use in the browser's `serverCertificateHashes` WebTransport option.

```rust
// crates/aetheris-transport-webtransport/src/lib.rs
use aetheris_transport_webtransport::WebTransportBridge;

// Binds to the given address and begins accepting WebTransport sessions
let bridge = WebTransportBridge::new("0.0.0.0:4433".parse()?).await;

// cert_hash is logged at startup — copy it for the browser client config
println!("Certificate hash: {}", bridge.cert_hash());

let transport: Box<dyn GameTransport> = Box::new(bridge);
```

**Security**: The cert hash is logged so browser clients can pin it via
`serverCertificateHashes`. Never use `--ignore-certificate-errors` in production.

---

## Multi-Transport Aggregation

`MultiTransport` wraps multiple `GameTransport` backends under a single interface,
allowing native UDP (renet) and WebTransport to coexist on the same server:

```rust
// crates/aetheris-server/src/multi_transport.rs
use aetheris_server::multi_transport::MultiTransport;

let mut multi = MultiTransport::new();
multi.add(Box::new(renet_transport));       // native clients
multi.add(Box::new(webtransport_bridge));   // browser clients

let transport: Box<dyn GameTransport> = Box::new(multi);
```

---

## Authentication — PASETO v4 Tokens

Session tokens are issued by the `AuthService` (gRPC, Control Plane).
They are validated once at connection time; no per-tick auth overhead.

```rust
// crates/aetheris-server/src/auth/
// Auth flow:
// 1. Client calls AuthService::Authenticate (gRPC) → receives session_token
// 2. Client sends NetworkEvent::Auth { session_token } on first connect
// 3. TickScheduler validates token → spawns entity → acknowledges with NetworkEvent::Spawn

// The TickScheduler's authenticated_clients map tracks:
//   ClientId → (session JTI, spawned NetworkId)
```

---

## Observability

Every tick records its duration as a Prometheus histogram:

```rust
metrics::histogram!("aetheris_tick_duration_seconds").record(elapsed.as_secs_f64());
```

Standard instrumentation patterns:

```rust
use tracing::{info_span, Instrument};

// Wrap async ops with spans for distributed tracing
async_op()
    .instrument(info_span!("tick", tick = self.current_tick))
    .await;

// Record entity counts after extraction
metrics::counter!("aetheris_ecs_extraction_count").increment(deltas.len() as u64);
metrics::gauge!("aetheris_ecs_entities_networked").set(self.bimap.len() as f64);
```

Metrics are exported via Prometheus on `AETHERIS_METRICS_PORT` (default `9000`).
OpenTelemetry OTLP spans are wired in `crates/aetheris-server/src/telemetry/`.

---

## Phase Evolution

| Subsystem | Phase 1 (current) | Phase 3 (target) |
|---|---|---|
| ECS | `BevyWorldAdapter` | Custom SoA ECS (`aetheris-ecs-custom`) |
| Transport (native) | `RenetTransport` | `QuinnTransport` |
| Transport (browser) | `WebTransportBridge` | `WebTransportBridge` (unchanged) |
| Encoder | `SerdeEncoder` | `BitpackEncoder` |

**Rule**: Phase 3 replacements are data-driven. No substitution occurs before
Phase 2 stress-test data confirms the component is a bottleneck.

---

## Key Conventions

- **No blocking I/O inside the tick budget.** All DB writes, auth checks, and
  external service calls must be decoupled via `tokio::mpsc` channels.
- **All errors use `thiserror`**. Do not use `anyhow` in library crates.
- **`Networked(NetworkId)` and `Ownership(ClientId)`** must be attached to every
  replicated Bevy entity. Missing `Ownership` means the entity rejects all client updates.
- **`ComponentKind(1)`** is reserved for `Transform`. Define new component kinds
  starting from `ComponentKind(2)` and register a corresponding `BoxedReplicator`.
- **Jemalloc is the allocator** in the server binary (`tikv-jemallocator`).
  Do not change the global allocator without profiling justification.
- Transport crates are `#[cfg(not(target_arch = "wasm32"))]` — they are server-only.

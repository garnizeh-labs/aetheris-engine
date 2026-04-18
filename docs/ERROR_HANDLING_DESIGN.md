---
Version: 0.2.0-draft
Status: Phase 1 — MVP / Phase 2 — Specified
Phase: P1 | P2
Last Updated: 2026-04-15
Authors: Team (Antigravity)
Spec References: [LC-0100, LC-0400]
Tier: 2
---

# Aetheris Error Handling — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Error Architecture — Domain Separation](#2-error-architecture--domain-separation)
3. [Core Error Types](#3-core-error-types)
4. [Error Propagation Model](#4-error-propagation-model)
5. [Error Handling in the Tick Pipeline](#5-error-handling-in-the-tick-pipeline)
6. [Transport Errors](#6-transport-errors)
7. [Encoder Errors — Malformed Payload Defence](#7-encoder-errors--malformed-payload-defence)
8. [ECS Errors](#8-ecs-errors)
9. [Control Plane Errors — gRPC Status Codes](#9-control-plane-errors--grpc-status-codes)
10. [Client-Side Error Handling](#10-client-side-error-handling)
11. [Observability Integration](#11-observability-integration)
12. [Performance Contracts](#12-performance-contracts)
13. [Open Questions](#13-open-questions)
14. [Appendix A — Glossary](#appendix-a--glossary)
15. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

Error handling in Aetheris follows two non-negotiable principles:

1. **The tick pipeline must never panic.** An unhandled error inside the 16.6 ms tick loop means the entire simulation crashes, disconnecting all clients. Every error on the hot path is caught, logged, and continued past.

2. **Errors are domain-typed, not stringly-typed.** Each subsystem defines its own error enum with `thiserror`. Errors carry structured context (the failing `NetworkId`, the buffer sizes, the offending byte offset) that allows automated observability without manual string parsing.

The error architecture is organized by **domain**: Transport, Encoder, ECS (World), Control Plane, and Persistence. Each domain's error type is defined in `aetheris-protocol` and is the only error type that crosses trait boundaries. Implementation crates convert internal errors into the protocol error types at their public API surface.

### Error Domain Map

| Domain | Error Type | Defined In | Hot Path? |
|---|---|---|---|
| Transport | `TransportError` | `aetheris-protocol/src/error.rs` | Yes (Stage 1, 5) |
| Encoder | `EncodeError` | `aetheris-protocol/src/error.rs` | Yes (Stage 2, 5) |
| ECS | `WorldError` | `aetheris-protocol/src/error.rs` | Yes (Stage 2, 3, 4) |
| Control Plane | `tonic::Status` | `tonic` crate | No (gRPC, off-tick) |
| Persistence | `sqlx::Error` (wrapped) | `aetheris-server` | No (async, off-tick) |

---

## 2. Error Architecture — Domain Separation

Each trait in the Trait Facade returns its own error type. This prevents error type leaks across subsystem boundaries:

```text
GameTransport::send_unreliable() → Result<(), TransportError>
Encoder::encode()                → Result<usize, EncodeError>
Encoder::decode()                → Result<ComponentUpdate, EncodeError>
WorldState::despawn_networked()  → Result<(), WorldError>
```

**No `Box<dyn Error>` on the hot path.** Dynamic dispatch on the error path introduces vtable indirection and prevents compile-time exhaustiveness checking. All hot-path error types are concrete enums.

**`thiserror` for ergonomics.** Every error type derives `thiserror::Error` for automatic `Display` and `From` implementations without runtime overhead.

---

## 3. Core Error Types

The canonical error definitions live in `aetheris-protocol/src/error.rs`:

### 3.1 `TransportError`

 ```textrust
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("Client {0:?} is not connected")]
    ClientNotConnected(ClientId),

    #[error("Datagram exceeds MTU limit ({size} > {max})")]
    PayloadTooLarge { size: usize, max: usize },

    #[error("Transport I/O error: {0}")]
    Io(#[from] std::io::Error),
}
 ```text

**`ClientNotConnected`**: Returned when `send_unreliable` or `send_reliable` targets a client that has disconnected between the start and end of the tick. This is a normal race condition in a concurrent system — the tick pipeline logs it and continues.

**`PayloadTooLarge`**: Returned when the encoded payload exceeds `MAX_SAFE_PAYLOAD_SIZE` (1,200 bytes). This is a defensive check against encoder bugs that produce oversized packets. The payload is dropped, the error is logged, and the tick continues.

**`Io`**: Wraps `std::io::Error` for low-level socket failures. In production, this typically indicates a system resource exhaustion (file descriptor limit, memory pressure) or a terminated connection.

### 3.2 `EncodeError`

 ```textrust
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("Buffer overflow: need {needed} bytes, have {available}")]
    BufferOverflow { needed: usize, available: usize },

    #[error("Malformed payload at byte offset {offset}")]
    MalformedPayload { offset: usize },

    #[error("Unknown component kind: {0:?}")]
    UnknownComponent(ComponentKind),

    #[error("Encoder I/O error: {0}")]
    Io(#[from] std::io::Error),
}
 ```text

**`BufferOverflow`**: The caller-supplied encode buffer is too small. This should never happen in production (the buffer is pre-allocated to `max_encoded_size()`). If it does, it indicates a bug in the encoder's size estimation.

**`MalformedPayload`**: The decoder encountered invalid bytes at the specified offset. This is the primary defence against adversarial clients sending crafted packets. The packet is dropped, the client's `SuspicionScore` may increase, and the tick continues.

**`UnknownComponent`**: A `ComponentKind` value that is not registered in the encoder's component table. This may indicate a client running a different protocol version or an injection attack.

### 3.3 `WorldError`

 ```textrust
#[derive(Debug, thiserror::Error)]
pub enum WorldError {
    #[error("Entity {0:?} not found")]
    EntityNotFound(NetworkId),

    #[error("Entity {0:?} already exists")]
    EntityAlreadyExists(NetworkId),
}
 ```text

**`EntityNotFound`**: An `apply_updates()` call references a `NetworkId` that does not exist in the local ECS. This is common during entity despawn/respawn races and is handled gracefully (the update is skipped).

**`EntityAlreadyExists`**: A `spawn_networked()` call attempts to create an entity with a `NetworkId` that is already in the bimap. This violates invariant B4 (no recycling) and indicates a bug in the allocator or a replayed spawn event.

---

## 4. Error Propagation Model

### 4.1 Hot Path: Log-and-Continue

Inside the tick pipeline, errors are **never propagated upward**. The tick must complete. The strategy:

 ```textrust
// Stage 5: Encode and send
for delta in &deltas {
    match encoder.encode(delta, &mut buffer) {
        Ok(len) => {
            if let Err(e) = transport.broadcast_unreliable(&buffer[..len]) {
                tracing::warn!(error = %e, "broadcast failed");
                metrics::counter!("aetheris_transport_errors_total", "kind" => "broadcast").increment(1);
                // Continue to next delta — do NOT abort the tick
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                network_id = %delta.network_id,
                component = ?delta.component_kind,
                "encode failed, skipping delta"
            );
            metrics::counter!("aetheris_encoder_errors_total", "kind" => e.variant()).increment(1);
            // Continue — one failed encode does not stop the tick
        }
    }
}
 ```text

### 4.2 Cold Path: Propagate via Result

Outside the tick pipeline (gRPC handlers, persistence sink, audit worker), errors propagate normally via `Result<T, E>` and `?` operator. These paths can afford to retry, log, or return an error to the caller.

### 4.3 Fatal vs. Recoverable

| Error | Impact | Recovery |
|---|---|---|
| Single `encode()` failure | 1 entity misses 1 tick of replication | Skip and continue |
| Single `send_unreliable()` failure | 1 packet lost for 1 client | Next tick sends fresher data |
| `poll_events()` I/O error | No inbound events this tick | Log, retry next tick |
| `simulate()` panic | **Fatal** — entire ECS state may be corrupt | Crash server, clients reconnect to backup |
| DB write failure (persistence sink) | Events queue in mpsc channel | Auto-retry with backoff |
| gRPC auth failure | Client cannot log in | Return `UNAUTHENTICATED` status |

---

## 5. Error Handling in the Tick Pipeline

The five-stage pipeline has specific error handling per stage:

### Stage 1 — Poll

 ```textrust
let events = transport.poll_events();
// poll_events() returns Vec<NetworkEvent>, never Err.
// Transport errors are reported as NetworkEvent::ClientDisconnected.
 ```text

### Stage 2 — Apply

 ```textrust
for event in events {
    match event {
        NetworkEvent::UnreliableMessage { data, client_id, .. } => {
            match encoder.decode(&data) {
                Ok(update) => { /* apply to worldstate */ }
                Err(EncodeError::MalformedPayload { offset }) => {
                    tracing::warn!(%client_id, offset, "malformed packet from client");
                    // Potential security event — see SECURITY_DESIGN.md §5
                }
                Err(e) => {
                    tracing::debug!(%client_id, error = %e, "decode failed");
                }
            }
        }
        // ... handle other event types
    }
}
 ```text

### Stage 3 — Simulate

The ECS simulation must not panic. All systems must handle edge cases (empty components, missing entities) gracefully. If a system panics, `catch_unwind` is **not used** — a panic in the simulation indicates a logic bug that may have corrupted ECS state. The server process crashes and clients reconnect to a healthy node.

### Stage 4 — Extract

 ```textrust
let deltas = world.extract_deltas();
// extract_deltas() returns Vec<ReplicationEvent>, never Err.
// Internal errors result in fewer deltas, not failures.
 ```text

### Stage 5 — Encode & Send

See §4.1 for the log-and-continue pattern.

---

## 6. Transport Errors

### 6.1 Client Disconnection During Tick

A client may disconnect between Stage 1 (poll) and Stage 5 (send). When `send_unreliable` returns `TransportError::ClientNotConnected`, the tick pipeline:

1. Logs the error at `DEBUG` level (disconnections are normal).
2. Skips the client for the remainder of this tick.
3. The next `poll_events()` call will produce a `NetworkEvent::ClientDisconnected`.
4. The server despawns the client's entities.

### 6.2 MTU Violation

If the encoder produces a payload exceeding `MAX_SAFE_PAYLOAD_SIZE`:

1. `send_unreliable` returns `TransportError::PayloadTooLarge`.
2. The event is logged with the entity's `NetworkId` and the payload size.
3. The metric `aetheris_transport_errors_total{kind="payload_too_large"}` increments.
4. The delta is dropped for this tick. The next tick will re-extract and attempt to send.

---

## 7. Encoder Errors — Malformed Payload Defence

### 7.1 Adversarial Input

The `decode()` method receives raw bytes from the network — bytes that may have been crafted by a modified client. The encoder must **never panic** on any input:

 ```textrust
// Security contract: decode MUST satisfy for ALL byte sequences b:
//   encoder.decode(b) returns Ok(_) or Err(EncodeError)
//   It NEVER panics, NEVER reads out of bounds, NEVER produces UB.
 ```text

### 7.2 Bounds Checking

Every decode path performs explicit bounds checking:

 ```textrust
fn decode(&self, buf: &[u8]) -> Result<ComponentUpdate, EncodeError> {
    if buf.len() < HEADER_SIZE {
        return Err(EncodeError::MalformedPayload { offset: 0 });
    }
    // ... parse header using safe byte-slice operations
    // ... validate ComponentKind is registered
    // ... validate remaining payload length matches expected
}
 ```text

### 7.3 Unknown Component Rejection

A packet with an unregistered `ComponentKind` is rejected immediately. It may indicate:

- A client running a newer protocol version (forward-compatibility scenario).
- An injection attack with random component IDs.

Either way, the packet is dropped and counted.

---

## 8. ECS Errors

### 8.1 Entity Not Found

During `apply_updates()`, if a `NetworkId` is not found in the bimap:

 ```textrust
match world.get_local_id(update.network_id) {
    Some(local_id) => { /* apply update to entity */ }
    None => {
        tracing::debug!(
            network_id = %update.network_id,
            "entity not found, skipping update (likely despawned)"
        );
        // Not an error — normal during despawn/respawn transitions
    }
}
 ```text

### 8.2 Duplicate Spawn Prevention

 ```textrust
match world.spawn_networked() {
    id => { /* success, entity spawned with fresh NetworkId */ }
}
// spawn_networked() allocates a monotonically increasing NetworkId.
// Duplicate IDs are structurally impossible (AtomicU64::fetch_add).
 ```text

### 8.3 Despawn of Missing Entity

 ```textrust
match world.despawn_networked(network_id) {
    Ok(()) => { /* entity removed from ECS and bimap */ }
    Err(WorldError::EntityNotFound(id)) => {
        tracing::warn!(%id, "despawn of already-despawned entity");
        // Idempotent — no further action needed
    }
}
 ```text

---

## 9. Control Plane Errors — gRPC Status Codes

The Control Plane uses standard gRPC status codes:

| Scenario | gRPC Status | Details |
|---|---|---|
| Invalid credentials | `UNAUTHENTICATED` | Wrong username or password |
| Expired JWT | `UNAUTHENTICATED` | Token past `exp` claim |
| Revoked JWT | `UNAUTHENTICATED` | `jti` in deny-list |
| Invalid request format | `INVALID_ARGUMENT` | Missing or malformed fields |
| Server overloaded | `UNAVAILABLE` | Matchmaking queue full |
| Internal error | `INTERNAL` | Unexpected server failure |
| Rate limited (P2) | `RESOURCE_EXHAUSTED` | Too many requests per second |

### 9.1 Client-Facing Error Messages

gRPC error messages exposed to clients contain **no internal implementation details**:

 ```textrust
// ✗ Bad: leaks internal state
Err(Status::unauthenticated("Argon2 hash verification failed for user admin"))

// ✓ Good: generic message
Err(Status::unauthenticated("Invalid credentials"))
 ```text

---

## 10. Client-Side Error Handling

### 10.1 WebTransport Connection Failure

If the WebTransport connection fails or times out:

1. Game Worker notifies the Main Thread via `postMessage({ type: 'connection_error' })`.
2. Main Thread displays a reconnection UI.
3. Game Worker attempts reconnection with exponential backoff (1s, 2s, 4s, max 30s).

### 10.2 Decode Errors on Client

If the client's `Encoder::decode()` fails:

1. The malformed packet is discarded.
2. A `decode_errors` counter increments.
3. If decode errors exceed 10 per second, the client assumes protocol mismatch and disconnects with an error message suggesting a client version update.

### 10.3 Server Authority Divergence

If the server's authoritative state diverges from the client's prediction by > 10 meters (see [CLIENT_DESIGN.md §3.4](https://github.com/garnizeh-labs/aetheris-client/blob/main/docs/CLIENT_DESIGN.md#34-input-history-buffer)):

1. The client teleports to the server's position (hard snap).
2. A visual "rubberbanding" indicator is briefly shown.
3. The prediction buffer is flushed.

---

## 11. Observability Integration

Every error in Aetheris is observable through metrics and structured logs:

### 11.1 Metrics

| Metric | Labels | Description |
|---|---|---|
| `aetheris_transport_errors_total` | `kind` | Count of transport errors by variant |
| `aetheris_encoder_errors_total` | `kind` | Count of encode/decode errors by variant |
| `aetheris_ecs_errors_total` | `kind` | Count of ECS errors by variant |
| `aetheris_grpc_errors_total` | `code`, `service` | Count of gRPC error responses |

### 11.2 Structured Logs

 ```textjson
{
  "timestamp": "2026-04-15T14:32:00.123Z",
  "level": "WARN",
  "target": "aetheris_server::tick",
  "message": "encode failed, skipping delta",
  "network_id": 9942,
  "component_kind": 3,
  "error": "BufferOverflow { needed: 1250, available: 1200 }",
  "tick": 50240,
  "trace_id": "abc123"
}
 ```text

---

## 12. Performance Contracts

| Operation | Budget | Target |
|---|---|---|
| Error construction (hot path) | < 100 ns | No allocation in error construction |
| Error logging (hot path) | < 1 μs | Structured tracing, no format! |
| Error metric increment | < 50 ns | Atomic counter increment |
| gRPC error response | < 1 ms | tonic Status construction |

---

## 13. Open Questions

| Question | Context | Impact |
|---|---|---|
| **Propagated Errors** | How should internal system errors be reflected to the client? | Debuggability vs. security information leakage. |
| **Error Rate Alerting** | At what error rate should Grafana trigger an alert? | Sensitivity tuning for operators. |
| **Client Error Reporting** | Should the client send error telemetry back to the server? | Debugging client-side issues remotely. |
| **Panic Recovery** | Should `catch_unwind` be used around individual ECS systems? | Fault isolation vs. state corruption risk. |

---

## Appendix A — Glossary

### Mini-Glossary (Quick Reference)

- **Error Domain**: A category of errors scoped to a specific subsystem (Transport, Encoder, ECS, Control Plane).
- **Log-and-Continue**: The error handling strategy for the hot tick path — errors are logged and the tick proceeds.
- **`thiserror`**: A Rust derive macro for ergonomic error type definitions.
- **Malformed Payload**: Input bytes that fail to parse into a valid protocol message.
- **gRPC Status**: Standardized error codes for RPC responses (UNAUTHENTICATED, INVALID_ARGUMENT, etc.).

[Full Glossary Document](https://github.com/garnize/aetheris/blob/main/docs/GLOSSARY.md)

---

## Appendix B — Decision Log

| # | Decision | Rationale | Revisit If... | Date |
|---|---|---|---|---|
| D1 | No `Box<dyn Error>` on hot path | Prevents vtable indirection and enables exhaustive matching. | Error type proliferation makes maintenance painful. | 2026-04-15 |
| D2 | Log-and-continue for tick errors | Tick must complete. One bad entity cannot halt the world. | A class of errors that corrupt global state is discovered. | 2026-04-15 |
| D3 | `thiserror` for all error types | Zero-cost abstraction, auto `Display`, auto `From`. | `thiserror` has a security advisory or API break. | 2026-04-15 |
| D4 | No `catch_unwind` in simulation | A panic indicates a logic bug. Continuing from corrupt state is worse than crashing. | Per-system isolation becomes feasible in P3 scheduler. | 2026-04-15 |
| D5 | Generic gRPC error messages | Prevents leaking internal state to potentially hostile clients. | A secure debug mode for trusted clients is needed. | 2026-04-15 |

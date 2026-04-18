---Version: 0.2.0-draft
Status: Phase 2 — Specified / Phase 4 — Planned
Phase: P2 | P3 | P4
Last Updated: 2026-04-15
Authors: Team (Antigravity)
Spec References: [LC-0300, LC-0700]
Tier: 2
---

# Aetheris Migration — Technical Design Document

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Migration Taxonomy](#2-migration-taxonomy)
3. [Database Schema Migrations (P2)](#3-database-schema-migrations-p2)
4. [Phase Transition — P1 to P3](#4-phase-transition--p1-to-p3)
5. [Protocol Version Migration](#5-protocol-version-migration)
6. [Entity Migration — Cross-Shard (P4)](#6-entity-migration--cross-shard-p4)
7. [Data Migration — Storage Tier Movement](#7-data-migration--storage-tier-movement)
8. [Client Migration — Version Compatibility](#8-client-migration--version-compatibility)
9. [Migration Tooling](#9-migration-tooling)
10. [Rollback Strategy](#10-rollback-strategy)
11. [Testing Migrations](#11-testing-migrations)
12. [Open Questions](#12-open-questions)
13. [Appendix A — Glossary](#appendix-a--glossary)
14. [Appendix B — Decision Log](#appendix-b--decision-log)

---

## Executive Summary

Aetheris has four distinct categories of migration, each with different risk profiles and strategies:

| Category | Phase | Frequency | Risk Level | Downtime? |
|---|---|---|---|---|
| **Database schema** | P2+ | Per release | Medium | Minimal (< 30s) |
| **Phase transition** (P1 → P3) | P3 | Once | High | Planned maintenance |
| **Protocol version** | All | Per breaking change | Medium | Rolling upgrade |
| **Entity cross-shard** | P4 | Continuous, runtime | Low | ≤ 1s freeze per entity |

In P1, there are no migrations — there is no persistence layer and no protocol versioning. This document specifies the migration strategies for P2+ when the persistence layer, protocol versioning, and federation features are introduced.

---

## 2. Migration Taxonomy

### 2.1 Schema Migrations (P2)

Changes to the PostgreSQL/TimescaleDB database schema: new columns, new tables, index changes, retention policy adjustments.

### 2.2 Phase Transitions (P3)

Swapping the Trait Triad implementations from Phase 1 (Bevy + Renet + Serde) to Phase 3 (Custom ECS + Quinn + Bitpack). This is a compile-time feature flag change, but has operational implications for data compatibility.

### 2.3 Protocol Migrations (All)

Changes to the wire format (encoder schema), `ComponentKind` registry, or gRPC API. These require client-server version coordination.

### 2.4 Entity Migrations (P4)

Cross-shard entity hand-off during federation. Live entities are frozen, serialized, transferred to a destination cluster, and respawned. See [FEDERATION_DESIGN.md](FEDERATION_DESIGN.md).

---

## 3. Database Schema Migrations (P2)

### 3.1 Migration Framework

Aetheris uses `sqlx migrate` with numbered SQL files:

```
crates/aetheris-server/migrations/
├── 001_create_entity_events.sql
├── 002_create_entity_snapshots.sql
├── 003_create_audit_checkpoints.sql
├── 004_add_chain_hash_column.sql
└── ...
```

### 3.2 Schema: `entity_events` (TimescaleDB Hypertable)

```sql
CREATE TABLE entity_events (
    tick           BIGINT       NOT NULL,
    network_id     BIGINT       NOT NULL,
    component_kind SMALLINT     NOT NULL,
    server_time    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    payload        BYTEA        NOT NULL,
    chain_hash     BYTEA,
    sequence       BIGINT       NOT NULL
);

-- TimescaleDB hypertable, partitioned by server_time
SELECT create_hypertable('entity_events', 'server_time', chunk_time_interval => INTERVAL '1 day');

-- Indexes
CREATE INDEX idx_entity_events_entity ON entity_events (network_id, tick ASC, sequence ASC);
CREATE INDEX idx_entity_events_tick   ON entity_events (tick ASC, server_time DESC);

-- 90-day rolling retention
SELECT add_retention_policy('entity_events', INTERVAL '90 days');
```

### 3.3 Schema: `entity_snapshots` (PostgreSQL)

```sql
CREATE TABLE entity_snapshots (
    network_id     BIGINT       NOT NULL,
    snapshot_tick  BIGINT       NOT NULL,
    captured_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    state_payload  BYTEA        NOT NULL,
    chain_hash     BYTEA,
    suspicion_score SMALLINT    NOT NULL DEFAULT 0,
    PRIMARY KEY (network_id, snapshot_tick)
);
```

Retention: 10 most recent snapshots per entity. A periodic cleanup job prunes older entries.

### 3.4 Schema: `audit_checkpoints`

```sql
CREATE TABLE audit_checkpoints (
    network_id          BIGINT      NOT NULL PRIMARY KEY,
    last_verified_tick  BIGINT      NOT NULL,
    last_verified_hash  BYTEA       NOT NULL,
    last_behavioral_tick BIGINT     NOT NULL,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

### 3.5 Migration Execution Strategy

| Step | Action | Downtime? |
|---|---|---|
| 1 | Run `sqlx migrate run` against the database | No (additive) |
| 2 | Deploy new server version that uses new schema | ≤ 30s (rolling restart) |
| 3 | Run cleanup migration (drop old columns/indexes) | No (background) |

**Rule: All migrations must be backward-compatible.** The new schema must work with both the old and new server versions for the duration of the rolling upgrade window. This means:

- **Add columns as nullable** or with defaults.
- **Never rename or drop columns** in the same release that adds the replacement.
- **Two-phase migration**: Release N adds new column → Release N+1 stops using old column → Release N+2 drops old column.

### 3.6 Live Migration Safety

The Persistence Sink writes to the database asynchronously from the tick loop (see [PERSISTENCE_DESIGN.md](PERSISTENCE_DESIGN.md)). During a schema migration:

1. The `COPY` protocol write path gracefully handles schema changes (new nullable columns are filled with NULL).
2. If the database is briefly unavailable during migration, the `mpsc` channel buffers events (capacity 1,024 batches).
3. If the buffer fills, events are dropped according to the backpressure tier (baseline = fire-and-forget).

---

## 4. Phase Transition — P1 to P3

### 4.1 What Changes

| Component | Phase 1 | Phase 3 |
|---|---|---|
| ECS | Bevy (`bevy_ecs 0.18`) | Custom (`aetheris-ecs-custom`) |
| Transport | Renet + WebTransport | Quinn QUIC (unified) |
| Encoder | `rmp-serde` (MessagePack) | Bitpack (custom binary) |

### 4.2 Feature Flag Swap

```bash
# Phase 1 (current default)
cargo build -p aetheris-server --features phase1

# Phase 3
cargo build -p aetheris-server --no-default-features --features phase3
```

The `compile_error!` macro prevents both from being active:

```rust
#[cfg(all(feature = "phase1", feature = "phase3"))]
compile_error!("Features 'phase1' and 'phase3' are mutually exclusive.");
```

### 4.3 Data Compatibility Concerns

| Concern | Issue | Mitigation |
|---|---|---|
| Wire format | Bitpack encodes differently than MessagePack | Clients must be updated simultaneously |
| Persistence payloads | Stored `BYTEA` in `entity_events` uses the old encoder | Migration script re-encodes or new events coexist with old |
| Snapshots | `state_payload` in `entity_snapshots` uses old ECS layout | Snapshots taken before transition are invalidated |

### 4.4 Transition Procedure

1. **Announce maintenance window.** Clients cannot hot-swap encoders.
2. **Stop all server instances.**
3. **Build with Phase 3 features.**
4. **Run data migration** (optional): re-encode critical snapshots with the new encoder, or mark old snapshots as stale.
5. **Deploy new server instances.**
6. **Deploy new clients** (WASM build with new encoder).
7. **Verify** via smoke tests and observability dashboards.

---

## 5. Protocol Version Migration

### 5.1 ComponentKind Registry Evolution

When new `ComponentKind` values are added:

| Range | Owner | Migration Impact |
|---|---|---|
| `0x0000–0x00FF` | Aetheris Engine | Server + client update required |
| `0x0100–0x0FFF` | Official extensions | Server update, client optional |
| `0x1000–0xFFFF` | Community | No server update needed |

### 5.2 Wire Format Versioning (P2+)

Each packet will include a version byte in the header:

```
┌─────────┬──────────┬────────────────┐
│ Version │ CompKind │ Payload ...    │
│ (1 byte)│ (2 bytes)│ (variable)     │
└─────────┴──────────┴────────────────┘
```

The encoder checks the version byte and routes to the appropriate decode path. Old versions are supported for a deprecation window (3 releases).

### 5.3 gRPC API Versioning

The gRPC Control Plane uses protobuf's built-in backward compatibility:

- **Adding fields**: Always safe (new fields have default values).
- **Removing fields**: Mark as `reserved`, never reuse field numbers.
- **Changing field types**: Forbidden.

---

## 6. Entity Migration — Cross-Shard (P4)

### 6.1 Hand-Over Protocol

When an entity crosses a zone boundary between shards:

| Step | Actor | Duration |
|---|---|---|
| 1. Detect boundary crossing | Source server | ≤ 50 ms |
| 2. Freeze entity, take snapshot | Source server | ≤ 10 ms |
| 3. `TransferEntityOwnership` (CockroachDB TXN) | Global Coordinator | ≤ 100 ms |
| 4. `EntityArriving` → spawn on destination | Destination server | ≤ 50 ms |
| 5. Confirm `EntityReceived` | Destination server | ≤ 10 ms |
| 6. Despawn entity, redirect client | Source server | ≤ 50 ms |
| **Total freeze window** | | **≤ 1.0 second** |

### 6.2 Failure Modes

| Failure | Recovery |
|---|---|
| Coordinator unavailable | Local cache (5 min TTL), migrations halted |
| Destination crashes mid-transfer | 5s timeout → rollback to source |
| Source crashes after transfer commit | Coordinator resolves → destination is authority |

### 6.3 NetworkId Continuity

Entity `NetworkId` survives cross-shard migration. The `global_entity_owners` table in CockroachDB tracks which cluster currently owns each entity:

```sql
CREATE TABLE global_entity_owners (
    network_id    BIGINT PRIMARY KEY,
    cluster_id    TEXT   NOT NULL,
    state_version BIGINT NOT NULL
);
```

---

## 7. Data Migration — Storage Tier Movement

### 7.1 Tiered Storage Model

| Tier | Substrate | Retention | Migration Trigger |
|---|---|---|---|
| **Hot** | ECS in-memory | Current tick | — |
| **Cold** | TimescaleDB | 90 days | Persistence Sink flush |
| **Warm** | PostgreSQL snapshots | Latest + 10 | Snapshot trigger (every 5,000 ticks) |
| **Ice** | Parquet on S3/R2 | Indefinite | TimescaleDB retention policy expiry |

### 7.2 Cold → Ice Migration

When TimescaleDB chunks expire past the 90-day retention window, they are exported to Parquet format on S3/R2 before deletion:

1. `pg_dump` the expiring chunk to a staging area.
2. Convert to Parquet using a scheduled job.
3. Upload to S3/R2 with lifecycle tagging.
4. Verify upload checksum.
5. Allow TimescaleDB retention policy to drop the chunk.

---

## 8. Client Migration — Version Compatibility

### 8.1 WASM Client Updates

The WASM client is served via Vite and is always loaded fresh on page reload. There is no client-side caching concern — the browser fetches the latest `.wasm` bundle each session.

### 8.2 Native Client Updates

Native clients (P3+) will require a version check on connection:

```
Client connects → sends version header → Server checks compatibility
  ├─ Compatible: proceed
  ├─ Outdated but compatible: warn, proceed
  └─ Incompatible: reject with "update required" message
```

### 8.3 Deprecation Windows

| Change Type | Deprecation Period |
|---|---|
| New `ComponentKind` added | None (additive) |
| `ComponentKind` removed | 3 releases |
| Wire format version bump | 3 releases (old decoder retained) |
| gRPC API field removed | 3 releases (field marked `reserved`) |

---

## 9. Migration Tooling

### 9.1 Current (P1)

No migration tooling — no persistence layer.

### 9.2 Planned (P2+)

| Tool | Purpose |
|---|---|
| `sqlx migrate run` | Apply pending database migrations |
| `sqlx migrate revert` | Roll back the last migration |
| `sqlx migrate info` | Show migration status |
| `just migrate` | Convenience wrapper for `sqlx migrate run` |
| `just migrate-check` | Verify all migrations are applied (CI) |

---

## 10. Rollback Strategy

### 10.1 Database Migrations

Every migration has a corresponding rollback script:

```
migrations/
├── 004_add_chain_hash_column.sql
└── 004_revert_chain_hash_column.sql
```

### 10.2 Phase Transition

Phase transitions are not easily rolled back because:

- Wire format is incompatible between encoders.
- Client bundles must match the encoder.
- Stored payloads may have been written in the new format.

**Mitigation**: Keep the Phase 1 binary and client bundle available for rapid rollback. Old persistence data remains readable.

### 10.3 Entity Migration (P4)

Cross-shard entity migration is atomic at the CockroachDB level. If the destination fails to confirm receipt within 5 seconds, the transaction is rolled back and the entity remains on the source shard.

---

## 11. Testing Migrations

### 11.1 Schema Migration Tests

```rust
#[tokio::test]
async fn test_migration_roundtrip() {
    let db = create_test_database().await;
    sqlx::migrate!("./migrations").run(&db).await.unwrap();
    // Verify schema matches expected state
    // Revert and verify clean rollback
}
```

### 11.2 Phase Transition Tests

1. Build server with Phase 1 features → start → populate state.
2. Stop server.
3. Build server with Phase 3 features → start → verify state.
4. Verify clients can connect with updated encoder.

### 11.3 Protocol Compatibility Tests

Test old-version encoded payloads against the new decoder to ensure backward compatibility within the deprecation window.

---

## 12. Open Questions

| Question | Context | Impact |
|---|---|---|
| **Live Migrations** | How do we handle database migrations for a live running world? | Availability and data integrity. Two-phase migration strategy addresses this. |
| **Cross-Encoder Playback** | Can audit replay work across encoder versions? | Forensic capability during transitions. |
| **Migration Orchestrator** | Should there be a CLI tool for coordinating phase transitions? | Operational safety during major upgrades. |
| **Blue-Green Schema** | Should we maintain two schemas during transition (read old, write new)? | Complexity vs. zero-downtime. |

---

## Appendix A — Glossary

### Mini-Glossary (Quick Reference)

- **Schema Migration**: The process of updating the database structure to a new version.
- **Phase Transition**: Switching from one set of Trait Triad implementations to another (P1 → P3).
- **Entity Migration**: Moving a live entity between shards/clusters in the federation model.
- **Wire Format**: The binary encoding used to serialize components for network transmission.
- **Deprecation Window**: The number of releases during which old behavior is maintained alongside new.

[Full Glossary Document](../GLOSSARY.md)

---

## Appendix B — Decision Log

| # | Decision | Rationale | Revisit If... | Date |
|---|---|---|---|---|
| D1 | `sqlx migrate` for schema migrations | Battle-tested, Rust-native, compile-time checked queries. | Migration complexity exceeds sqlx capabilities. | 2026-04-15 |
| D2 | Two-phase migration (add then remove) | Ensures backward compatibility during rolling upgrades. | Single-step migrations are safe for all changes. | 2026-04-15 |
| D3 | Phase transitions require maintenance window | Wire format incompatibility makes zero-downtime impossible. | A universal encoder fallback is implemented. | 2026-04-15 |
| D4 | 3-release deprecation window | Gives operators time to update clients and tooling. | Rapid iteration pace makes 3 releases too slow. | 2026-04-15 |
| D5 | Atomic entity migration via CockroachDB | Guarantees no entity duplication or loss during cross-shard transfer. | CockroachDB latency exceeds the 1s freeze budget. | 2026-04-15 |

# aetheris-ecs-bevy

Bevy ECS integration for the Aetheris Protocol.

## Overview

This crate provides an implementation of the `WorldState` trait from `aetheris-protocol` specifically for the Bevy ECS. It bridges the gap between the network protocol and Bevy's entity-component system.

## Technical Specifications

- **Adapter**: `BevyWorldAdapter`
- **Capability**: Bridge for `bevy_ecs`.
- **Determinism**: Uses `rand_chacha::ChaCha8Rng` for all internal seeded logic.
- **Hot-Path Contract**: Implements the **Zero Heap Allocation** mandate for `extract_deltas`.

## Performance & Hardening (VS-07)

This crate is the primary target for Aetheris performance hardening.

### Zero-Allocation Benchmarks
The `ecs_pipeline` benchmarks use `alloc_counter` to verify that world-state extraction does not allocate on the heap after the initial warmup.
```bash
rtk cargo bench --bench ecs_pipeline
```

### Deterministic State Hashing
`BevyWorldAdapter` implements `state_hash()` to facilitate bit-perfect regression testing. It hashes:
- `NetworkId` (sorted for stability)
- `TransformComponent` (bit-casted floats)
- `ShipStatsComponent`

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
aetheris-ecs-bevy = "0.8.0"
```

For more details, see the [main repository README](../../README.md).

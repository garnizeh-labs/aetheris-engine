# aetheris-ecs-bevy

Bevy ECS integration for the Aetheris Protocol.

## Overview

This crate provides an implementation of the `WorldState` trait from `aetheris-protocol` specifically for the Bevy ECS. It bridges the gap between the network protocol and Bevy's entity-component system.

## Technical Specifications

- **Adapter**: `BevyWorldAdapter`
- **Capability**: Bridge for `bevy_ecs`.
- **Primary Use**: Game clients and tools that leverage the Bevy engine.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
aetheris-ecs-bevy = "0.1.0"
```

For more details, see the [main repository README](https://github.com/garnizeh-labs/aetheris-engine).

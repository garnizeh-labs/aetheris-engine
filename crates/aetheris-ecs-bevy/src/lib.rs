//! Aetheris ECS Bevy Adapter.
//!
//! Provides the `BevyWorldAdapter` which implements `WorldState` using `bevy_ecs`.
#![warn(clippy::all, clippy::pedantic)]

pub use adapter::BevyWorldAdapter;
pub use components::*;
pub use registry::{ComponentReplicator, DefaultReplicator, ReplicatableComponent};

mod adapter;
pub mod components;
pub mod deterministic_rng;
pub mod mining;
pub mod physics_consts;
pub mod registry;

/// Marker component for entities that are replicated over the network.
/// Stores the global `NetworkId` assigned by the server.
#[derive(
    bevy_ecs::prelude::Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct Networked(pub aetheris_protocol::types::NetworkId);

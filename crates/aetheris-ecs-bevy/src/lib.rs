//! Aetheris ECS Bevy Adapter.
//!
//! Provides the `BevyWorldAdapter` which implements `WorldState` using `bevy_ecs`.
#![warn(clippy::all, clippy::pedantic)]

pub use adapter::{BevyWorldAdapter, Transform};
pub use registry::{ComponentReplicator, DefaultReplicator, ReplicatableComponent};

mod adapter;
mod registry;

/// Marker component for entities that are replicated over the network.
/// Stores the global `NetworkId` assigned by the server.
#[derive(
    bevy_ecs::prelude::Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct Networked(pub aetheris_protocol::types::NetworkId);

/// Component identifying the owner of an entity.
///
/// Used by the server to prevent unauthorized updates from other clients.
#[derive(
    bevy_ecs::prelude::Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct Ownership(pub aetheris_protocol::types::ClientId);

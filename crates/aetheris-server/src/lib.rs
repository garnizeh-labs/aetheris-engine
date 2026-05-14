//! Aetheris server library.
//!
//! Contains the core logic for the authoritative game server, including
//! the tick scheduler and configuration management.

#![warn(clippy::all, clippy::pedantic)]

#[cfg(not(target_arch = "wasm32"))]
/// Authentication and session management for the game server.
pub mod auth;
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod matchmaking;
pub mod multi_transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod telemetry;
#[cfg(not(target_arch = "wasm32"))]
pub mod tick;

pub use multi_transport::MultiTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use tick::TickScheduler;

#[cfg(not(target_arch = "wasm32"))]
/// Bootstraps a Phase 1 world with the default engine components and optional extra registrations.
pub fn bootstrap_phase1_world(
    tick_rate: u64,
    extra_registry: impl FnOnce(&mut aetheris_ecs_bevy::registry::ComponentRegistry),
) -> aetheris_ecs_bevy::BevyWorldAdapter {
    let mut world =
        aetheris_ecs_bevy::BevyWorldAdapter::new(bevy_ecs::world::World::new(), tick_rate);
    let mut registry = aetheris_ecs_bevy::registry::ComponentRegistry::new();
    aetheris_ecs_bevy::registry::register_platform_components(&mut registry);
    extra_registry(&mut registry);

    for descriptor in registry.components.values() {
        world.register_replicator(descriptor.replicator.clone());
    }
    world
}

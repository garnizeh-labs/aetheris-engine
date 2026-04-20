use aetheris_protocol::events::{ComponentUpdate, ReplicationEvent};
use aetheris_protocol::types::{ComponentKind, NetworkId};
use bevy_ecs::change_detection::Tick;
use bevy_ecs::prelude::{Component, Entity, World};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Classification of a component by its intended crate scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentScope {
    /// Engine core components (Transport, Velocity, etc.)
    Core,
    /// Game-specific components (`ShipStats`, Mining, etc.)
    Game,
    /// Purely visual/client-side components.
    Visual,
}

/// Classification of a component by its permanence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentClassification {
    /// Persisted across sessions (e.g., Inventory).
    Persistent,
    /// Reset on every tick (not applicable here, but common in ECS).
    Transient,
    /// Simulation state.
    Simulated,
}

/// Metadata describing a component in the registry.
#[derive(Clone)]
pub struct ComponentDescriptor {
    pub kind: ComponentKind,
    pub name: &'static str,
    pub scope: ComponentScope,
    pub classification: ComponentClassification,
    pub replicator: BoxedReplicator,
}

/// Authoritative registry of all ECS components in the engine.
#[derive(Default, Clone)]
pub struct ComponentRegistry {
    pub components: std::collections::HashMap<ComponentKind, ComponentDescriptor>,
}

impl ComponentRegistry {
    /// Creates a new, empty component registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a component descriptor.
    ///
    /// # Panics
    ///
    /// Panics if a component with the same `ComponentKind` is already registered.
    pub fn register(&mut self, descriptor: ComponentDescriptor) {
        assert!(
            !self.components.contains_key(&descriptor.kind),
            "DUPLICATE ComponentKind registration: {} (already registered as {})",
            descriptor.kind.0,
            descriptor.name
        );
        self.components.insert(descriptor.kind, descriptor);
    }
}

/// Logic for replicating a specific component type.
pub trait ComponentReplicator: Send + Sync {
    /// Returns the `ComponentKind` this replicator handles.
    fn kind(&self) -> ComponentKind;

    /// Extracts a replication event if the component on the given entity has changed.
    fn extract(
        &self,
        world: &World,
        entity: Entity,
        network_id: NetworkId,
        tick: u64,
        last_tick: Option<Tick>,
    ) -> Option<ReplicationEvent>;

    /// Applies a component update to the given entity in the world.
    fn apply(&self, world: &mut World, entity: Entity, update: &ComponentUpdate);
}

/// A type-erased container for a component replicator.
pub type BoxedReplicator = Arc<dyn ComponentReplicator>;

/// Default implementation for any component that implements `Component`.
pub struct DefaultReplicator<T> {
    kind: ComponentKind,
    _marker: std::marker::PhantomData<T>,
}

impl<T: ReplicatableComponent> DefaultReplicator<T> {
    /// Creates a new replicator for type `T`.
    #[must_use]
    pub fn new(kind: ComponentKind) -> Self {
        Self {
            kind,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: ReplicatableComponent> ComponentReplicator for DefaultReplicator<T> {
    fn kind(&self) -> ComponentKind {
        self.kind
    }

    fn extract(
        &self,
        world: &World,
        entity: Entity,
        network_id: NetworkId,
        tick: u64,
        last_tick: Option<Tick>,
    ) -> Option<ReplicationEvent> {
        let component = world.get::<T>(entity)?;
        let ticks = world.get_entity(entity).ok()?.get_change_ticks::<T>()?;

        let current_tick = world.read_change_tick();
        let is_changed = match last_tick {
            Some(last) => ticks.is_changed(last, current_tick),
            None => true, // First extraction, send full state
        };

        if is_changed {
            let payload: Vec<u8> = component.clone().try_into().ok()?;
            Some(ReplicationEvent {
                network_id,
                component_kind: self.kind,
                payload,
                tick,
            })
        } else {
            None
        }
    }

    fn apply(&self, world: &mut World, entity: Entity, update: &ComponentUpdate) {
        if let (Ok(component), Ok(mut entity_mut)) = (
            T::try_from(update.payload.clone()),
            world.get_entity_mut(entity),
        ) {
            entity_mut.insert(component);
        }
    }
}

/// Specialized replicator for client input commands.
/// Implements anti-replay logic by validating client ticks.
pub struct InputCommandReplicator;

impl ComponentReplicator for InputCommandReplicator {
    fn kind(&self) -> ComponentKind {
        aetheris_protocol::types::INPUT_COMMAND_KIND
    }

    fn extract(
        &self,
        _world: &World,
        _entity: Entity,
        _network_id: NetworkId,
        _tick: u64,
        _last_tick: Option<Tick>,
    ) -> Option<ReplicationEvent> {
        // Inbound-only (Client -> Server), never extracted back to clients.
        None
    }

    fn apply(&self, world: &mut World, entity: Entity, update: &ComponentUpdate) {
        use crate::components::LatestInput;
        use aetheris_protocol::types::InputCommand;

        let Ok(command) = rmp_serde::from_slice::<InputCommand>(&update.payload) else {
            return;
        };

        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            if let Some(mut latest) = entity_mut.get_mut::<LatestInput>() {
                // Anti-replay: Only apply if the new tick is strictly greater
                if command.tick > latest.last_client_tick {
                    latest.command = command;
                    latest.last_client_tick = command.tick;
                }
            } else {
                // First input for this entity
                entity_mut.insert(LatestInput {
                    command,
                    last_client_tick: command.tick,
                });
            }
        }
    }
}

/// Trait alias for components that can be replicated.
/// Requires `Component`, `Clone`, and conversion to/from `Vec<u8>`.
pub trait ReplicatableComponent: Component + Clone + TryInto<Vec<u8>> + TryFrom<Vec<u8>> {}
impl<T: Component + Clone + TryInto<Vec<u8>> + TryFrom<Vec<u8>>> ReplicatableComponent for T {}

/// Registers all 31 canonical Void Rush components into the provided registry.
///
/// This implements the authoritative component list for M1020 (14 replicated + 17 server-only).
#[allow(clippy::wildcard_imports, clippy::too_many_lines)]
pub fn register_void_rush_components(registry: &mut ComponentRegistry) {
    use crate::components::*;

    // --- REPLICATED COMPONENTS (1-14) ---

    // 1: Transform (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(1),
        name: "Transform",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<TransformComponent>::new(ComponentKind(
            1,
        ))),
    });

    // 2: Velocity (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(2),
        name: "Velocity",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<Velocity>::new(ComponentKind(2))),
    });

    // 3: ShipStats (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(3),
        name: "ShipStats",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ShipStatsComponent>::new(ComponentKind(
            3,
        ))),
    });

    // 4: Loadout (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(4),
        name: "Loadout",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<Loadout>::new(ComponentKind(4))),
    });

    // 5: ShipClass (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(5),
        name: "ShipClass",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ShipClassComponent>::new(ComponentKind(
            5,
        ))),
    });

    // 6: PlayerName (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(6),
        name: "PlayerName",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<PlayerName>::new(ComponentKind(6))),
    });

    // 7: FactionTag (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(7),
        name: "FactionTag",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<FactionTag>::new(ComponentKind(7))),
    });

    // 8: AsteroidHP (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(8),
        name: "AsteroidHP",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<AsteroidHP>::new(ComponentKind(8))),
    });

    // 9: AsteroidYield (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(9),
        name: "AsteroidYield",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<AsteroidYield>::new(ComponentKind(9))),
    });

    // 10: LootDrop (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(10),
        name: "LootDrop",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<LootDrop>::new(ComponentKind(10))),
    });

    // 11: Station (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(11),
        name: "Station",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<Station>::new(ComponentKind(11))),
    });

    // 12: JumpGate (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(12),
        name: "JumpGate",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<JumpGate>::new(ComponentKind(12))),
    });

    // 13: ProjectileMarker (Game)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(13),
        name: "ProjectileMarker",
        scope: ComponentScope::Game,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ProjectileMarker>::new(ComponentKind(
            13,
        ))),
    });

    // 14: DockedState (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(14),
        name: "DockedState",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<DockedState>::new(ComponentKind(14))),
    });

    // 128: InputCommand (Core Extension - Inbound Only)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::INPUT_COMMAND_KIND,
        name: "InputCommand",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Transient,
        replicator: Arc::new(InputCommandReplicator),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_void_rush_registry_completeness() {
        let mut registry = ComponentRegistry::new();
        register_void_rush_components(&mut registry);

        // Verify we have 15 components (14 replicated + 1 transient input)
        assert_eq!(
            registry.components.len(),
            15,
            "Registry MUST contain 15 components (14 replicated + 1 input)"
        );

        // Verify canonical IDs 1-14 are present (M1020)
        for i in 1..=14 {
            let kind = ComponentKind(i);
            assert!(
                registry.components.contains_key(&kind),
                "Missing canonical ComponentKind({i})"
            );
        }

        // Verify InputCommand (128) is present
        assert!(
            registry
                .components
                .contains_key(&aetheris_protocol::types::INPUT_COMMAND_KIND),
            "Missing InputCommand(128)"
        );
    }

    #[test]
    fn test_bijectivity() {
        let mut registry = ComponentRegistry::new();
        register_void_rush_components(&mut registry);

        let mut names = std::collections::HashSet::new();
        for desc in registry.components.values() {
            assert!(
                names.insert(desc.name),
                "Duplicate component name in registry: {}",
                desc.name
            );
        }
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_input_replicator_anti_replay() {
        use crate::components::LatestInput;
        use aetheris_protocol::events::ComponentUpdate;
        use aetheris_protocol::types::{ComponentKind, InputCommand, NetworkId};

        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let replicator = InputCommandReplicator;

        let cmd1 = InputCommand {
            tick: 100,
            move_x: 1.0,
            move_y: 0.0,
            actions: 0,
        };
        let payload1 = rmp_serde::to_vec(&cmd1).unwrap();

        // 1. Initial apply
        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: payload1,
                tick: 0,
            },
        );

        let latest = world.get::<LatestInput>(entity).unwrap();
        assert_eq!(latest.last_client_tick, 100);
        assert_eq!(latest.command.move_x, 1.0);

        // 2. Replay apply (same tick) -> Should be ignored
        let cmd2 = InputCommand {
            tick: 100,
            move_x: 0.0,
            move_y: 1.0, // Different value
            actions: 0,
        };
        let payload2 = rmp_serde::to_vec(&cmd2).unwrap();
        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: payload2,
                tick: 0,
            },
        );

        let latest = world.get::<LatestInput>(entity).unwrap();
        assert_eq!(latest.last_client_tick, 100);
        assert_eq!(latest.command.move_x, 1.0, "Replayed input must be ignored");

        // 3. Newer tick -> Should be applied
        let cmd3 = InputCommand {
            tick: 101,
            move_x: 0.5,
            move_y: 0.5,
            actions: 0,
        };
        let payload3 = rmp_serde::to_vec(&cmd3).unwrap();
        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: payload3,
                tick: 0,
            },
        );

        let latest = world.get::<LatestInput>(entity).unwrap();
        assert_eq!(latest.last_client_tick, 101);
        assert_eq!(latest.command.move_x, 0.5);
    }
}

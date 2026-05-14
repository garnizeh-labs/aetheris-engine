use aetheris_protocol::events::{ComponentUpdate, ReplicationEvent};
use aetheris_protocol::types::{BEAM_MARKER_KIND, ComponentKind, NetworkId};
use bevy_ecs::change_detection::Tick;
use bevy_ecs::prelude::{Component, Entity, World};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Classification of a component by its intended crate scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentScope {
    /// Engine core components (Transport, Velocity, etc.)
    Core,
    /// Platform-specific components (`AgentProperties`, Extraction, etc.)
    Platform,
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
const MAX_FORWARD_TICK_JUMP: u64 = 600;

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
        // 1. Verify Ownership and Session (M1013 §4.2)
        if !Self::validate_access_gate(world, entity, update.network_id) {
            return;
        }

        // 2. Deserialize and Validate Command
        let Some(command) = Self::deserialize_command(&update.payload, update.network_id) else {
            return;
        };

        // 3. Apply Update to World State
        Self::apply_command_update(world, entity, update.network_id, command);
    }
}

impl InputCommandReplicator {
    /// Validates that the entity exists, has an owner, and is a session agent.
    fn validate_access_gate(world: &World, entity: Entity, nid: NetworkId) -> bool {
        use crate::components::{NetworkOwner, SessionAgent};

        let has_owner = world.get::<NetworkOwner>(entity).is_some();
        let has_session = world.get::<SessionAgent>(entity).is_some();

        tracing::debug!(
            network_id = nid.0,
            has_owner,
            has_session,
            "[InputCmd] gate check"
        );

        if !has_owner {
            tracing::warn!(
                network_id = nid.0,
                "Rejected InputCommand: Entity missing Ownership"
            );
            return false;
        }

        if !has_session {
            tracing::warn!(
                network_id = nid.0,
                has_owner,
                "[InputCmd] REJECTED: Entity is not a session agent (missing SessionAgent marker)"
            );
            return false;
        }

        true
    }

    /// Deserializes and validates the input command from the network payload.
    fn deserialize_command(
        payload: &[u8],
        nid: NetworkId,
    ) -> Option<aetheris_protocol::types::InputCommand> {
        use aetheris_protocol::types::InputCommand;

        match rmp_serde::from_slice::<InputCommand>(payload) {
            Ok(cmd) => {
                if let Err(e) = cmd.validate() {
                    tracing::warn!(
                        network_id = nid.0,
                        error = e,
                        "Rejected InputCommand: Validation failed"
                    );
                    None
                } else {
                    Some(cmd)
                }
            }
            Err(e) => {
                tracing::warn!(
                    network_id = nid.0,
                    error = ?e,
                    "Rejected InputCommand: Deserialization failed"
                );
                None
            }
        }
    }

    /// Updates the entity's `LatestInput` component with anti-replay and synchronization logic.
    fn apply_command_update(
        world: &mut World,
        entity: Entity,
        nid: NetworkId,
        mut command: aetheris_protocol::types::InputCommand,
    ) {
        use crate::components::{LatestInput, ServerTick};

        let server_tick = world.get_resource::<ServerTick>().map_or_else(
            || {
                tracing::warn!(
                    network_id = nid.0,
                    "[InputCmd] ServerTick resource missing — defaulting to 0. \
                     Initialization may be incomplete."
                );
                0
            },
            |t| t.0,
        );

        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            if let Some(mut latest) = entity_mut.get_mut::<LatestInput>() {
                // M1038: Anti-replay and window validation.
                // We allow a command if it satisfies EITHER:
                // 1. It is a valid progression from the last heard client tick (within 600 ticks).
                // 2. It is roughly aligned with our authoritative ServerTick resource (Resync Fallback).

                let is_valid_jump = command.tick > latest.last_client_tick
                    && command.tick
                        <= latest
                            .last_client_tick
                            .saturating_add(MAX_FORWARD_TICK_JUMP);

                let is_in_server_window = command.tick
                    <= server_tick.saturating_add(MAX_FORWARD_TICK_JUMP)
                    && command.tick >= server_tick.saturating_sub(MAX_FORWARD_TICK_JUMP);

                let last_tick_in_window = latest.last_client_tick
                    <= server_tick.saturating_add(MAX_FORWARD_TICK_JUMP)
                    && latest.last_client_tick >= server_tick.saturating_sub(MAX_FORWARD_TICK_JUMP);

                if is_valid_jump || (is_in_server_window && !last_tick_in_window) {
                    let old_tick = latest.last_client_tick;
                    latest.last_client_tick = command.tick;

                    if command.actions.is_empty() {
                        tracing::debug!(
                            network_id = nid.0,
                            tick = command.tick,
                            old_tick,
                            "[InputCmd] Updated InputCommand (no actions)"
                        );
                    } else {
                        tracing::debug!(
                            network_id = nid.0,
                            tick = command.tick,
                            old_tick,
                            actions = command.actions.len(),
                            "[InputCmd] Updated InputCommand with actions"
                        );
                    }
                    latest.command = command;
                } else {
                    tracing::warn!(
                        network_id = nid.0,
                        client_tick = command.tick,
                        last_tick = latest.last_client_tick,
                        max_jump = MAX_FORWARD_TICK_JUMP,
                        "[InputCmd] InputCommand rejected (tick window mismatch)"
                    );
                }
            } else {
                // First input for this entity: validate against authoritative server tick
                let original_tick = command.tick;
                let capped_tick =
                    original_tick.min(server_tick.saturating_add(MAX_FORWARD_TICK_JUMP));

                command.tick = capped_tick;

                tracing::debug!(
                    network_id = nid.0,
                    client_tick = original_tick,
                    server_tick,
                    capped_tick,
                    "[InputCmd] First input for entity — Inserting LatestInput"
                );
                entity_mut.insert(LatestInput {
                    command,
                    last_client_tick: capped_tick,
                });
            }
        } else {
            tracing::error!(
                network_id = nid.0,
                "Failed to get entity_mut for InputCommand"
            );
        }
    }
}

/// Trait alias for components that can be replicated.
/// Requires `Component`, `Clone`, and conversion to/from `Vec<u8>`.
pub trait ReplicatableComponent: Component + Clone + TryInto<Vec<u8>> + TryFrom<Vec<u8>> {}
impl<T: Component + Clone + TryInto<Vec<u8>> + TryFrom<Vec<u8>>> ReplicatableComponent for T {}

/// Registers all 31 canonical platform components into the provided registry.
///
/// This implements the authoritative component list for M1020 (14 replicated + 17 server-only).
#[allow(clippy::wildcard_imports, clippy::too_many_lines)]
pub fn register_platform_components(registry: &mut ComponentRegistry) {
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

    // 3: AgentProperties (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(3),
        name: "AgentProperties",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<AgentPropertiesComponent>::new(
            ComponentKind(3),
        )),
    });

    // 4: AgentConfiguration (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(4),
        name: "AgentConfiguration",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<AgentConfiguration>::new(ComponentKind(
            4,
        ))),
    });

    // 5: AgentKind (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(5),
        name: "AgentKind",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<AgentKindComponent>::new(ComponentKind(
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

    // 7: FactionTag (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(7),
        name: "FactionTag",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<FactionTag>::new(ComponentKind(7))),
    });

    // 8: ResourceIntegrity (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(8),
        name: "ResourceIntegrity",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ResourceIntegrity>::new(ComponentKind(
            8,
        ))),
    });

    // 9: ResourceYield (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(9),
        name: "ResourceYield",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ResourceYield>::new(ComponentKind(9))),
    });

    // 10: DataDrop (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(10),
        name: "DataDrop",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<DataDrop>::new(ComponentKind(10))),
    });

    // 11: Station (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(11),
        name: "Station",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<Station>::new(ComponentKind(11))),
    });

    // 12: ZoneGate (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(12),
        name: "ZoneGate",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ZoneGate>::new(ComponentKind(12))),
    });

    // 13: BeamMarker (Platform)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(13),
        name: "BeamMarker",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<BeamMarker>::new(BEAM_MARKER_KIND)),
    });

    // 14: DockedState (Core)
    registry.register(ComponentDescriptor {
        kind: ComponentKind(14),
        name: "DockedState",
        scope: ComponentScope::Core,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<DockedState>::new(ComponentKind(14))),
    });

    // 1024: ExtractionBeam (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::EXTRACTION_BEAM_KIND,
        name: "ExtractionBeam",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ExtractionBeam>::new(
            aetheris_protocol::types::EXTRACTION_BEAM_KIND,
        )),
    });

    // 1025: DataStore (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::DATA_STORE_KIND,
        name: "DataStore",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<DataStore>::new(
            aetheris_protocol::types::DATA_STORE_KIND,
        )),
    });

    // 1026: Resource (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::RESOURCE_KIND,
        name: "Resource",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<Resource>::new(
            aetheris_protocol::types::RESOURCE_KIND,
        )),
    });

    // 1027: ToolComponent (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::TOOL_KIND,
        name: "ToolComponent",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<ToolComponent>::new(
            aetheris_protocol::types::TOOL_KIND,
        )),
    });

    // 1028: PriorityPoolComponent (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::PRIORITY_POOL_KIND,
        name: "PriorityPoolComponent",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<PriorityPoolComponent>::new(
            aetheris_protocol::types::PRIORITY_POOL_KIND,
        )),
    });

    // 1029: IntegrityPoolComponent (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::INTEGRITY_POOL_KIND,
        name: "IntegrityPoolComponent",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<IntegrityPoolComponent>::new(
            aetheris_protocol::types::INTEGRITY_POOL_KIND,
        )),
    });

    // 1030: DataDropComponent (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::DATA_DROP_KIND,
        name: "DataDropComponent",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<DataDropComponent>::new(
            aetheris_protocol::types::DATA_DROP_KIND,
        )),
    });

    // 129: WorkspaceDefinition (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::WORKSPACE_DEFINITION_KIND,
        name: "WorkspaceDefinition",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<WorkspaceDefinitionComponent>::new(
            aetheris_protocol::types::WORKSPACE_DEFINITION_KIND,
        )),
    });

    // 130: WorkspaceBounds (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::WORKSPACE_BOUNDS_KIND,
        name: "WorkspaceBounds",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<WorkspaceBoundsComponent>::new(
            aetheris_protocol::types::WORKSPACE_BOUNDS_KIND,
        )),
    });

    // 131: WorkspaceMembership (Platform)
    registry.register(ComponentDescriptor {
        kind: aetheris_protocol::types::WORKSPACE_MEMBERSHIP_KIND,
        name: "WorkspaceMembership",
        scope: ComponentScope::Platform,
        classification: ComponentClassification::Simulated,
        replicator: Arc::new(DefaultReplicator::<WorkspaceMembershipComponent>::new(
            aetheris_protocol::types::WORKSPACE_MEMBERSHIP_KIND,
        )),
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
        register_platform_components(&mut registry);

        // Verify we have 25 components (14 replicated core + 3 platform + 3 workspace + 4 platform + 1 transient input)
        assert_eq!(
            registry.components.len(),
            25,
            "Registry MUST contain 25 components (24 replicated + 1 input)"
        );

        // Verify canonical IDs 1-14 are present (M1020)
        for i in 1..=14 {
            let kind = ComponentKind(i);
            assert!(
                registry.components.contains_key(&kind),
                "Missing canonical ComponentKind({i})"
            );
        }

        // Verify Platform IDs are present
        assert!(
            registry
                .components
                .contains_key(&aetheris_protocol::types::EXTRACTION_BEAM_KIND)
        );
        assert!(
            registry
                .components
                .contains_key(&aetheris_protocol::types::DATA_STORE_KIND)
        );
        assert!(
            registry
                .components
                .contains_key(&aetheris_protocol::types::RESOURCE_KIND)
        );

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
        register_platform_components(&mut registry);

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
        use crate::components::{LatestInput, NetworkOwner};
        use aetheris_protocol::events::ComponentUpdate;
        use aetheris_protocol::types::{
            ClientId, ComponentKind, InputCommand, NetworkId, PlayerInputKind,
        };

        let mut world = World::new();
        let entity = world
            .spawn((NetworkOwner(ClientId(1)), crate::components::SessionAgent))
            .id();
        let replicator = InputCommandReplicator;

        let cmd1 = InputCommand {
            tick: 100,
            actions: vec![PlayerInputKind::Move { x: 1.0, y: 0.0 }],
            actions_mask: 0,
            last_seen_input_tick: None,
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
        if let PlayerInputKind::Move { x, .. } = latest.command.actions[0] {
            assert_eq!(x, 1.0);
        } else {
            panic!("Expected Move action");
        }

        // 2. Replay apply (same tick) -> Should be ignored
        let cmd2 = InputCommand {
            tick: 100,
            actions: vec![PlayerInputKind::Move { x: 0.0, y: 1.0 }],
            actions_mask: 0,
            last_seen_input_tick: None,
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
        if let PlayerInputKind::Move { x, .. } = latest.command.actions[0] {
            assert_eq!(x, 1.0, "Replayed input must be ignored");
        } else {
            panic!("Expected Move action");
        }

        // 3. Newer tick -> Should be applied
        let cmd3 = InputCommand {
            tick: 101,
            actions: vec![PlayerInputKind::Move { x: 0.5, y: 0.5 }],
            actions_mask: 0,
            last_seen_input_tick: None,
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
        if let PlayerInputKind::Move { x, .. } = latest.command.actions[0] {
            assert_eq!(x, 0.5);
        } else {
            panic!("Expected Move action");
        }
    }

    #[test]
    fn test_input_replicator_rejected_tick_jump() {
        use crate::components::{LatestInput, NetworkOwner};
        use aetheris_protocol::events::ComponentUpdate;
        use aetheris_protocol::types::{ClientId, ComponentKind, InputCommand, NetworkId};

        let mut world = World::new();
        let entity = world
            .spawn((NetworkOwner(ClientId(1)), crate::components::SessionAgent))
            .id();
        let replicator = InputCommandReplicator;

        // 1. Establish baseline
        let cmd1 = InputCommand {
            tick: 100,
            actions: vec![],
            actions_mask: 0,
            last_seen_input_tick: None,
        };
        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: rmp_serde::to_vec(&cmd1).unwrap(),
                tick: 0,
            },
        );

        assert_eq!(
            world.get::<LatestInput>(entity).unwrap().last_client_tick,
            100
        );

        // 2. Attempt huge jump
        let cmd2 = InputCommand {
            tick: 100 + MAX_FORWARD_TICK_JUMP + 1,
            actions: vec![],
            actions_mask: 0,
            last_seen_input_tick: None,
        };
        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: rmp_serde::to_vec(&cmd2).unwrap(),
                tick: 0,
            },
        );

        // Should be unchanged
        assert_eq!(
            world.get::<LatestInput>(entity).unwrap().last_client_tick,
            100
        );
    }

    #[test]
    fn test_input_replicator_no_session_ship() {
        use crate::components::{LatestInput, NetworkOwner};
        use aetheris_protocol::events::ComponentUpdate;
        use aetheris_protocol::types::{ClientId, ComponentKind, InputCommand, NetworkId};

        let mut world = World::new();
        // Entity has owner but LACKS SessionShip
        let entity = world.spawn(NetworkOwner(ClientId(1))).id();
        let replicator = InputCommandReplicator;

        let cmd = InputCommand {
            tick: 100,
            actions: vec![],
            actions_mask: 0,
            last_seen_input_tick: None,
        };

        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: rmp_serde::to_vec(&cmd).unwrap(),
                tick: 0,
            },
        );

        // Should not have created LatestInput because SessionShip was missing
        assert!(world.get::<LatestInput>(entity).is_none());
    }

    #[test]
    fn test_input_replicator_no_ownership() {
        use crate::components::LatestInput;
        use aetheris_protocol::events::ComponentUpdate;
        use aetheris_protocol::types::{ComponentKind, InputCommand, NetworkId};

        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let replicator = InputCommandReplicator;

        let cmd = InputCommand {
            tick: 100,
            actions: vec![],
            actions_mask: 0,
            last_seen_input_tick: None,
        };

        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: rmp_serde::to_vec(&cmd).unwrap(),
                tick: 0,
            },
        );

        // Should not have created LatestInput
        assert!(world.get::<LatestInput>(entity).is_none());
    }

    #[test]
    fn test_input_replicator_malformed_payload() {
        use crate::components::{LatestInput, NetworkOwner, SessionAgent};
        use aetheris_protocol::events::ComponentUpdate;
        use aetheris_protocol::types::{ClientId, ComponentKind, NetworkId};

        let mut world = World::new();
        // SessionShip marker required: InputCommandReplicator gates on its presence
        let entity = world.spawn((NetworkOwner(ClientId(1)), SessionAgent)).id();
        let replicator = InputCommandReplicator;

        replicator.apply(
            &mut world,
            entity,
            &ComponentUpdate {
                network_id: NetworkId(1),
                component_kind: ComponentKind(128),
                payload: vec![0xFF, 0x00, 0x42], // Invalid MessagePack
                tick: 0,
            },
        );

        // Should not have created LatestInput
        assert!(world.get::<LatestInput>(entity).is_none());
    }
}

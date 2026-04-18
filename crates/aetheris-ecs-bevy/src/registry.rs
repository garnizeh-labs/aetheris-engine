use std::sync::Arc;

use aetheris_protocol::events::{ComponentUpdate, ReplicationEvent};
use aetheris_protocol::types::{ComponentKind, NetworkId};
use bevy_ecs::change_detection::Tick;
use bevy_ecs::prelude::{Component, Entity, World};

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
            Some(ReplicationEvent {
                network_id,
                component_kind: self.kind,
                payload: component.clone().into(),
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

// Add TryFrom bound for T to support apply
/// Trait alias for components that can be replicated.
/// Requires `Component`, `Clone`, and conversion to/from `Vec<u8>`.
pub trait ReplicatableComponent: Component + Clone + Into<Vec<u8>> + TryFrom<Vec<u8>> {}
impl<T: Component + Clone + Into<Vec<u8>> + TryFrom<Vec<u8>>> ReplicatableComponent for T {}

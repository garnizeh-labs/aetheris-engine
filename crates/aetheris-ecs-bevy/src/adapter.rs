use bevy_ecs::change_detection::Tick;
use bevy_ecs::prelude::{Entity, World};
use bimap::BiHashMap;
use std::collections::BTreeMap;

use aetheris_protocol::error::WorldError;
use aetheris_protocol::events::{ComponentUpdate, ReplicationEvent};
use aetheris_protocol::traits::WorldState;
use aetheris_protocol::types::{ClientId, ComponentKind, LocalId, NetworkId};

use crate::Networked;
use crate::registry::BoxedReplicator;
use aetheris_protocol::types::Transform as ProtocolTransform;

/// Adapts a Bevy ECS World to the `WorldState` trait.
pub struct BevyWorldAdapter {
    world: World,
    bimap: BiHashMap<NetworkId, Entity>,
    owners: std::collections::HashMap<NetworkId, ClientId>,
    replicators: BTreeMap<ComponentKind, BoxedReplicator>,
    allocator: aetheris_protocol::types::NetworkIdAllocator,
    last_extraction_tick: Option<Tick>,
}

impl Default for BevyWorldAdapter {
    fn default() -> Self {
        Self::new(World::new())
    }
}

impl BevyWorldAdapter {
    /// Creates a new adapter wrapping the given Bevy world.
    pub fn new(world: World) -> Self {
        Self {
            world,
            bimap: BiHashMap::new(),
            owners: std::collections::HashMap::new(),
            replicators: BTreeMap::new(),
            allocator: aetheris_protocol::types::NetworkIdAllocator::new(1),
            last_extraction_tick: None,
        }
    }

    /// Registers a component replicator for a specific `ComponentKind`.
    pub fn register_replicator(&mut self, replicator: BoxedReplicator) {
        self.replicators.insert(replicator.kind(), replicator);
    }

    /// Access the underlying Bevy world.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Access the underlying Bevy world mutably.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }
}

impl WorldState for BevyWorldAdapter {
    fn get_local_id(&self, network_id: NetworkId) -> Option<LocalId> {
        self.bimap
            .get_by_left(&network_id)
            .map(|e| LocalId(e.to_bits()))
    }

    fn get_network_id(&self, local_id: LocalId) -> Option<NetworkId> {
        let entity = Entity::from_bits(local_id.0);
        self.bimap.get_by_right(&entity).copied()
    }

    #[tracing::instrument(skip(self))]
    fn extract_deltas(&mut self) -> Vec<ReplicationEvent> {
        let mut deltas = Vec::new();
        let current_tick = self.world.change_tick();
        // Bevy's change tick is internally a u32, but our protocol uses u64
        let tick = u64::from(current_tick.get());

        // For each networked entity in our bimap
        for (&network_id, &entity) in &self.bimap {
            // For each registered replicator
            for replicator in self.replicators.values() {
                if let Some(event) = replicator.extract(
                    &self.world,
                    entity,
                    network_id,
                    tick,
                    self.last_extraction_tick,
                ) {
                    deltas.push(event);
                }
            }
        }

        self.last_extraction_tick = Some(current_tick);

        metrics::counter!("aetheris_ecs_extraction_count").increment(deltas.len() as u64);
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!("aetheris_ecs_entities_networked").set(self.bimap.len() as f64);

        deltas
    }

    fn apply_updates(&mut self, updates: &[(ClientId, ComponentUpdate)]) {
        let mut applied_count = 0u64;
        let mut unauthorized_count = 0u64;

        // Cache for the last used replicator to avoid BTreeMap lookups in batches
        let mut last_kind = None;
        let mut last_replicator = None;

        for (client_id, update) in updates {
            let Some(&entity) = self.bimap.get_by_left(&update.network_id) else {
                continue;
            };

            // 1. Verify ownership (Hot path optimized via owners cache)
            let is_authorized = if let Some(&owner_id) = self.owners.get(&update.network_id) {
                owner_id == *client_id
            } else {
                // Fallback to slow ECS lookup for entities not in cache
                self.world.get::<crate::Ownership>(entity).is_some_and(|o| {
                    self.owners.insert(update.network_id, o.0);
                    o.0 == *client_id
                })
            };

            if !is_authorized {
                unauthorized_count += 1;
                tracing::warn!(?client_id, network_id = ?update.network_id, "Unauthorized update attempt");
                continue;
            }

            // 2. Resolve replicator (Hot path optimized via kind caching)
            let replicator = if last_kind == Some(update.component_kind) {
                last_replicator
            } else {
                last_kind = Some(update.component_kind);
                last_replicator = self.replicators.get(&update.component_kind);
                last_replicator
            };

            if let Some(replicator) = replicator {
                replicator.apply(&mut self.world, entity, update);
                applied_count += 1;
            }
        }
        metrics::counter!("aetheris_ecs_updates_applied_total").increment(applied_count);
        metrics::counter!("aetheris_ecs_unauthorized_updates_total").increment(unauthorized_count);
    }

    fn advance_tick(&mut self) {
        // Advance the Bevy change tick before Stage 2 (input application / entity spawning).
        // This ensures newly spawned entities receive a tick strictly greater than
        // `last_extraction_tick`, so Bevy's `is_changed` check detects them as new in
        // the same `extract_deltas` call.  Without this pre-increment the spawned entities
        // share the same tick as `last_extraction_tick` and are silently skipped.
        self.world.increment_change_tick();
    }

    #[tracing::instrument(skip(self))]
    fn simulate(&mut self) {
        // In Phase 1, we advance the world tick to support Bevy's change detection.
        // `increment_change_tick` was moved to `advance_tick` (called before Stage 2) so
        // that entities spawned by inputs have a higher tick than `last_extraction_tick`.
        // `clear_trackers` still calls `increment_change_tick` internally, providing a
        // second increment that keeps the delta window correct.
        // Full system execution via Schedules will be implemented in M300.
        self.world.clear_trackers();
    }

    fn spawn_networked(&mut self) -> NetworkId {
        let id = self
            .allocator
            .allocate()
            .expect("NetworkId allocation failed (exhausted)");
        let entity = self.world.spawn(Networked(id)).id();
        self.bimap.insert(id, entity);
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!("aetheris_ecs_entities_networked").set(self.bimap.len() as f64);
        id
    }

    fn spawn_networked_for(&mut self, client_id: ClientId) -> NetworkId {
        let id = self.spawn_networked();
        let entity = *self.bimap.get_by_left(&id).expect("Spawned but missing id");
        self.world
            .entity_mut(entity)
            .insert(crate::Ownership(client_id));
        self.owners.insert(id, client_id);
        id
    }

    fn despawn_networked(&mut self, network_id: NetworkId) -> Result<(), WorldError> {
        if let Some(entity) = self.bimap.remove_by_left(&network_id).map(|(_, e)| e) {
            self.owners.remove(&network_id);
            if let Ok(entity_mut) = self.world.get_entity_mut(entity) {
                entity_mut.despawn();
                #[allow(clippy::cast_precision_loss)]
                metrics::gauge!("aetheris_ecs_entities_networked").set(self.bimap.len() as f64);
                Ok(())
            } else {
                Err(WorldError::EntityNotFound(network_id))
            }
        } else {
            Err(WorldError::EntityNotFound(network_id))
        }
    }

    fn stress_test(&mut self, count: u16, rotate: bool) {
        tracing::info!(count, rotate, "Executing server-side stress test");
        // Use a simple LCG to produce deterministic-ish pseudo-random positions
        // without pulling in a full RNG dependency.
        let mut seed: u32 = 0xDEAD_BEEF;
        let lcg_next = |s: &mut u32| -> f32 {
            *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // Map to [-20, 20]
            #[allow(clippy::cast_precision_loss)]
            let v = (*s as f32 / u32::MAX as f32) * 40.0 - 20.0;
            v
        };
        let entity_types: [u16; 5] = [1, 3, 4, 5, 6];
        for i in 0..count {
            let x = lcg_next(&mut seed);
            let y = lcg_next(&mut seed);
            let rot = lcg_next(&mut seed) * std::f32::consts::PI / 20.0; // [−π, π]
            let kind = entity_types[i as usize % entity_types.len()];
            self.spawn_kind(kind, x, y, rot);
        }
    }

    fn spawn_kind(&mut self, kind: u16, x: f32, y: f32, rot: f32) -> NetworkId {
        let network_id = self
            .allocator
            .allocate()
            .expect("NetworkId allocation failed (exhausted)");
        let entity = self
            .world
            .spawn((
                crate::Networked(network_id),
                Transform {
                    x,
                    y,
                    z: 0.0,
                    rotation: rot,
                    entity_type: kind,
                },
            ))
            .id();
        self.bimap.insert(network_id, entity);
        network_id
    }

    fn clear_world(&mut self) {
        tracing::info!("Clearing all networked entities from the world");
        let ids: Vec<_> = self.bimap.iter().map(|(&id, _)| id).collect();
        for id in ids {
            let _ = self.despawn_networked(id);
        }
        self.allocator.reset();
    }
}

/// Bevy-compatible Transform component that replicates via the protocol Transform.
#[derive(bevy_ecs::prelude::Component, Clone, Debug, PartialEq)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation: f32,
    pub entity_type: u16,
}

impl From<Transform> for Vec<u8> {
    fn from(t: Transform) -> Self {
        let p = ProtocolTransform {
            x: t.x,
            y: t.y,
            z: t.z,
            rotation: t.rotation,
            entity_type: t.entity_type,
        };
        rmp_serde::to_vec(&p).unwrap_or_default()
    }
}

impl TryFrom<Vec<u8>> for Transform {
    // Default replicator expects a Result, error type doesn't strictly matter as long as it's not Ok
    type Error = String;
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        rmp_serde::from_slice::<ProtocolTransform>(&value)
            .map(|p| Transform {
                x: p.x,
                y: p.y,
                z: p.z,
                rotation: p.rotation,
                entity_type: p.entity_type,
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DefaultReplicator;
    use bevy_ecs::prelude::Component;
    use std::sync::Arc;

    #[derive(Component, Clone, Debug, PartialEq)]
    struct MockPos(u32);

    impl From<MockPos> for Vec<u8> {
        fn from(pos: MockPos) -> Self {
            pos.0.to_le_bytes().to_vec()
        }
    }

    impl TryFrom<Vec<u8>> for MockPos {
        type Error = ();
        fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
            if value.len() == 4 {
                let bytes: [u8; 4] = value.try_into().unwrap();
                Ok(MockPos(u32::from_le_bytes(bytes)))
            } else {
                Err(())
            }
        }
    }

    #[test]
    fn test_lifecycle() {
        let mut adapter = BevyWorldAdapter::default();

        // Spawn
        let nid = adapter.spawn_networked();
        let lid = adapter
            .get_local_id(nid)
            .expect("ID mapping failed after spawn");
        assert_eq!(adapter.get_network_id(lid), Some(nid));

        // Entity exists in world
        let entity = Entity::from_bits(lid.0);
        assert!(adapter.world().get::<Networked>(entity).is_some());

        // Despawn
        adapter.despawn_networked(nid).expect("Despawn failed");
        assert_eq!(adapter.get_local_id(nid), None);
        // Entity should be gone
        assert!(adapter.world().get_entity(entity).is_err());
    }

    #[test]
    fn test_replication_roundtrip() {
        let mut adapter = BevyWorldAdapter::default();
        let kind = ComponentKind(1);
        adapter.register_replicator(Arc::new(DefaultReplicator::<MockPos>::new(kind)));

        // Spawn entity
        let nid = adapter.spawn_networked();
        let entity = Entity::from_bits(adapter.get_local_id(nid).unwrap().0);

        // Insert component
        adapter.world_mut().entity_mut(entity).insert(MockPos(42));

        // Extraction
        let deltas = adapter.extract_deltas();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].network_id, nid);
        assert_eq!(deltas[0].component_kind, kind);
        assert_eq!(deltas[0].payload, vec![42, 0, 0, 0]);

        // Apply update to another environment
        let mut client_adapter = BevyWorldAdapter::default();
        client_adapter.register_replicator(Arc::new(DefaultReplicator::<MockPos>::new(kind)));

        let nid = client_adapter.spawn_networked_for(ClientId(1));
        let lid = client_adapter.get_local_id(nid).unwrap();
        let client_entity = bevy_ecs::entity::Entity::from_bits(lid.0);

        let update = ComponentUpdate {
            network_id: nid,
            component_kind: kind,
            payload: vec![100, 0, 0, 0],
            tick: 1, // Bevy world starts at tick 0, first change is tick 1
        };

        client_adapter.apply_updates(&[(ClientId(1), update)]);

        assert_eq!(
            client_adapter.world().get::<MockPos>(client_entity),
            Some(&MockPos(100))
        );
    }
}

use bevy_ecs::change_detection::Tick;
use bevy_ecs::prelude::{Entity, World};
use bimap::BiHashMap;
use std::collections::BTreeMap;

use aetheris_protocol::error::WorldError;
use aetheris_protocol::events::ComponentUpdate;
use aetheris_protocol::traits::WorldState;
use aetheris_protocol::types::{
    ClientId, ComponentKind, LocalId, NetworkId, NetworkIdAllocator, ShipClass, ShipStats,
};

use crate::Networked;
use crate::components::{
    LatestInput, PhysicsBody, RoomBoundsComponent, RoomDefinitionComponent,
    RoomMembershipComponent, ShipClassComponent, ShipStatsComponent, TransformComponent, Velocity,
};
use crate::physics_consts::{DEFAULT_DRAG, MASS_PER_ORE};
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
    tick_rate: u64,
}

impl Default for BevyWorldAdapter {
    fn default() -> Self {
        Self::new(World::new(), 60)
    }
}

impl BevyWorldAdapter {
    /// Creates a new adapter wrapping the given Bevy world.
    ///
    /// # Panics
    ///
    /// Panics if `tick_rate` is 0.
    pub fn new(world: World, tick_rate: u64) -> Self {
        assert!(tick_rate > 0, "tick_rate must be > 0");
        let mut adapter = Self {
            world,
            bimap: BiHashMap::new(),
            owners: std::collections::HashMap::new(),
            replicators: BTreeMap::new(),
            allocator: NetworkIdAllocator::new(1),
            last_extraction_tick: None,
            tick_rate,
        };
        adapter
            .world
            .insert_resource(crate::components::ServerTick(0));
        adapter
            .world
            .insert_resource(crate::components::ReliableEvents::default());
        adapter
            .world
            .insert_resource(crate::components::RoomIndex::default());
        adapter
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

    fn extract_deltas(&mut self) -> Vec<aetheris_protocol::events::ReplicationEvent> {
        let mut deltas = Vec::new();
        let current_tick = self.world.change_tick();
        // Bevy's change tick is internally a u32, but our protocol uses u64
        let tick = u64::from(current_tick.get());

        // Extract using RoomIndex? We only need to optimize the extraction logic when
        // deciding WHAT to send to WHICH client. BUT `extract_deltas` currently extracts
        // for ALL entities and then the transport broadcasts.
        // Wait, the delta payload is broadcasted globally. To implement per-client filtering
        // (Stage 4), the server's `tick.rs` will need to filter.
        // `extract_deltas` extracts all changed components. Then in `tick.rs` they are filtered?
        // Wait, `WorldState::extract_deltas()` returns `Vec<ReplicationEvent>`.
        // The `ReplicationEvent` has `network_id`. We can filter them before broadcasting.

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

    fn extract_reliable_events(
        &mut self,
    ) -> Vec<(Option<ClientId>, aetheris_protocol::events::WireEvent)> {
        let mut events = Vec::new();
        if let Some(mut reliable) = self
            .world
            .get_resource_mut::<crate::components::ReliableEvents>()
        {
            for (client_id, game_event) in reliable.queue.drain(..) {
                events.push((
                    client_id,
                    aetheris_protocol::events::WireEvent::GameEvent(game_event),
                ));
            }
        }
        events
    }

    fn apply_updates(&mut self, updates: &[(ClientId, ComponentUpdate)]) {
        let mut applied_count = 0u64;
        let mut unauthorized_count = 0u64;

        // Cache for the last used replicator to avoid BTreeMap lookups in batches
        let mut last_kind = None;
        let mut last_replicator = None;

        for (client_id, update) in updates {
            let Some(&entity) = self.bimap.get_by_left(&update.network_id) else {
                tracing::debug!(
                    client_id = client_id.0,
                    network_id = update.network_id.0,
                    kind = update.component_kind.0,
                    "[apply_updates] DROPPED: network_id not in bimap (entity unknown to server)"
                );
                continue;
            };

            // 1. Verify ownership (Hot path optimized via owners cache)
            let is_authorized = if let Some(&owner_id) = self.owners.get(&update.network_id) {
                let ok = owner_id == *client_id;
                tracing::debug!(
                    client_id = client_id.0,
                    network_id = update.network_id.0,
                    kind = update.component_kind.0,
                    owner_id = owner_id.0,
                    authorized = ok,
                    "[apply_updates] ownership check (cache hit)"
                );
                ok
            } else {
                // Fallback to slow ECS lookup for entities not in cache
                self.world
                    .get::<crate::components::NetworkOwner>(entity)
                    .is_some_and(|o| {
                        let ok = o.0 == *client_id;
                        tracing::debug!(
                            client_id = client_id.0,
                            network_id = update.network_id.0,
                            kind = update.component_kind.0,
                            owner_id = o.0.0,
                            authorized = ok,
                            "[apply_updates] ownership check (ECS fallback)"
                        );
                        self.owners.insert(update.network_id, o.0);
                        ok
                    })
            };

            if !is_authorized {
                unauthorized_count += 1;
                let cached_owner = self.owners.get(&update.network_id).copied();
                let ecs_owner = self
                    .world
                    .get::<crate::components::NetworkOwner>(entity)
                    .map(|o| o.0);
                tracing::error!(?client_id, network_id = ?update.network_id, ?cached_owner, ?ecs_owner, "Unauthorized update attempt");
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

        // Increment authoritative server tick resource
        if let Some(mut tick) = self
            .world
            .get_resource_mut::<crate::components::ServerTick>()
        {
            tick.0 = tick.0.saturating_add(1);
        }
    }

    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self))]
    fn simulate(&mut self) {
        #[allow(clippy::cast_precision_loss)]
        let dt = 1.0 / self.tick_rate as f32;

        // Stage 1: Auth Newtonian Physics + Input Application (M1015/M1020/M1038)
        let mut query = self.world.query::<(
            &mut Velocity,
            &mut TransformComponent,
            &PhysicsBody,
            Option<&LatestInput>,
            Option<&crate::components::CargoHold>,
            &crate::Networked, // M1013 inclusion for diagnostics
        )>();

        for (mut velocity, mut transform, physics, input, cargo, networked) in
            query.iter_mut(&mut self.world)
        {
            let network_id = networked.0;
            // 1.1 Calculate total mass with cargo penalty (M1038)
            let ore_count = cargo.map_or(0.0, |c| f32::from(c.ore_count));
            let total_mass = physics.base_mass + (ore_count * physics.mass_per_ore);

            // 1.2 Process Inputs
            let mut move_x = 0.0;
            let mut move_y = 0.0;

            if let Some(latest) = input {
                for action in &latest.command.actions {
                    if let aetheris_protocol::types::PlayerInputKind::Move { x, y } = action {
                        move_x = *x;
                        move_y = *y;
                    }
                }
            }

            // 1.3 Calculate acceleration
            let accel_x = move_x * (physics.thrust_force / total_mass);
            let accel_y = move_y * (physics.thrust_force / total_mass);

            if accel_x.abs() > f32::EPSILON || accel_y.abs() > f32::EPSILON {
                tracing::info!(
                    ?network_id,
                    move_x,
                    move_y,
                    accel_x,
                    accel_y,
                    "Applied Acceleration"
                );
            }

            // 1.4 Update velocity (Euler integration)
            velocity.dx += accel_x * dt;
            velocity.dy += accel_y * dt;

            // 1.5 Apply Drag (M1015 - prevents infinite sliding)
            // Hardening: Use stable semi-implicit drag model to prevent oscillation at high dt
            let drag_factor = 1.0 / (1.0 + physics.drag * dt);
            velocity.dx *= drag_factor;
            velocity.dy *= drag_factor;

            // 1.6 Clamp to Max Velocity
            let speed = (velocity.dx * velocity.dx + velocity.dy * velocity.dy).sqrt();
            if speed > physics.max_velocity && speed > f32::EPSILON {
                let ratio = physics.max_velocity / speed;
                velocity.dx *= ratio;
                velocity.dy *= ratio;
            }

            // 1.7 Update transform
            transform.0.x += velocity.dx * dt;
            transform.0.y += velocity.dy * dt;
        }

        // Room Bounds Enforcement (Stage 3 Simulate)
        let mut bounds_query = self
            .world
            .query::<(bevy_ecs::prelude::Entity, &RoomBoundsComponent)>();
        let mut rooms = Vec::new();
        for (e, bounds) in bounds_query.iter(&self.world) {
            if let Some(&nid) = self.bimap.get_by_right(&e) {
                rooms.push((nid, bounds.0));
            }
        }

        let mut avatar_query = self
            .world
            .query::<(&mut TransformComponent, &RoomMembershipComponent)>();
        for (mut transform, membership) in avatar_query.iter_mut(&mut self.world) {
            let room_id = membership.0.0;
            if let Some((_, bounds)) = rooms.iter().find(|(nid, _)| *nid == room_id) {
                let mut clamped = false;
                if transform.0.x < bounds.min_x {
                    transform.0.x = bounds.min_x;
                    clamped = true;
                }
                if transform.0.x > bounds.max_x {
                    transform.0.x = bounds.max_x;
                    clamped = true;
                }
                if transform.0.y < bounds.min_y {
                    transform.0.y = bounds.min_y;
                    clamped = true;
                }
                if transform.0.y > bounds.max_y {
                    transform.0.y = bounds.max_y;
                    clamped = true;
                }
                if clamped {
                    // Optional: zero velocity or handle bumping
                }
            }
        }

        // Stage 1.8: Process Targeted Actions (Mining)
        let mut input_query = self
            .world
            .query::<(Entity, &LatestInput, &mut crate::components::MiningBeam)>();
        for (_entity, latest, mut beam) in input_query.iter_mut(&mut self.world) {
            // Edge-detect: Only process actions if the client tick has changed.
            if beam.last_seen_input_tick != Some(latest.command.tick) {
                for action in &latest.command.actions {
                    if let aetheris_protocol::types::PlayerInputKind::ToggleMining { target } =
                        action
                    {
                        beam.active = !beam.active;
                        beam.target = Some(*target);
                        break;
                    }
                }
                beam.last_seen_input_tick = Some(latest.command.tick);
            }
        }

        // Stage 2: Gameplay Systems (M1038)
        let depleted = crate::mining::process_mining(&mut self.world, &self.bimap);
        for entity in depleted {
            if let Some(network_id) = self.bimap.get_by_right(&entity).copied() {
                let _ = self.despawn_networked(network_id);
            }
        }

        let to_respawn = crate::mining::process_respawn(&mut self.world);
        for (x, y, capacity) in to_respawn {
            let nid = self.spawn_kind(5, x, y, 0.0); // Kind 5 = Asteroid
            if let Some(entity) = self.bimap.get_by_left(&nid)
                && let Some(mut asteroid) =
                    self.world.get_mut::<crate::components::Asteroid>(*entity)
            {
                asteroid.total_capacity = capacity;
                asteroid.ore_remaining = capacity;
            }
        }

        // Stage 3: Enforce Z-clamp (M1015/M1020)
        // Void Rush is a 2D game on a 3D engine; z must stay 0.0.
        let mut z_query = self
            .world
            .query::<(&mut TransformComponent, &mut Velocity)>();
        for (mut transform, mut velocity) in z_query.iter_mut(&mut self.world) {
            if transform.0.z.abs() > f32::EPSILON {
                transform.0.z = 0.0;
            }
            if velocity.dz.abs() > f32::EPSILON {
                velocity.dz = 0.0;
            }
        }

        // In Phase 1, we advance the world tick to support Bevy's change detection.
        // Full system execution via Schedules will be implemented in M300.
        // NOTE: clear_trackers is intentionally NOT called here. It is called in
        // post_extract() (after extract_deltas) so that position/velocity changes
        // made during simulate() are still visible to the extraction pipeline.
    }

    fn post_extract(&mut self) {
        // Reset Bevy's change-detection tracking *after* extract_deltas() has consumed
        // all dirty bits.  If we cleared here during simulate(), the replication pipeline
        // would see every component as unchanged and send zero world-state updates.
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
        self.world.entity_mut(entity).insert((
            crate::components::NetworkOwner(client_id),
            TransformComponent(ProtocolTransform {
                x: 0.0,
                y: 0.0,
                z: 0.0, // Enforced Z-clamp
                rotation: 0.0,
                entity_type: 0, // Default/Unknown
            }),
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0, // Enforced Z-clamp
            },
        ));
        self.owners.insert(id, client_id);
        id
    }

    fn despawn_networked(&mut self, network_id: NetworkId) -> Result<(), WorldError> {
        if let Some(entity) = self.bimap.remove_by_left(&network_id).map(|(_, e)| e) {
            self.owners.remove(&network_id);
            if let Some(mut index) = self
                .world
                .get_resource_mut::<crate::components::RoomIndex>()
            {
                for memberships in index.memberships.values_mut() {
                    memberships.remove(&entity);
                }
            }
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
            let rot = if rotate {
                lcg_next(&mut seed) * std::f32::consts::PI / 20.0
            } else {
                0.0
            };
            let kind = entity_types[i as usize % entity_types.len()];
            self.spawn_kind(kind, x, y, rot);
        }
    }

    #[allow(clippy::too_many_lines, clippy::collapsible_if)]
    fn spawn_kind(&mut self, kind: u16, x: f32, y: f32, rot: f32) -> NetworkId {
        let network_id = self
            .allocator
            .allocate()
            .expect("NetworkId allocation failed (exhausted)");

        // We find the Playground_Master room id before spawning to avoid double borrow
        let mut master_nid = NetworkId(1); // Usually 1 if spawned first.
        {
            let mut query = self
                .world
                .query::<(bevy_ecs::prelude::Entity, &RoomDefinitionComponent)>();
            for (e, def) in query.iter(&self.world) {
                if def.0.name == "Playground_Master" {
                    if let Some(&nid) = self.bimap.get_by_right(&e) {
                        master_nid = nid;
                        break;
                    }
                }
            }
        }

        let mut entity_mut = self.world.spawn((
            crate::Networked(network_id),
            TransformComponent(ProtocolTransform {
                x,
                y,
                z: 0.0, // Enforced Z-clamp (M1015/M1020)
                rotation: rot,
                entity_type: kind,
            }),
            crate::components::Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0, // Enforced Z-clamp (M1015/M1020)
            },
        ));

        // Map entity kind to components (M1020 §3.2)
        match kind {
            // 1 = Player Interceptor, 2 = AI Interceptor (GDD §4.2)
            1 | 2 => {
                // Interceptor (GDD §4.2 / M1020 §3.1)
                entity_mut.insert((
                    ShipClassComponent(ShipClass::Interceptor),
                    ShipStatsComponent(ShipStats {
                        hp: 200,
                        max_hp: 200,
                        shield: 100,
                        max_shield: 100,
                        energy: 100,
                        max_energy: 100,
                        shield_regen_per_s: 5,
                        energy_regen_per_s: 10,
                    }),
                    PhysicsBody {
                        base_mass: 50.0,
                        thrust_force: 500.0, // Restored (prev: 125.0 during debugging)
                        max_velocity: 30.0,  // Reduced (prev: 60.0)
                        turn_rate: 270.0,
                        drag: DEFAULT_DRAG,
                        mass_per_ore: MASS_PER_ORE,
                    },
                    crate::components::CargoHold {
                        ore_count: 0,
                        capacity: 50,
                    },
                    crate::components::MiningBeam::default(),
                    crate::components::PlayerName {
                        name: "Player".to_string(),
                    },
                ));
            }
            3 => {
                // Dreadnought (GDD §4.2 / M1020 §3.1)
                entity_mut.insert((
                    ShipClassComponent(ShipClass::Dreadnought),
                    ShipStatsComponent(ShipStats {
                        hp: 1500,
                        max_hp: 1500,
                        shield: 500,
                        max_shield: 500,
                        energy: 300,
                        max_energy: 300,
                        shield_regen_per_s: 15,
                        energy_regen_per_s: 20,
                    }),
                    PhysicsBody {
                        base_mass: 300.0,
                        thrust_force: 100.0, // Reduced (prev: 200.0)
                        max_velocity: 20.0,  // Reduced (prev: 40.0)
                        turn_rate: 60.0,
                        drag: DEFAULT_DRAG,
                        mass_per_ore: MASS_PER_ORE,
                    },
                    crate::components::CargoHold {
                        ore_count: 0,
                        capacity: 100,
                    },
                    crate::components::MiningBeam::default(),
                    crate::components::PlayerName {
                        name: "Dreadnought".to_string(),
                    },
                ));
            }
            4 => {
                // Hauler (GDD §4.2 / M1020 §3.1)
                entity_mut.insert((
                    ShipClassComponent(ShipClass::Hauler),
                    ShipStatsComponent(ShipStats {
                        hp: 600,
                        max_hp: 600,
                        shield: 200,
                        max_shield: 200,
                        energy: 150,
                        max_energy: 150,
                        shield_regen_per_s: 8,
                        energy_regen_per_s: 12,
                    }),
                    PhysicsBody {
                        base_mass: 150.0,
                        thrust_force: 175.0, // Reduced (prev: 350.0)
                        max_velocity: 35.0,  // Reduced (prev: 70.0)
                        turn_rate: 150.0,
                        drag: DEFAULT_DRAG,
                        mass_per_ore: MASS_PER_ORE,
                    },
                    crate::components::CargoHold {
                        ore_count: 0,
                        capacity: 500,
                    },
                    crate::components::MiningBeam::default(),
                    crate::components::PlayerName {
                        name: "Hauler".to_string(),
                    },
                ));
            }
            5 => {
                // Mining Asteroid (Kind 5 from renderer/UI)
                entity_mut.insert((
                    crate::components::Asteroid {
                        ore_remaining: 1000,
                        total_capacity: 1000,
                    },
                    crate::components::AsteroidHP {
                        hp: 500,
                        max_hp: 500,
                    },
                ));
            }
            _ => {}
        }

        entity_mut.insert(RoomMembershipComponent(
            aetheris_protocol::types::RoomMembership(master_nid),
        ));
        let entity = entity_mut.id();

        if let Some(mut index) = self
            .world
            .get_resource_mut::<crate::components::RoomIndex>()
        {
            index
                .memberships
                .entry(master_nid)
                .or_default()
                .insert(entity);
        }

        self.bimap.insert(network_id, entity);
        network_id
    }

    fn spawn_kind_for(
        &mut self,
        kind: u16,
        x: f32,
        y: f32,
        rot: f32,
        client_id: ClientId,
    ) -> NetworkId {
        let nid = self.spawn_kind(kind, x, y, rot);
        if let Some(&entity) = self.bimap.get_by_left(&nid) {
            self.world
                .entity_mut(entity)
                .insert(crate::components::NetworkOwner(client_id));
            self.owners.insert(nid, client_id);
        }
        tracing::debug!(
            network_id = nid.0,
            kind,
            client_id = client_id.0,
            session_ship = false,
            "[spawn_kind_for] playground entity spawned (NO SessionShip)"
        );
        nid
    }

    fn spawn_session_ship(
        &mut self,
        kind: u16,
        x: f32,
        y: f32,
        rot: f32,
        client_id: ClientId,
    ) -> NetworkId {
        let nid = self.spawn_kind_for(kind, x, y, rot, client_id);
        if let Some(&entity) = self.bimap.get_by_left(&nid) {
            self.world
                .entity_mut(entity)
                .insert(crate::components::SessionShip);
        }
        tracing::info!(
            network_id = nid.0,
            kind,
            client_id = client_id.0,
            session_ship = true,
            "[spawn_session_ship] session ship spawned (SessionShip marker attached)"
        );
        nid
    }

    fn setup_world(&mut self) {
        let room_nid = self.spawn_networked();
        if let Some(&entity) = self.bimap.get_by_left(&room_nid) {
            self.world.entity_mut(entity).insert((
                RoomDefinitionComponent(aetheris_protocol::types::RoomDefinition {
                    name: "Playground_Master".to_string(),
                    capacity: 0, // unlimited
                    access: aetheris_protocol::types::RoomAccessPolicy::Open,
                    is_template: false,
                }),
                RoomBoundsComponent(aetheris_protocol::types::RoomBounds {
                    min_x: -250.0,
                    min_y: -250.0,
                    max_x: 250.0,
                    max_y: 250.0,
                }),
                // The room belongs to itself (or no membership needed for the room itself)
                RoomMembershipComponent(aetheris_protocol::types::RoomMembership(room_nid)),
            ));
        }
    }

    /// Queues a reliable game event for a specific client (or all clients if None).
    fn queue_reliable_event(
        &mut self,
        client_id: Option<aetheris_protocol::types::ClientId>,
        event: aetheris_protocol::events::GameEvent,
    ) {
        if let Some(mut reliable) = self
            .world
            .get_resource_mut::<crate::components::ReliableEvents>()
        {
            reliable.queue.push((client_id, event));
        }
    }

    fn clear_world(&mut self) {
        tracing::info!("Clearing all networked entities from the world");
        let ids: Vec<_> = self.bimap.iter().map(|(&id, _)| id).collect();
        for id in ids {
            let _ = self.despawn_networked(id);
        }

        // VS-02 — Also clear pending respawn trackers to prevent "ghost" reappearances
        let mut respawn_query = self
            .world
            .query_filtered::<Entity, bevy_ecs::prelude::With<crate::components::AsteroidRespawn>>(
            );
        let to_despawn: Vec<_> = respawn_query.iter(&self.world).collect();
        for entity in to_despawn {
            self.world.despawn(entity);
        }

        self.allocator.reset();

        // Recreate the Master Room after a clear
        self.setup_world();
    }

    fn get_entity_room(&self, network_id: NetworkId) -> Option<NetworkId> {
        let entity = self.bimap.get_by_left(&network_id)?;
        let membership = self.world.get::<RoomMembershipComponent>(*entity)?;
        Some(membership.0.0)
    }

    #[allow(clippy::collapsible_if)]
    fn get_client_room(&self, client_id: ClientId) -> Option<NetworkId> {
        // Find the session ship for this client by iterating bimap
        let mut session_entity = None;
        for (_, &entity) in &self.bimap {
            if let Some(owner) = self.world.get::<crate::components::NetworkOwner>(entity) {
                if owner.0 == client_id
                    && self
                        .world
                        .get::<crate::components::SessionShip>(entity)
                        .is_some()
                {
                    session_entity = Some(entity);
                    break;
                }
            }
        }
        let e = session_entity?;
        let membership = self.world.get::<RoomMembershipComponent>(e)?;
        Some(membership.0.0)
    }
}

// Local Transform removed. Use components::TransformComponent instead.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DefaultReplicator;
    use bevy_ecs::component::Component;
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
    #[allow(clippy::float_cmp)]
    fn test_input_replicator_anti_replay() {
        let mut adapter = BevyWorldAdapter::default();
        let kind = ComponentKind(200);
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

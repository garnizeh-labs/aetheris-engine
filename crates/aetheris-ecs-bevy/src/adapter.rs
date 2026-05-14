use bevy_ecs::change_detection::Tick;
use bevy_ecs::prelude::{Entity, World};
use bimap::BiHashMap;
use std::collections::BTreeMap;

use aetheris_protocol::error::WorldError;
use aetheris_protocol::events::ComponentUpdate;
use aetheris_protocol::traits::WorldState;
use aetheris_protocol::types::{
    AgentKind, AgentProperties, ClientId, ComponentKind, ENTITY_TYPE_AGENT, ENTITY_TYPE_AI_AGENT,
    ENTITY_TYPE_BEAM, ENTITY_TYPE_CARRIER_AGENT, ENTITY_TYPE_DATA_DROP, ENTITY_TYPE_HEAVY_AGENT,
    ENTITY_TYPE_RESOURCE, ENTITY_TYPE_TRAINING_TARGET, LocalId, NetworkId, NetworkIdAllocator,
    get_default_properties,
};

use crate::Networked;
use crate::components::{
    AgentKindComponent, AgentPropertiesComponent, AiControlled, IntegrityPoolComponent,
    LatestInput, PhysicsBody, PriorityPoolComponent, ToolComponent, TrainingTarget,
    TransformComponent, Velocity, WorkspaceBoundsComponent, WorkspaceDefinitionComponent,
    WorkspaceMembershipComponent,
};
use crate::physics_consts::MASS_PER_PAYLOAD;
use crate::registry::BoxedReplicator;
use aetheris_protocol::types::Transform as ProtocolTransform;

type PostSimulateHook = Box<dyn Fn(&mut World) + Send + Sync>;

/// Adapts a Bevy ECS World to the `WorldState` trait.
pub struct BevyWorldAdapter {
    world: World,
    bimap: BiHashMap<NetworkId, Entity>,
    owners: std::collections::HashMap<NetworkId, ClientId>,
    replicators: BTreeMap<ComponentKind, BoxedReplicator>,
    allocator: aetheris_protocol::types::NetworkIdAllocator,
    last_extraction_tick: Option<Tick>,
    tick_rate: u64,
    rng: crate::deterministic_rng::DeterministicRng,
    post_simulate_hook: Option<PostSimulateHook>,
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
            rng: crate::deterministic_rng::DeterministicRng::default(),
            post_simulate_hook: None,
        };
        adapter
            .world
            .insert_resource(crate::components::ServerTick(0));
        adapter
            .world
            .insert_resource(crate::components::ReliableEvents::default());
        adapter
            .world
            .insert_resource(crate::interaction::BeamSpawnRequests::default());
        adapter
            .world
            .insert_resource(crate::components::WorkspaceIndex::default());
        adapter.world.insert_resource(adapter.rng.clone());
        adapter
    }

    /// Creates a new adapter with a custom deterministic RNG.
    pub fn new_with_rng(
        world: World,
        tick_rate: u64,
        rng: crate::deterministic_rng::DeterministicRng,
    ) -> Self {
        let mut adapter = Self {
            world,
            bimap: BiHashMap::new(),
            owners: std::collections::HashMap::new(),
            replicators: BTreeMap::new(),
            allocator: NetworkIdAllocator::new(1),
            last_extraction_tick: None,
            tick_rate,
            rng,
            post_simulate_hook: None,
        };
        adapter
            .world
            .insert_resource(crate::components::ServerTick(0));
        adapter
            .world
            .insert_resource(crate::components::ReliableEvents::default());
        adapter
            .world
            .insert_resource(crate::components::WorkspaceIndex::default());
        adapter.world.insert_resource(adapter.rng.clone());
        adapter
    }

    /// Registers a component replicator for a specific `ComponentKind`.
    pub fn register_replicator(&mut self, replicator: BoxedReplicator) {
        self.replicators.insert(replicator.kind(), replicator);
    }

    /// Sets a hook to be run after the simulation step.
    pub fn set_post_simulate_hook(&mut self, hook: impl Fn(&mut World) + Send + Sync + 'static) {
        self.post_simulate_hook = Some(Box::new(hook));
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
        // M1038: Use authoritative ServerTick resource instead of Bevy's internal change_tick.
        // This ensures the tick window in InputCommandReplicator matches outgoing component ticks.
        let tick = self
            .world
            .get_resource::<crate::components::ServerTick>()
            .map_or(0, |t| t.0);
        let current_tick = self.world.change_tick();

        // Extract using WorkspaceIndex? We only need to optimize the extraction logic when
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
            events.append(&mut reliable.queue);
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
        let server_tick = self
            .world
            .get_resource::<crate::components::ServerTick>()
            .map_or(0, |t| t.0);
        let log_this_tick = server_tick.is_multiple_of(600);

        if log_this_tick && server_tick > 0 {
            let total = self.world.entities().len();
            let mut entities_info = Vec::new();
            let mut query = self.world.query::<Entity>();
            for entity in query.iter(&self.world) {
                let mut components = Vec::new();
                if self.world.get::<TransformComponent>(entity).is_some() {
                    components.push("Transform");
                }
                if self
                    .world
                    .get::<crate::components::BeamMarker>(entity)
                    .is_some()
                {
                    components.push("Beam");
                }
                if self
                    .world
                    .get::<crate::components::IntegrityPoolComponent>(entity)
                    .is_some()
                {
                    components.push("Integrity");
                }
                if self
                    .world
                    .get::<crate::components::Velocity>(entity)
                    .is_some()
                {
                    components.push("Velocity");
                }
                if self.world.get::<crate::Networked>(entity).is_some() {
                    components.push("Networked");
                }

                let nid = self.bimap.get_by_right(&entity).copied();
                entities_info.push(format!("{entity:?}(nid:{nid:?}) -> {components:?}"));
            }
            tracing::debug!(
                server_tick,
                total,
                ?entities_info,
                "Detailed World Snapshot"
            );
        }

        let mut query = self.world.query::<(
            &mut Velocity,
            &mut TransformComponent,
            &PhysicsBody,
            Option<&LatestInput>,
            Option<&crate::components::DataStore>,
            &crate::Networked,
        )>();

        for (mut velocity, mut transform, physics, input, cargo, networked) in
            query.iter_mut(&mut self.world)
        {
            let network_id = networked.0;
            // 1.1 Calculate total mass with payload penalty
            let payload_count = cargo.map_or(0.0, |c| f32::from(c.payload_count));
            let total_mass = physics.base_mass + (payload_count * physics.mass_per_payload);

            // 1.2 Process Inputs
            let mut move_x = 0.0f32;
            let mut move_y = 0.0f32;

            if let Some(latest) = input {
                for action in &latest.command.actions {
                    if let aetheris_protocol::types::PlayerInputKind::Move { x, y } = *action {
                        move_x = x;
                        move_y = y;
                    }
                }

                if move_x.abs() > 0.001 || move_y.abs() > 0.001 {
                    tracing::info!(
                        ?network_id,
                        move_x,
                        move_y,
                        tick = latest.command.tick,
                        "[Server Simulate] Received Input"
                    );
                }
            }

            // 1.3 Calculate acceleration
            let accel_x = move_x * (physics.thrust_force / total_mass);
            let accel_y = move_y * (physics.thrust_force / total_mass);

            if accel_x.abs() > 0.001f32 || accel_y.abs() > 0.001f32 {
                tracing::info!(
                    ?network_id,
                    move_x,
                    move_y,
                    accel_x,
                    accel_y,
                    vel_x = velocity.dx,
                    vel_y = velocity.dy,
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

            // 1.8 Update rotation (M1020 - rotate towards velocity)
            let speed_sq = velocity.dx * velocity.dx + velocity.dy * velocity.dy;
            if speed_sq > 0.01 {
                let target_rotation = velocity.dy.atan2(velocity.dx);
                let current_rotation = transform.0.rotation;

                // Calculate shortest angular distance
                let mut delta = target_rotation - current_rotation;
                while delta > std::f32::consts::PI {
                    delta -= std::f32::consts::TAU;
                }
                while delta < -std::f32::consts::PI {
                    delta += std::f32::consts::TAU;
                }

                let turn_speed = physics.turn_rate.to_radians(); // turn_rate is in deg/s
                let max_turn = turn_speed * dt;

                if delta.abs() < max_turn {
                    transform.0.rotation = target_rotation;
                } else {
                    transform.0.rotation += delta.signum() * max_turn;
                }
            }

            // Diagnostic: Log movement (Tick-sampled gate — M1020)
            if log_this_tick && speed_sq > 0.001 {
                tracing::debug!(
                    ?network_id,
                    x = transform.0.x,
                    y = transform.0.y,
                    "Authoritative Position Update"
                );
            }
        }

        // Workspace Bounds Enforcement (Stage 3 Simulate)
        let mut bounds_query = self
            .world
            .query::<(bevy_ecs::prelude::Entity, &WorkspaceBoundsComponent)>();
        let mut workspaces = Vec::new();
        for (e, bounds) in bounds_query.iter(&self.world) {
            if let Some(&nid) = self.bimap.get_by_right(&e) {
                workspaces.push((nid, bounds.0));
            }
        }

        let mut agent_query = self
            .world
            .query::<(&mut TransformComponent, &WorkspaceMembershipComponent)>();
        for (mut transform, membership) in agent_query.iter_mut(&mut self.world) {
            let workspace_id = membership.0.0;
            if let Some((_, bounds)) = workspaces.iter().find(|(nid, _)| *nid == workspace_id) {
                // Toroidal wrapping
                let width = bounds.max_x - bounds.min_x;
                let height = bounds.max_y - bounds.min_y;

                if width > 0.0 {
                    transform.0.x =
                        ((transform.0.x - bounds.min_x).rem_euclid(width)) + bounds.min_x;
                }
                if height > 0.0 {
                    transform.0.y =
                        ((transform.0.y - bounds.min_y).rem_euclid(height)) + bounds.min_y;
                }
            }
        }

        // Stage 1.8: Process Targeted Actions (Extraction)
        let mut input_query =
            self.world
                .query::<(Entity, &LatestInput, &mut crate::components::ExtractionBeam)>();
        for (_entity, latest, mut beam) in input_query.iter_mut(&mut self.world) {
            // Edge-detect: Only process actions if the client tick has changed.
            if beam.last_seen_input_tick != Some(latest.command.tick) {
                for action in &latest.command.actions {
                    if let aetheris_protocol::types::PlayerInputKind::ToggleExtraction { target } =
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

        // Stage 2: Gameplay Systems
        let exhausted = crate::extraction::process_extraction(&mut self.world, &self.bimap);
        let mut reliable_events = Vec::new();
        for entity in exhausted {
            if let Some(network_id) = self.bimap.get_by_right(&entity).copied() {
                reliable_events.push((
                    None,
                    aetheris_protocol::events::WireEvent::PlatformEvent(
                        aetheris_protocol::events::PlatformEvent::ResourceExhausted { network_id },
                    ),
                ));
                let _ = self.despawn_networked(network_id);
            }
        }

        // Interaction Loop
        let _ = crate::interaction::process_interaction(&mut self.world, &self.bimap);

        let collected = crate::extraction::process_payload_collection(&mut self.world, &self.bimap);
        for entity in collected {
            if let Some(network_id) = self.bimap.get_by_right(&entity).copied() {
                reliable_events.push((
                    None,
                    aetheris_protocol::events::WireEvent::PlatformEvent(
                        aetheris_protocol::events::PlatformEvent::PayloadCollected {
                            network_id,
                            amount: 0,
                        },
                    ),
                ));
                let _ = self.despawn_networked(network_id);
            }
        }

        let (b_to_despawn, b_terminations) =
            crate::interaction::process_beams(&mut self.world, &self.bimap);

        // Handle terminations
        for entity in b_terminations {
            if let Some(network_id) = self.bimap.get_by_right(&entity).copied() {
                reliable_events.push((
                    None,
                    aetheris_protocol::events::WireEvent::PlatformEvent(
                        aetheris_protocol::events::PlatformEvent::Termination {
                            target: network_id,
                        },
                    ),
                ));
                // Drop payload
                if let Some(store) = self.world.get::<crate::components::DataStore>(entity) {
                    let amount = store.payload_count;
                    if amount > 0 {
                        let pos = self.world.get::<TransformComponent>(entity).map_or(
                            ProtocolTransform {
                                x: 0.0,
                                y: 0.0,
                                z: 0.0,
                                rotation: 0.0,
                                entity_type: 0,
                            },
                            |t| t.0,
                        );
                        let drop_id = self.spawn_kind(ENTITY_TYPE_DATA_DROP, pos.x, pos.y, 0.0);
                        if let Some(drop_entity) = self.bimap.get_by_left(&drop_id)
                            && let Some(mut drop_comp) =
                                self.world
                                    .get_mut::<crate::components::DataDropComponent>(*drop_entity)
                        {
                            drop_comp.0.amount = amount;
                        }
                    }
                }

                // If training target, spawn reinitialization tracker
                if self
                    .world
                    .get::<crate::components::TrainingTarget>(entity)
                    .is_some()
                {
                    self.world.spawn((
                        crate::components::TrainingTarget,
                        crate::components::RespawnTimer {
                            delay_ticks: 600,
                            location: aetheris_protocol::types::RespawnLocation::Coordinate(
                                50.0, 50.0,
                            ),
                        },
                    ));
                }

                let _ = self.despawn_networked(network_id);
            }
        }

        for entity in &b_to_despawn {
            if let Some(network_id) = self.bimap.get_by_right(entity).copied() {
                tracing::info!(?network_id, "Despawning beam (range/hit)");
                reliable_events.push((
                    None,
                    aetheris_protocol::events::WireEvent::PlatformEvent(
                        aetheris_protocol::events::PlatformEvent::Termination {
                            target: network_id,
                        },
                    ),
                ));
                let _ = self.despawn_networked(network_id);
            } else if let Ok(e_mut) = self.world.get_entity_mut(*entity) {
                tracing::info!(?entity, "Despawning beam (local-only)");
                e_mut.despawn();
            }
        }

        if !reliable_events.is_empty()
            && let Some(mut reliable) = self
                .world
                .get_resource_mut::<crate::components::ReliableEvents>()
        {
            reliable.queue.extend(reliable_events);
        }

        // Handle Beam Spawn Requests
        let mut spawns = Vec::new();
        if let Some(mut requests) = self
            .world
            .get_resource_mut::<crate::interaction::BeamSpawnRequests>()
        {
            spawns = std::mem::take(&mut requests.0);
        }
        for spawn in spawns {
            let rot = spawn.vel[1].atan2(spawn.vel[0]);
            let bid = self.spawn_kind(ENTITY_TYPE_BEAM, spawn.pos[0], spawn.pos[1], rot);
            if let Some(b_entity) = self.bimap.get_by_left(&bid) {
                if let Some(mut marker) = self
                    .world
                    .get_mut::<crate::components::BeamMarker>(*b_entity)
                {
                    marker.owner = spawn.owner;
                }
                if let Some(mut vel) = self.world.get_mut::<crate::components::Velocity>(*b_entity)
                {
                    vel.dx = spawn.vel[0];
                    vel.dy = spawn.vel[1];
                }
            }
        }

        crate::interaction::process_regen(&mut self.world);

        let to_reinit = crate::interaction::process_target_reinitialization(&mut self.world);
        for (x, y) in to_reinit {
            self.spawn_kind(ENTITY_TYPE_TRAINING_TARGET, x, y, 0.0);
        }

        let to_respawn = crate::extraction::process_respawn(&mut self.world);
        for (x, y, capacity) in to_respawn {
            let nid = self.spawn_kind(ENTITY_TYPE_RESOURCE, x, y, 0.0);
            if let Some(entity) = self.bimap.get_by_left(&nid)
                && let Some(mut resource) =
                    self.world.get_mut::<crate::components::Resource>(*entity)
            {
                resource.total_capacity = capacity;
                resource.payload_remaining = capacity;
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

        // Run post-simulation hook if present
        if let Some(hook) = self.post_simulate_hook.as_ref() {
            hook(&mut self.world);
        }
    }

    fn post_extract(&mut self) {
        // Reset Bevy's change-detection tracking *after* extract_deltas() has consumed
        // all dirty bits.  If we cleared here during simulate(), the replication pipeline
        // would see every component as unchanged and send zero world-state updates.
        self.world.clear_trackers();
    }

    fn state_hash(&self) -> u64 {
        use crate::components::{AgentPropertiesComponent, TransformComponent};
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();

        // 1. Get all networked entities and sort by NetworkId for determinism
        let mut nids: Vec<_> = self.bimap.iter().map(|(nid, _)| *nid).collect();
        nids.sort();

        for nid in nids {
            nid.hash(&mut hasher);
            if let Some(lid) = self.get_local_id(nid) {
                let entity = bevy_ecs::prelude::Entity::from_bits(lid.0);

                // Hash Transform
                if let Some(t) = self.world.get::<TransformComponent>(entity) {
                    t.0.x.to_bits().hash(&mut hasher);
                    t.0.y.to_bits().hash(&mut hasher);
                    t.0.z.to_bits().hash(&mut hasher);
                    t.0.rotation.to_bits().hash(&mut hasher);
                    t.0.entity_type.hash(&mut hasher);
                }

                // Hash AgentProperties
                if let Some(p) = self.world.get::<AgentPropertiesComponent>(entity) {
                    p.0.integrity.hash(&mut hasher);
                    p.0.priority.hash(&mut hasher);
                    p.0.energy.hash(&mut hasher);
                }
            }
        }

        hasher.finish()
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
            let is_session_agent = self
                .world
                .get::<crate::components::SessionAgent>(entity)
                .is_some();
            let owner_id = self
                .world
                .get::<crate::components::NetworkOwner>(entity)
                .map(|o| o.0);

            if let Some(mut index) = self
                .world
                .get_resource_mut::<crate::components::WorkspaceIndex>()
            {
                for memberships in index.memberships.values_mut() {
                    memberships.remove(&entity);
                }
                // If it was a session agent, remove the client's workspace assignment
                if is_session_agent && let Some(cid) = owner_id {
                    index.client_workspaces.remove(&cid);
                }
            }
            if let Ok(entity_mut) = self.world.get_entity_mut(entity) {
                entity_mut.despawn();
                #[allow(clippy::cast_precision_loss)]
                metrics::gauge!("aetheris_ecs_entities_networked").set(self.bimap.len() as f64);

                // Queue reliable despawn event for replication
                if let Some(mut reliable) = self
                    .world
                    .get_resource_mut::<crate::components::ReliableEvents>()
                {
                    tracing::info!(?network_id, "Queuing EntityDespawned event for replication");
                    reliable.queue.push((
                        None, // Broadcast to all
                        aetheris_protocol::events::WireEvent::EntityDespawned { network_id },
                    ));
                }

                Ok(())
            } else {
                Err(WorldError::EntityNotFound(network_id))
            }
        } else {
            Err(WorldError::EntityNotFound(network_id))
        }
    }

    fn stress_test(&mut self, count: u16, rotate: bool) {
        use rand::RngExt;
        tracing::info!(count, rotate, "Executing server-side stress test");

        let entity_types: [u16; 5] = [
            ENTITY_TYPE_AGENT,
            ENTITY_TYPE_HEAVY_AGENT,
            ENTITY_TYPE_CARRIER_AGENT,
            ENTITY_TYPE_RESOURCE,
            ENTITY_TYPE_DATA_DROP,
        ];
        for i in 0..count {
            let x = self.rng.inner_mut().random_range(-20.0..20.0);
            let y = self.rng.inner_mut().random_range(-20.0..20.0);
            let rot = if rotate {
                self.rng
                    .inner_mut()
                    .random_range(0.0..std::f32::consts::TAU)
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

        // We find the Playground_Master workspace id before spawning to avoid double borrow
        let mut master_nid = NetworkId(1); // Usually 1 if spawned first.
        let mut found_master = false;
        {
            let mut query = self
                .world
                .query::<(bevy_ecs::prelude::Entity, &WorkspaceDefinitionComponent)>();
            for (e, def) in query.iter(&self.world) {
                if def.0.name.as_str() == "Playground_Master" {
                    if let Some(&nid) = self.bimap.get_by_right(&e) {
                        master_nid = nid;
                        found_master = true;
                        break;
                    }
                }
            }
        }
        // M10156 — Lazy workspace creation: if the master workspace doesn't exist yet, create it.
        // This ensures the world can start with 0 entities but still function when needed.
        if !found_master {
            let workspace_nid = self.spawn_networked();
            if let Some(&entity) = self.bimap.get_by_left(&workspace_nid) {
                self.world.entity_mut(entity).insert((
                    WorkspaceDefinitionComponent(aetheris_protocol::types::WorkspaceDefinition {
                        name: aetheris_protocol::types::WorkspaceName::new("Playground_Master")
                            .expect("static workspace name fits within MAX_WORKSPACE_STRING_BYTES"),
                        capacity: 0, // unlimited
                        access: aetheris_protocol::types::WorkspaceAccessPolicy::Open,
                        is_template: false,
                    }),
                    WorkspaceBoundsComponent(aetheris_protocol::types::WorkspaceBounds {
                        min_x: -250.0,
                        min_y: -250.0,
                        max_x: 250.0,
                        max_y: 250.0,
                    }),
                    WorkspaceMembershipComponent(aetheris_protocol::types::WorkspaceMembership(
                        workspace_nid,
                    )),
                ));
                master_nid = workspace_nid;
                tracing::info!(?master_nid, "Playground_Master workspace created lazily");

                // VS-02 refinement: spawn a single authoritative resource at (30, 0)
                // when the master workspace is first created.
                self.spawn_kind(ENTITY_TYPE_RESOURCE, 30.0, 0.0, 0.0);
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
            ENTITY_TYPE_AGENT | ENTITY_TYPE_AI_AGENT => {
                // Agent (GDD §4.2 / M1020 §3.1)
                let (hp, priority) = get_default_properties(kind);
                entity_mut.insert((
                    AgentKindComponent(AgentKind::Standard),
                    AgentPropertiesComponent(AgentProperties {
                        integrity: hp,
                        max_integrity: hp,
                        priority,
                        max_priority: priority,
                        energy: 100,
                        max_energy: 100,
                        priority_regen_per_s: 5,
                        energy_regen_per_s: 10,
                    }),
                    PhysicsBody {
                        base_mass: 100.0,
                        thrust_force: crate::physics_consts::DEFAULT_THRUST_FORCE,
                        max_velocity: crate::physics_consts::DEFAULT_MAX_VELOCITY,
                        turn_rate: 270.0,
                        drag: crate::physics_consts::DEFAULT_DRAG,
                        mass_per_payload: crate::physics_consts::MASS_PER_PAYLOAD,
                    },
                    crate::components::DataStore {
                        payload_count: 0,
                        capacity: 500,
                    },
                    crate::components::ExtractionBeam {
                        active: false,
                        target: None,
                        extraction_range: 15.0,
                        base_extraction_rate: 2, // Slower pacing
                        last_seen_input_tick: None,
                    },
                    crate::components::PlayerName {
                        name: if kind == ENTITY_TYPE_AI_AGENT {
                            "AI Agent"
                        } else {
                            "Player"
                        }
                        .to_string(),
                    },
                    ToolComponent(aetheris_protocol::types::Tool {
                        cooldown_ticks: 30, // 0.5s
                        last_fired_tick: 0,
                    }),
                    PriorityPoolComponent(aetheris_protocol::types::PriorityPool {
                        current: priority,
                        max: priority,
                    }),
                    IntegrityPoolComponent(aetheris_protocol::types::IntegrityPool {
                        current: hp,
                        max: hp,
                    }),
                    crate::components::PriorityRegenTimer {
                        ticks_until_regen: 0,
                    },
                ));

                if kind == ENTITY_TYPE_AI_AGENT {
                    entity_mut.insert(AiControlled);
                }
            }
            ENTITY_TYPE_BEAM => {
                // Beam (VS-03 refinement)
                entity_mut.insert((
                    crate::components::BeamMarker {
                        beam_type: aetheris_protocol::types::InteractionBeamType::PulseBeam,
                        spawn_pos: [x, y],
                        max_range: 200.0,    // VS-03: range aligned with auto-aim
                        owner: NetworkId(0), // Default, set later if needed
                        lifetime_ticks: 300,
                    },
                    Velocity {
                        dx: 0.0,
                        dy: 0.0,
                        dz: 0.0,
                    },
                    PhysicsBody {
                        base_mass: 1.0,
                        thrust_force: 0.0,
                        max_velocity: 1000.0, // High speed
                        turn_rate: 0.0,
                        drag: 0.0, // No drag for beams
                        mass_per_payload: 0.0,
                    },
                ));
            }
            ENTITY_TYPE_DATA_DROP => {
                // Data Drop
                entity_mut.insert((
                    crate::components::DataDropComponent(aetheris_protocol::types::DataDrop {
                        amount: 0,
                    }),
                    PhysicsBody {
                        base_mass: 10.0,
                        thrust_force: 0.0,
                        max_velocity: 10.0,
                        turn_rate: 0.0,
                        drag: 2.0,
                        mass_per_payload: 0.0,
                    },
                ));
            }
            ENTITY_TYPE_TRAINING_TARGET => {
                // Training Target (VS-03)
                let (hp, priority) = get_default_properties(kind);
                entity_mut.insert((
                    TrainingTarget,
                    AgentKindComponent(AgentKind::Carrier),
                    PriorityPoolComponent(aetheris_protocol::types::PriorityPool {
                        current: priority,
                        max: priority,
                    }),
                    IntegrityPoolComponent(aetheris_protocol::types::IntegrityPool {
                        current: hp,
                        max: hp,
                    }),
                    crate::components::PlayerName {
                        name: "Training Target".to_string(),
                    },
                    crate::components::PriorityRegenTimer {
                        ticks_until_regen: 0,
                    },
                    PhysicsBody {
                        base_mass: 200.0,
                        thrust_force: 0.0,
                        max_velocity: 0.0,
                        turn_rate: 0.0,
                        drag: 1.0,
                        mass_per_payload: 0.0,
                    },
                ));
            }
            ENTITY_TYPE_HEAVY_AGENT => {
                // Heavy Agent (GDD §4.2 / M1020 §3.1)
                let (hp, priority) = get_default_properties(kind);
                entity_mut.insert((
                    AgentKindComponent(AgentKind::Heavy),
                    AgentPropertiesComponent(AgentProperties {
                        integrity: hp,
                        max_integrity: hp,
                        priority,
                        max_priority: priority,
                        energy: 300,
                        max_energy: 300,
                        priority_regen_per_s: 15,
                        energy_regen_per_s: 20,
                    }),
                    PhysicsBody {
                        base_mass: 400.0,
                        thrust_force: 40000.0,
                        max_velocity: 60.0,
                        turn_rate: 60.0,
                        drag: crate::physics_consts::DEFAULT_DRAG,
                        mass_per_payload: MASS_PER_PAYLOAD,
                    },
                    crate::components::DataStore {
                        payload_count: 0,
                        capacity: 500,
                    },
                    crate::components::ExtractionBeam::default(),
                    crate::components::PlayerName {
                        name: "Heavy Agent".to_string(),
                    },
                    PriorityPoolComponent(aetheris_protocol::types::PriorityPool {
                        current: priority,
                        max: priority,
                    }),
                    IntegrityPoolComponent(aetheris_protocol::types::IntegrityPool {
                        current: hp,
                        max: hp,
                    }),
                    crate::components::PriorityRegenTimer {
                        ticks_until_regen: 0,
                    },
                ));
            }
            ENTITY_TYPE_CARRIER_AGENT => {
                // Carrier (GDD §4.2 / M1020 §3.1)
                let (hp, priority) = get_default_properties(kind);
                entity_mut.insert((
                    AgentKindComponent(AgentKind::Carrier),
                    AgentPropertiesComponent(AgentProperties {
                        integrity: hp,
                        max_integrity: hp,
                        priority,
                        max_priority: priority,
                        energy: 150,
                        max_energy: 150,
                        priority_regen_per_s: 8,
                        energy_regen_per_s: 12,
                    }),
                    PhysicsBody {
                        base_mass: 200.0,
                        thrust_force: 25000.0,
                        max_velocity: 80.0,
                        turn_rate: 150.0,
                        drag: crate::physics_consts::DEFAULT_DRAG,
                        mass_per_payload: MASS_PER_PAYLOAD,
                    },
                    crate::components::DataStore {
                        payload_count: 0,
                        capacity: 500,
                    },
                    crate::components::ExtractionBeam::default(),
                    crate::components::PlayerName {
                        name: "Carrier Agent".to_string(),
                    },
                    PriorityPoolComponent(aetheris_protocol::types::PriorityPool {
                        current: priority,
                        max: priority,
                    }),
                    IntegrityPoolComponent(aetheris_protocol::types::IntegrityPool {
                        current: hp,
                        max: hp,
                    }),
                    crate::components::PriorityRegenTimer {
                        ticks_until_regen: 0,
                    },
                ));
            }
            ENTITY_TYPE_RESOURCE => {
                // Resource (Kind 5 from renderer/UI)
                entity_mut.insert((
                    crate::components::Resource {
                        payload_remaining: 100,
                        total_capacity: 100,
                    },
                    crate::components::ResourceIntegrity {
                        integrity: 500,
                        max_integrity: 500,
                    },
                ));
            }
            _ => {}
        }
        entity_mut.insert(WorkspaceMembershipComponent(
            aetheris_protocol::types::WorkspaceMembership(master_nid),
        ));
        let entity = entity_mut.id();

        if let Some(mut index) = self
            .world
            .get_resource_mut::<crate::components::WorkspaceIndex>()
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
            session_agent = false,
            "[spawn_kind_for] playground entity spawned (NO SessionAgent)"
        );
        nid
    }

    fn spawn_session_agent(
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
                .insert(crate::components::SessionAgent);

            // VS-06 — Register client workspace assignment for fast lookup
            let workspace_id = self
                .get_entity_workspace(nid)
                .expect("Session agent missing workspace");
            if let Some(mut index) = self
                .world
                .get_resource_mut::<crate::components::WorkspaceIndex>()
            {
                index.client_workspaces.insert(client_id, workspace_id);
            }
        }
        tracing::info!(
            network_id = nid.0,
            kind,
            client_id = client_id.0,
            session_agent = true,
            "[spawn_session_agent] session agent spawned (SessionAgent marker attached + WorkspaceIndex updated)"
        );
        nid
    }

    fn setup_world(&mut self) {
        // VS-02 — Empty start (0 entities).
        // The master workspace will be created lazily on the first spawn_kind call.
    }

    /// Queues a reliable platform event for a specific client (or all clients if None).
    fn queue_reliable_event(
        &mut self,
        client_id: Option<aetheris_protocol::types::ClientId>,
        event: aetheris_protocol::events::PlatformEvent,
    ) {
        if let Some(mut reliable) = self
            .world
            .get_resource_mut::<crate::components::ReliableEvents>()
        {
            reliable.queue.push((
                client_id,
                aetheris_protocol::events::WireEvent::PlatformEvent(event),
            ));
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
            .query_filtered::<Entity, bevy_ecs::prelude::With<crate::components::ResourceRespawn>>(
            );
        let to_despawn: Vec<_> = respawn_query.iter(&self.world).collect();
        for entity in to_despawn {
            self.world.despawn(entity);
        }

        self.allocator.reset();

        // VS-06 — Explicitly clear WorkspaceIndex to prevent stale memberships/client mappings
        if let Some(mut index) = self
            .world
            .get_resource_mut::<crate::components::WorkspaceIndex>()
        {
            index.memberships.clear();
            index.client_workspaces.clear();
        }

        // Force a full state extraction for all entities in the next tick
        self.last_extraction_tick = None;

        // Recreate the Master Workspace after a clear
        self.setup_world();
    }

    fn get_entity_workspace(&self, network_id: NetworkId) -> Option<NetworkId> {
        let entity = self.bimap.get_by_left(&network_id)?;
        let membership = self.world.get::<WorkspaceMembershipComponent>(*entity)?;
        Some(membership.0.0)
    }

    #[allow(clippy::collapsible_if)]
    fn get_client_workspace(&self, client_id: ClientId) -> Option<NetworkId> {
        // 1. Check the fast lookup index
        if let Some(index) = self
            .world
            .get_resource::<crate::components::WorkspaceIndex>()
        {
            if let Some(&workspace_id) = index.client_workspaces.get(&client_id) {
                return Some(workspace_id);
            }
        }

        // 2. Fallback for Playground: Every client is in Playground_Master by default
        // We use a manual loop over bimap to avoid needing &mut World for a Query
        for (_, entity) in &self.bimap {
            if let Some(def) = self.world.get::<WorkspaceDefinitionComponent>(*entity) {
                if def.0.name.as_str() == "Playground_Master" {
                    return self.bimap.get_by_right(entity).copied();
                }
            }
        }
        None
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

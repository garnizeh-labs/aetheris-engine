use crate::components::{
    DataDropComponent, DataStore, ExtractionBeam, NetworkOwner, ReliableEvents, Resource,
    ResourceRespawn, ServerTick, TransformComponent,
};
use aetheris_protocol::events::PlatformEvent;
use aetheris_protocol::types::NetworkId;
use bevy_ecs::prelude::{Entity, World};
use bimap::BiHashMap;

/// Processes the extraction loop for all active extraction beams.
///
/// This system implements the server-authoritative extraction logic:
/// 1. Validation of range and target.
/// 2. Payload transfer from Resource to `DataStore`.
/// 3. Resource exhaustion.
#[allow(clippy::missing_panics_doc)]
pub fn process_extraction(world: &mut World, bimap: &BiHashMap<NetworkId, Entity>) -> Vec<Entity> {
    let mut transfers = Vec::new(); // (Agent, Resource, Amount)
    let mut to_despawn = Vec::new();
    let mut beams_to_disable = Vec::new();

    let server_tick = world.get_resource::<ServerTick>().map_or(0, |t| t.0);

    // 1. Collect all potential extraction attempts without borrowing World mutably yet
    let mut potential_attempts = Vec::new();
    {
        let mut query = world.query::<(Entity, &ExtractionBeam, &TransformComponent, &DataStore)>();
        for (entity, beam, transform, store) in query.iter(world) {
            if !beam.active {
                continue;
            }

            if let Some(target_id) = beam.target {
                if let Some(&target_entity) = bimap.get_by_left(&target_id) {
                    potential_attempts.push((entity, target_entity, *transform, *store, *beam));
                } else {
                    beams_to_disable.push(entity);
                }
            } else {
                beams_to_disable.push(entity);
            }
        }
    }

    // 2. Validate against targets
    for (agent_entity, target_entity, transform, store, beam) in potential_attempts {
        let target_data = world.get_entity(target_entity).ok();
        let mut resource_data = None;
        let mut target_transform = None;

        if let Some(data) = target_data {
            resource_data = data.get::<Resource>();
            target_transform = data.get::<TransformComponent>();
        }

        if let (Some(resource), Some(t_transform)) = (resource_data, target_transform) {
            // Range validation
            let dx = t_transform.0.x - transform.0.x;
            let dy = t_transform.0.y - transform.0.y;
            let dist_sq = dx * dx + dy * dy;

            if dist_sq > beam.extraction_range * beam.extraction_range {
                beams_to_disable.push(agent_entity);
                continue;
            }

            // DataStore full validation
            if store.payload_count >= store.capacity {
                beams_to_disable.push(agent_entity);
                continue;
            }

            if resource.payload_remaining > 0 {
                // To slow down extraction even further, we only extract every 10 ticks (6 times per second)
                if server_tick.is_multiple_of(10) {
                    let dist = dist_sq.sqrt();
                    // Efficiency: linear falloff from 1.0 (at dist 0) to 0.0 (at extraction_range)
                    let efficiency = (1.0 - (dist / beam.extraction_range)).max(0.1);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let amount = (f32::from(beam.base_extraction_rate) * efficiency).round() as u16;
                    let amount = amount.max(1); // At least 1 per 10 ticks if active

                    transfers.push((agent_entity, target_entity, amount));
                }
            }
        } else {
            beams_to_disable.push(agent_entity);
        }
    }

    // 3. Apply state changes
    for entity in beams_to_disable {
        if let Some(mut beam) = world.get_mut::<ExtractionBeam>(entity) {
            beam.active = false;
            tracing::debug!(
                ?entity,
                "Extraction beam disabled due to validation failure"
            );
        }
    }

    // Execute transfers
    for (agent_entity, resource_entity, base_amount) in transfers {
        let (payload_to_add, payload_to_remove) = {
            let agent = world.get_entity(agent_entity).unwrap();
            let store = agent.get::<DataStore>().unwrap();
            let resource = world
                .get_entity(resource_entity)
                .unwrap()
                .get::<Resource>()
                .unwrap();

            let amount = base_amount
                .min(resource.payload_remaining)
                .min(store.capacity - store.payload_count);

            (amount, amount)
        };

        if payload_to_add > 0 {
            let mut agent_mut = world.get_entity_mut(agent_entity).unwrap();
            let mut store = agent_mut.get_mut::<DataStore>().unwrap();
            store.payload_count += payload_to_add;

            let owner = agent_mut.get::<NetworkOwner>().map(|o| o.0.0.to_string());

            let mut resource_mut = world.get_entity_mut(resource_entity).unwrap();
            let mut resource = resource_mut.get_mut::<Resource>().unwrap();
            resource.payload_remaining -= payload_to_remove;

            if let Some(client_id) = owner {
                metrics::counter!("aetheris_extraction_payload_transferred_total", "client_id" => client_id)
                    .increment(u64::from(payload_to_add));
            } else {
                metrics::counter!("aetheris_extraction_payload_transferred_total", "client_id" => "ai")
                    .increment(u64::from(payload_to_add));
            }
        }
    }

    // 4. Resource Exhaustion
    handle_resource_exhaustion(world, bimap, &mut to_despawn);

    to_despawn
}

/// Helper function to handle resource exhaustion logic.
fn handle_resource_exhaustion(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
    to_despawn: &mut Vec<Entity>,
) {
    let mut exhausted = Vec::new();
    {
        let mut query = world.query::<(Entity, &Resource, &TransformComponent)>();
        for (entity, resource, transform) in query.iter(world) {
            if resource.payload_remaining == 0 {
                exhausted.push((entity, *transform, resource.total_capacity));
            }
        }
    }

    if !exhausted.is_empty() {
        let mut events_to_push = Vec::new();
        let mut trackers_to_spawn = Vec::new();

        for (entity, transform, capacity) in exhausted {
            if let Some(network_id) = bimap.get_by_right(&entity) {
                // Platform event (ReliableMessage)
                events_to_push.push((
                    None, // Broadcast to all
                    aetheris_protocol::events::WireEvent::PlatformEvent(
                        PlatformEvent::ResourceExhausted {
                            network_id: *network_id,
                        },
                    ),
                ));

                // Spawn respawn tracker (300 ticks)
                trackers_to_spawn.push(ResourceRespawn {
                    delay_ticks: 300,
                    remaining: 301,
                    x: transform.0.x,
                    y: transform.0.y,
                    total_capacity: capacity,
                });

                metrics::counter!("aetheris_resource_exhaustions_total").increment(1);

                tracing::info!(?network_id, "Resource exhausted and queued for respawn");

                to_despawn.push(entity);
            }
        }

        // Apply deferred changes
        if let Some(mut reliable) = world.get_resource_mut::<ReliableEvents>() {
            reliable.queue.extend(events_to_push);
        }
        for tracker in trackers_to_spawn {
            world.spawn(tracker);
        }
    }
}

/// Processes resource respawn timers.
pub fn process_respawn(world: &mut World) -> Vec<(f32, f32, u16)> {
    let mut to_spawn = Vec::new();
    let mut to_despawn = Vec::new();

    let mut query = world.query::<(Entity, &mut ResourceRespawn)>();
    for (entity, mut respawn) in query.iter_mut(world) {
        respawn.remaining = respawn.remaining.saturating_sub(1);
        if respawn.remaining == 0 {
            to_spawn.push((respawn.x, respawn.y, respawn.total_capacity));
            to_despawn.push(entity);
        }
    }

    for entity in to_despawn {
        world.despawn(entity);
    }

    to_spawn
}

/// Processes collection of data drops by agents with data stores.
pub fn process_payload_collection(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
) -> Vec<Entity> {
    let mut to_despawn = Vec::new();
    let mut collections = Vec::new(); // (AgentEntity, DropEntity, Amount)

    // 1. Find all drops and nearby agents that can collect them
    {
        let mut drop_query = world.query::<(Entity, &DataDropComponent, &TransformComponent)>();
        let mut agent_query = world.query::<(Entity, &DataStore, &TransformComponent)>();

        for (d_entity, d_comp, d_transform) in drop_query.iter(world) {
            for (a_entity, a_store, a_transform) in agent_query.iter(world) {
                if d_entity == a_entity {
                    continue;
                }

                let dx = d_transform.0.x - a_transform.0.x;
                let dy = d_transform.0.y - a_transform.0.y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq < 10.0 * 10.0 {
                    // 10m collection radius
                    let available_space = a_store.capacity.saturating_sub(a_store.payload_count);
                    if available_space > 0 {
                        let amount = d_comp.0.amount.min(available_space);
                        collections.push((a_entity, d_entity, amount));
                        break; // This drop is claimed
                    }
                }
            }
        }
    }

    // 2. Apply collections
    for (a_entity, d_entity, amount) in collections {
        if amount > 0 {
            if let Some(mut a_store) = world.get_mut::<DataStore>(a_entity) {
                a_store.payload_count += amount;
            }

            if let Some(mut d_comp) = world.get_mut::<DataDropComponent>(d_entity) {
                if amount >= d_comp.0.amount {
                    d_comp.0.amount = 0;
                    to_despawn.push(d_entity);
                } else {
                    d_comp.0.amount -= amount;
                }
            }

            if let Some(&drop_id) = bimap.get_by_right(&d_entity)
                && let Some(mut reliable) = world.get_resource_mut::<ReliableEvents>()
            {
                reliable.queue.push((
                    None,
                    aetheris_protocol::events::WireEvent::PlatformEvent(
                        PlatformEvent::PayloadCollected {
                            network_id: drop_id,
                            amount,
                        },
                    ),
                ));
            }
        }
    }

    to_despawn
}

use crate::components::{
    Asteroid, AsteroidRespawn, CargoHold, MiningBeam, NetworkOwner, ReliableEvents,
    TransformComponent,
};
use crate::physics_consts::{MINING_ORE_PER_TICK, MINING_RANGE};
use aetheris_protocol::events::GameEvent;
use aetheris_protocol::types::NetworkId;
use bevy_ecs::prelude::{Entity, World};
use bimap::BiHashMap;

/// Processes the mining loop for all active mining beams.
///
/// This system implements the server-authoritative mining logic:
/// 1. Validation of range and target.
/// 2. Ore transfer from Asteroid to `CargoHold`.
/// 3. Resource depletion.
#[allow(clippy::missing_panics_doc)]
pub fn process_mining(world: &mut World, bimap: &BiHashMap<NetworkId, Entity>) -> Vec<Entity> {
    let mut transfers = Vec::new();
    let mut to_despawn = Vec::new();

    // 1. Collect all mining attempts
    {
        let mut query = world.query::<(Entity, &MiningBeam, &TransformComponent, &CargoHold)>();
        for (entity, beam, transform, cargo) in query.iter(world) {
            if !beam.active {
                continue;
            }

            let Some(target_id) = beam.target else {
                continue;
            };

            let Some(&target_entity) = bimap.get_by_left(&target_id) else {
                continue;
            };

            // Check if target is an asteroid
            let target_data = world.get_entity(target_entity).ok();
            let mut asteroid_data = None;
            let mut target_transform = None;

            if let Some(data) = target_data {
                asteroid_data = data.get::<Asteroid>();
                target_transform = data.get::<TransformComponent>();
            }

            if let (Some(asteroid), Some(t_transform)) = (asteroid_data, target_transform) {
                // Range validation
                let dx = transform.0.x - t_transform.0.x;
                let dy = transform.0.y - t_transform.0.y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq <= MINING_RANGE * MINING_RANGE
                    && asteroid.ore_remaining > 0
                    && cargo.ore_count < cargo.capacity
                {
                    transfers.push((entity, target_entity));
                }
            }
        }
    }

    // 2. Execute transfers
    for (ship_entity, asteroid_entity) in transfers {
        let (ore_to_add, ore_to_remove) = {
            let ship_mut = world.get_entity(ship_entity).unwrap();
            let cargo = ship_mut.get::<CargoHold>().unwrap();

            let asteroid_mut = world.get_entity(asteroid_entity).unwrap();
            let asteroid = asteroid_mut.get::<Asteroid>().unwrap();

            let amount = MINING_ORE_PER_TICK
                .min(asteroid.ore_remaining)
                .min(cargo.capacity - cargo.ore_count);

            (amount, amount)
        };

        if ore_to_add > 0 {
            let mut ship_mut = world.get_entity_mut(ship_entity).unwrap();
            let mut cargo = ship_mut.get_mut::<CargoHold>().unwrap();
            cargo.ore_count += ore_to_add;

            let owner = ship_mut.get::<NetworkOwner>().map(|o| o.0.0.to_string());

            let mut asteroid_mut = world.get_entity_mut(asteroid_entity).unwrap();
            let mut asteroid = asteroid_mut.get_mut::<Asteroid>().unwrap();
            asteroid.ore_remaining -= ore_to_remove;

            // M1062 — Observability: Track ore transfer
            if let Some(client_id) = owner {
                metrics::counter!("aetheris_mining_ore_transferred_total", "client_id" => client_id)
                    .increment(u64::from(ore_to_add));
            } else {
                metrics::counter!("aetheris_mining_ore_transferred_total", "client_id" => "ai")
                    .increment(u64::from(ore_to_add));
            }
        }
    }

    // 3. Asteroid Depletion (VS-02 spec)
    handle_asteroid_depletion(world, bimap, &mut to_despawn);

    to_despawn
}

/// Helper function to handle asteroid depletion logic.
fn handle_asteroid_depletion(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
    to_despawn: &mut Vec<Entity>,
) {
    let mut depleted = Vec::new();
    {
        let mut query = world.query::<(Entity, &Asteroid, &TransformComponent)>();
        for (entity, asteroid, transform) in query.iter(world) {
            if asteroid.ore_remaining == 0 {
                depleted.push((entity, *transform, asteroid.total_capacity));
            }
        }
    }

    if !depleted.is_empty() {
        let mut events_to_push = Vec::new();
        let mut trackers_to_spawn = Vec::new();

        for (entity, transform, capacity) in depleted {
            if let Some(network_id) = bimap.get_by_right(&entity) {
                // VS-02 — Asteroid depletion event (ReliableMessage)
                events_to_push.push((
                    None, // Broadcast to all
                    GameEvent::AsteroidDepleted {
                        network_id: *network_id,
                    },
                ));

                // Spawn respawn tracker (300 ticks)
                trackers_to_spawn.push(AsteroidRespawn {
                    delay_ticks: 300,
                    remaining: 301,
                    x: transform.0.x,
                    y: transform.0.y,
                    total_capacity: capacity,
                });

                // M1062 — Observability: Track depletion
                metrics::counter!("aetheris_asteroid_depletions_total").increment(1);

                tracing::info!(?network_id, "Asteroid depleted and queued for respawn");

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
/// Processes asteroid respawn timers.
pub fn process_respawn(world: &mut World) -> Vec<(f32, f32, u16)> {
    let mut to_spawn = Vec::new();
    let mut to_despawn = Vec::new();

    let mut query = world.query::<(Entity, &mut AsteroidRespawn)>();
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

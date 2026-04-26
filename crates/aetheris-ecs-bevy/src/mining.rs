use crate::components::{
    Asteroid, AsteroidRespawn, CargoDropComponent, CargoHold, MiningBeam, NetworkOwner,
    ReliableEvents, ServerTick, TransformComponent,
};
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
    let mut transfers = Vec::new(); // (Ship, Asteroid, Amount)
    let mut to_despawn = Vec::new();
    let mut beams_to_disable = Vec::new();

    let server_tick = world.get_resource::<ServerTick>().map_or(0, |t| t.0);

    // 1. Collect all potential mining attempts without borrowing World mutably yet
    let mut potential_attempts = Vec::new();
    {
        let mut query = world.query::<(Entity, &MiningBeam, &TransformComponent, &CargoHold)>();
        for (entity, beam, transform, cargo) in query.iter(world) {
            if !beam.active {
                continue;
            }

            if let Some(target_id) = beam.target {
                if let Some(&target_entity) = bimap.get_by_left(&target_id) {
                    potential_attempts.push((entity, target_entity, *transform, *cargo, *beam));
                } else {
                    beams_to_disable.push(entity);
                }
            } else {
                beams_to_disable.push(entity);
            }
        }
    }

    // 2. Validate against targets
    for (ship_entity, target_entity, transform, cargo, beam) in potential_attempts {
        let target_data = world.get_entity(target_entity).ok();
        let mut asteroid_data = None;
        let mut target_transform = None;

        if let Some(data) = target_data {
            asteroid_data = data.get::<Asteroid>();
            target_transform = data.get::<TransformComponent>();
        }

        if let (Some(asteroid), Some(t_transform)) = (asteroid_data, target_transform) {
            // Range validation
            let dx = t_transform.0.x - transform.0.x;
            let dy = t_transform.0.y - transform.0.y;
            let dist_sq = dx * dx + dy * dy;

            if dist_sq > beam.mining_range * beam.mining_range {
                beams_to_disable.push(ship_entity);
                continue;
            }

            // Cargo full validation
            if cargo.ore_count >= cargo.capacity {
                beams_to_disable.push(ship_entity);
                continue;
            }

            // VS-02 — No more arc validation. Radial proximity only.
            if asteroid.ore_remaining > 0 {
                // To slow down mining even further, we only mine every 10 ticks (6 times per second)
                if server_tick.is_multiple_of(10) {
                    let dist = dist_sq.sqrt();
                    // Efficiency: linear falloff from 1.0 (at dist 0) to 0.0 (at mining_range)
                    // Min efficiency of 0.1 to avoid total stall at range edge.
                    let efficiency = (1.0 - (dist / beam.mining_range)).max(0.1);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let amount = (f32::from(beam.base_mining_rate) * efficiency).round() as u16;
                    let amount = amount.max(1); // At least 1 per 10 ticks if active

                    transfers.push((ship_entity, target_entity, amount));
                }
            }
        } else {
            beams_to_disable.push(ship_entity);
        }
    }

    // 3. Apply state changes
    for entity in beams_to_disable {
        if let Some(mut beam) = world.get_mut::<MiningBeam>(entity) {
            beam.active = false;
            tracing::debug!(?entity, "Mining beam disabled due to validation failure");
        }
    }

    // Execute transfers
    for (ship_entity, asteroid_entity, base_amount) in transfers {
        let (ore_to_add, ore_to_remove) = {
            let ship = world.get_entity(ship_entity).unwrap();
            let cargo = ship.get::<CargoHold>().unwrap();
            let asteroid = world
                .get_entity(asteroid_entity)
                .unwrap()
                .get::<Asteroid>()
                .unwrap();

            let amount = base_amount
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

            if let Some(client_id) = owner {
                metrics::counter!("aetheris_mining_ore_transferred_total", "client_id" => client_id)
                    .increment(u64::from(ore_to_add));
            } else {
                metrics::counter!("aetheris_mining_ore_transferred_total", "client_id" => "ai")
                    .increment(u64::from(ore_to_add));
            }
        }
    }

    // 4. Asteroid Depletion
    handle_asteroid_depletion(world, bimap, &mut to_despawn);

    to_despawn
}

/// Helper function to handle asteroid depletion logic.
fn handle_asteroid_depletion(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
    to_despawn: &mut Vec<Entity>,
) {
    use crate::deterministic_rng::DeterministicRng;
    use rand::RngExt;

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

                // Deterministic offset calculation
                let (offset_x, offset_y) = if let Some(mut rng_res) =
                    world.get_resource_mut::<DeterministicRng>()
                {
                    (
                        rng_res.inner_mut().random_range(-15.0..15.0),
                        rng_res.inner_mut().random_range(-15.0..15.0),
                    )
                } else {
                    tracing::warn!(
                        "DeterministicRng resource missing in mining offset calculation; using zero offset"
                    );
                    (0.0, 0.0)
                };

                // Spawn respawn tracker (300 ticks)
                trackers_to_spawn.push(AsteroidRespawn {
                    delay_ticks: 300,
                    remaining: 301,
                    x: transform.0.x + offset_x,
                    y: transform.0.y + offset_y,
                    total_capacity: capacity,
                });

                // M1062 — Observability: Track depletion
                metrics::counter!("aetheris_asteroid_depletions_total").increment(1);

                tracing::info!(
                    ?network_id,
                    "Asteroid depleted and queued for respawn at ({:.2}, {:.2})",
                    transform.0.x + offset_x,
                    transform.0.y + offset_y
                );

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

/// Processes collection of cargo drops by ships with cargo holds.
#[allow(clippy::too_many_lines)]
pub fn process_cargo_collection(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
) -> Vec<Entity> {
    let mut to_despawn = Vec::new();
    let mut collections = Vec::new(); // (ShipEntity, DropEntity, Amount)

    // 1. Find all drops and nearby ships that can collect them
    {
        let mut drop_query = world.query::<(Entity, &CargoDropComponent, &TransformComponent)>();
        let mut ship_query = world.query::<(Entity, &CargoHold, &TransformComponent)>();

        for (d_entity, d_comp, d_transform) in drop_query.iter(world) {
            for (s_entity, s_cargo, s_transform) in ship_query.iter(world) {
                // Ignore if the ship is actually a drop (shouldn't happen with queries but good to be safe)
                if d_entity == s_entity {
                    continue;
                }

                let dx = d_transform.0.x - s_transform.0.x;
                let dy = d_transform.0.y - s_transform.0.y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq < 10.0 * 10.0 {
                    // 10m collection radius
                    let available_space = s_cargo.capacity.saturating_sub(s_cargo.ore_count);
                    if available_space > 0 {
                        let amount = d_comp.0.quantity.min(available_space);
                        collections.push((s_entity, d_entity, amount));
                        break; // This drop is claimed
                    }
                }
            }
        }
    }

    // 2. Apply collections
    for (s_entity, d_entity, amount) in collections {
        if amount > 0 {
            if let Some(mut s_cargo) = world.get_mut::<CargoHold>(s_entity) {
                s_cargo.ore_count += amount;
            }

            if let Some(mut d_comp) = world.get_mut::<CargoDropComponent>(d_entity) {
                if amount >= d_comp.0.quantity {
                    d_comp.0.quantity = 0;
                    to_despawn.push(d_entity);
                } else {
                    d_comp.0.quantity -= amount;
                }
            }

            // VS-02 — Play collection sound/effect via event
            if let Some(&drop_id) = bimap.get_by_right(&d_entity)
                && let Some(mut reliable) = world.get_resource_mut::<ReliableEvents>()
            {
                reliable.queue.push((
                    None,
                    GameEvent::CargoCollected {
                        network_id: drop_id,
                        amount,
                    },
                ));
            }
        } else {
            // Even if amount is 0 (full cargo), we might want to despawn it if the player "touches" it
            // but for now let's just leave it there.
        }
    }

    to_despawn
}

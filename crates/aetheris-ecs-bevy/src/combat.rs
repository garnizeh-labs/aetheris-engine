use crate::components::{
    HullPoolComponent, LatestInput, ProjectileMarker, ReliableEvents, RespawnTimer, ServerTick,
    ShieldPoolComponent, TransformComponent, WeaponComponent,
};
use aetheris_protocol::events::GameEvent;
use aetheris_protocol::types::{ACTION_FIRE_WEAPON, NetworkId};
use bevy_ecs::prelude::{Entity, Resource, World};
use bimap::BiHashMap;

#[derive(Resource, Default)]
pub struct ProjectileSpawnRequests(pub Vec<ProjectileSpawn>);

pub struct ProjectileSpawn {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub owner: NetworkId,
}

/// Processes authoritative combat logic.
///
/// # Panics
///
/// Panics if a target entity found in the query does not have a `TransformComponent`.
pub fn process_combat(world: &mut World, bimap: &BiHashMap<NetworkId, Entity>) -> Vec<Entity> {
    let mut firing_attempts = Vec::new();
    let server_tick = world.get_resource::<ServerTick>().map_or(0, |t| t.0);

    // 1. Collect firing attempts
    {
        let mut query = world.query::<(
            Entity,
            &LatestInput,
            &mut WeaponComponent,
            &TransformComponent,
        )>();
        for (entity, latest, mut weapon, transform) in query.iter_mut(world) {
            if (latest.command.actions_mask & ACTION_FIRE_WEAPON) != 0 {
                // Check cooldown
                if server_tick >= weapon.0.last_fired_tick + u64::from(weapon.0.cooldown_ticks) {
                    weapon.0.last_fired_tick = server_tick;
                    firing_attempts.push((entity, *transform));
                }
            }
        }
    }

    // 2. Spawn Projectiles (Auto-aim nearest enemy within 200m)
    for (attacker_entity, attacker_transform) in firing_attempts {
        let mut best_target = None;
        let mut closest_dist_sq = 200.0 * 200.0;

        // VS-03: Target anything with health (HullPool or AsteroidHP)
        let mut target_query = world.query::<(
            Entity,
            &TransformComponent,
            Option<&HullPoolComponent>,
            Option<&crate::components::AsteroidHP>,
        )>();

        for (t_entity, t_transform, opt_hull, opt_asteroid) in target_query.iter(world) {
            if t_entity == attacker_entity {
                continue;
            }

            // Must have some health to be a target
            if opt_hull.is_none() && opt_asteroid.is_none() {
                continue;
            }

            let dx = t_transform.0.x - attacker_transform.0.x;
            let dy = t_transform.0.y - attacker_transform.0.y;
            let dist_sq = dx * dx + dy * dy;

            if dist_sq < closest_dist_sq {
                closest_dist_sq = dist_sq;
                best_target = Some(t_entity);
            }
        }

        // Calculate direction using manual math
        let direction = if let Some(target_entity) = best_target {
            let target_transform = world.get::<TransformComponent>(target_entity).unwrap();
            let dx = target_transform.0.x - attacker_transform.0.x;
            let dy = target_transform.0.y - attacker_transform.0.y;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > 0.001 {
                [dx / dist, dy / dist]
            } else {
                [
                    attacker_transform.0.rotation.cos(),
                    attacker_transform.0.rotation.sin(),
                ]
            }
        } else {
            [
                attacker_transform.0.rotation.cos(),
                attacker_transform.0.rotation.sin(),
            ]
        };

        let projectile_velocity = [direction[0] * 35.0, direction[1] * 35.0];

        if let Some(mut requests) = world.get_resource_mut::<ProjectileSpawnRequests>() {
            requests.0.push(ProjectileSpawn {
                pos: [attacker_transform.0.x, attacker_transform.0.y],
                vel: [projectile_velocity[0], projectile_velocity[1]],
                owner: bimap
                    .get_by_right(&attacker_entity)
                    .copied()
                    .unwrap_or(NetworkId(0)),
            });
        }
    }

    Vec::new()
}

/// Processes projectile movement, collision and lifetime.
#[allow(clippy::too_many_lines)]
pub fn process_projectiles(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
) -> (Vec<Entity>, Vec<Entity>) {
    let mut to_despawn = Vec::new();
    let mut deaths = Vec::new();
    let mut damage_events = Vec::new();

    // 1. Projectile collision & lifetime
    {
        // Use a query for projectiles
        let mut projectiles = world.query::<(Entity, &ProjectileMarker, &TransformComponent)>();
        let p_count = projectiles.iter(world).count();
        if p_count > 0 {
            tracing::warn!(p_count, "Processing projectiles");
        }
        let mut projectile_data = Vec::new();
        for (entity, marker, transform) in projectiles.iter(world) {
            projectile_data.push((
                entity,
                marker.spawn_pos,
                marker.max_range,
                marker.owner,
                marker.lifetime_ticks,
                transform.0.x,
                transform.0.y,
            ));
        }

        // Collision check (Simple radius)
        let mut target_query = world.query::<(
            Entity,
            Option<&mut ShieldPoolComponent>,
            Option<&mut HullPoolComponent>,
            Option<&mut crate::components::AsteroidHP>,
            &TransformComponent,
        )>();

        for (p_entity, spawn_pos, max_range, owner_nid, lifetime, px, py) in projectile_data {
            // Lifetime check
            if lifetime == 0 {
                tracing::warn!(?p_entity, "Projectile expired by lifetime");
                to_despawn.push(p_entity);
                continue;
            }

            // VS-03: Update lifetime in the component (we have to borrow again since projectile_data is a copy)
            if let Some(mut marker) = world.get_mut::<ProjectileMarker>(p_entity) {
                marker.lifetime_ticks = marker.lifetime_ticks.saturating_sub(1);
            }

            // Distance check (M1038 Refinement)
            let dx = px - spawn_pos[0];
            let dy = py - spawn_pos[1];
            let dist_sq = dx * dx + dy * dy;
            if dist_sq > max_range * max_range {
                tracing::warn!(
                    ?p_entity,
                    dist = dist_sq.sqrt(),
                    max = max_range,
                    "Projectile exceeded range"
                );
                to_despawn.push(p_entity);
                continue;
            }

            let mut hit = false;
            for (t_entity, opt_shield, opt_hull, opt_asteroid, t_transform) in
                target_query.iter_mut(world)
            {
                if t_entity == p_entity {
                    continue;
                }

                // Skip target if it has no health component (i.e. it's another projectile or untargetable)
                if opt_shield.is_none() && opt_hull.is_none() && opt_asteroid.is_none() {
                    continue;
                }

                // Skip collision with owner to prevent self-hitting on spawn
                if let Some(&t_nid) = bimap.get_by_right(&t_entity)
                    && t_nid == owner_nid
                {
                    continue;
                }

                let dx = t_transform.0.x - px;
                let dy = t_transform.0.y - py;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq < 4.0 * 4.0 {
                    // 4m hit radius
                    hit = true;
                    tracing::debug!(?p_entity, ?t_entity, "Projectile hit target");

                    // Apply damage
                    let damage = 25u16;
                    let mut remaining = damage;

                    // 1. Try Shield/Hull (Ships/Dummies)
                    if let Some(mut shield) = opt_shield {
                        if shield.0.current >= remaining {
                            shield.0.current -= remaining;
                        } else {
                            remaining -= shield.0.current;
                            shield.0.current = 0;
                        }
                    }

                    if remaining > 0
                        && let Some(mut hull) = opt_hull
                    {
                        if hull.0.current >= remaining {
                            hull.0.current -= remaining;
                        } else {
                            remaining -= hull.0.current;
                            hull.0.current = 0;
                        }

                        if hull.0.current == 0 {
                            deaths.push(t_entity);
                        }
                    }

                    // 2. Try AsteroidHP (Asteroids)
                    if remaining > 0
                        && let Some(mut asteroid_hp) = opt_asteroid
                    {
                        if asteroid_hp.hp >= remaining {
                            asteroid_hp.hp -= remaining;
                        } else {
                            asteroid_hp.hp = 0;
                        }

                        if asteroid_hp.hp == 0 {
                            deaths.push(t_entity);
                        }
                    }

                    if let Some(&target_id) = bimap.get_by_right(&t_entity) {
                        damage_events.push((
                            None,
                            GameEvent::DamageEvent {
                                source: owner_nid,
                                target: target_id,
                                amount: damage,
                            },
                        ));
                    }

                    break;
                }
            }

            if hit {
                to_despawn.push(p_entity);
            }
        }
    }

    // Broadcast damage events
    if !damage_events.is_empty()
        && let Some(mut reliable) = world.get_resource_mut::<ReliableEvents>()
    {
        for (cid, event) in damage_events {
            reliable.queue.push((cid, event));
        }
    }

    (to_despawn, deaths)
}

/// Processes shield regeneration.
pub fn process_regen(world: &mut World) {
    let mut query = world.query::<(
        &mut ShieldPoolComponent,
        &mut crate::components::ShieldRegenTimer,
    )>();
    for (mut shield, mut timer) in query.iter_mut(world) {
        if timer.ticks_until_regen > 0 {
            timer.ticks_until_regen = timer.ticks_until_regen.saturating_sub(1);
        } else if shield.0.current < shield.0.max {
            shield.0.current = (shield.0.current + 1).min(shield.0.max);
        }
    }
}

/// Processes training dummy respawns.
pub fn process_dummy_respawn(world: &mut World) -> Vec<(f32, f32)> {
    let mut respawn_list = Vec::new();
    let mut despawn_list = Vec::new();

    let mut query = world.query::<(Entity, &mut RespawnTimer)>();
    for (entity, mut timer) in query.iter_mut(world) {
        timer.delay_ticks = timer.delay_ticks.saturating_sub(1);
        if timer.delay_ticks == 0 {
            if let aetheris_protocol::types::RespawnLocation::Coordinate(x, y) = timer.location {
                respawn_list.push((x, y));
            }
            despawn_list.push(entity);
        }
    }

    for entity in despawn_list {
        world.despawn(entity);
    }

    respawn_list
}

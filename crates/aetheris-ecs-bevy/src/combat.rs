use crate::components::{
    HullPoolComponent, LatestInput, ProjectileMarker, ReliableEvents, ServerTick,
    ShieldPoolComponent, TrainingDummy, TransformComponent, WeaponComponent,
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

        {
            let mut target_query = world.query::<(Entity, &TrainingDummy, &TransformComponent)>();
            for (target_entity, _, target_transform) in target_query.iter(world) {
                if target_entity == attacker_entity {
                    continue;
                }

                let dx = target_transform.0.x - attacker_transform.0.x;
                let dy = target_transform.0.y - attacker_transform.0.y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq < closest_dist_sq {
                    closest_dist_sq = dist_sq;
                    best_target = Some(target_entity);
                }
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

        let projectile_velocity = [direction[0] * 12.5, direction[1] * 12.5];

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
pub fn process_projectiles(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
) -> (Vec<Entity>, Vec<Entity>) {
    let mut to_despawn = Vec::new();
    let mut deaths = Vec::new();
    let server_tick = world.get_resource::<ServerTick>().map_or(0, |t| t.0);
    let mut damage_events = Vec::new();

    // 1. Projectile collision & lifetime
    {
        // Use a query for projectiles
        let mut projectiles = world.query::<(Entity, &ProjectileMarker, &TransformComponent)>();
        let mut projectile_data = Vec::new();
        for (entity, marker, transform) in projectiles.iter(world) {
            projectile_data.push((
                entity,
                marker.origin_tick,
                marker.owner,
                transform.0.x,
                transform.0.y,
            ));
        }

        for (p_entity, origin_tick, owner_nid, px, py) in projectile_data {
            // Lifetime: 2 seconds (120 ticks)
            if server_tick > origin_tick + 120 {
                to_despawn.push(p_entity);
                continue;
            }

            // Collision check (Simple radius)
            let mut target_query = world.query::<(
                Entity,
                &mut ShieldPoolComponent,
                &mut HullPoolComponent,
                &TransformComponent,
            )>();
            let mut hit = false;
            for (t_entity, mut shield, mut hull, t_transform) in target_query.iter_mut(world) {
                if t_entity == p_entity {
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

                if dist_sq < 3.0 * 3.0 {
                    // 3m hit radius
                    hit = true;

                    // Apply damage
                    let damage = 25u16;
                    let mut remaining = damage;

                    if shield.0.current >= remaining {
                        shield.0.current -= remaining;
                        remaining = 0;
                    } else {
                        remaining -= shield.0.current;
                        shield.0.current = 0;
                    }

                    if remaining > 0 {
                        if hull.0.current >= remaining {
                            hull.0.current -= remaining;
                        } else {
                            hull.0.current = 0;
                        }
                    }

                    if hull.0.current == 0 {
                        deaths.push(t_entity);
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
pub fn process_dummy_respawn(_world: &mut World) -> Vec<(f32, f32)> {
    Vec::new()
}

use crate::components::{
    HullPoolComponent, LatestInput, ReliableEvents, ServerTick, ShieldPoolComponent, TrainingDummy,
    TransformComponent, WeaponComponent,
};
use aetheris_protocol::events::GameEvent;
use aetheris_protocol::types::{ACTION_FIRE_WEAPON, NetworkId};
use bevy_ecs::prelude::{Entity, World};
use bimap::BiHashMap;

/// Processes authoritative combat logic.
pub fn process_combat(world: &mut World, bimap: &BiHashMap<NetworkId, Entity>) -> Vec<Entity> {
    let mut to_despawn = Vec::new();
    let mut damage_events = Vec::new();
    let mut deaths = Vec::new();

    let server_tick = world.get_resource::<ServerTick>().map_or(0, |t| t.0);

    // 1. Collect firing attempts
    let mut firing_attempts = Vec::new();
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

    // 2. Resolve hitscan (Targeting TrainingDummy)
    for (attacker_entity, attacker_transform) in firing_attempts {
        let mut best_target = None;
        let mut closest_dist_sq = 200.0 * 200.0; // Max range 200m

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
                    // Arc check: attacker must be facing the target
                    let target_angle = dy.atan2(dx);
                    let mut angle_diff = (target_angle - attacker_transform.0.rotation).abs();
                    while angle_diff > std::f32::consts::PI {
                        angle_diff -= std::f32::consts::TAU;
                    }
                    while angle_diff < -std::f32::consts::PI {
                        angle_diff += std::f32::consts::TAU;
                    }

                    // Allow 30 degree cone
                    if angle_diff.abs() < 30.0f32.to_radians() {
                        closest_dist_sq = dist_sq;
                        best_target = Some(target_entity);
                    }
                }
            }
        }

        if let Some(target_entity) = best_target {
            // Apply damage (Fixed 25 damage for training)
            let damage = 25u16;

            if let Some(target_id) = bimap.get_by_right(&target_entity) {
                damage_events.push((
                    None,
                    GameEvent::DamageEvent {
                        target: *target_id,
                        amount: damage,
                    },
                ));

                // Apply to pools
                let mut is_dead = false;
                if let Some(mut shield) = world.get_mut::<ShieldPoolComponent>(target_entity) {
                    if shield.0.current >= damage {
                        shield.0.current -= damage;
                    } else {
                        let overflow = damage - shield.0.current;
                        shield.0.current = 0;
                        if let Some(mut hull) = world.get_mut::<HullPoolComponent>(target_entity) {
                            hull.0.current = hull.0.current.saturating_sub(overflow);
                            if hull.0.current == 0 {
                                is_dead = true;
                            }
                        }
                    }
                } else if let Some(mut hull) = world.get_mut::<HullPoolComponent>(target_entity) {
                    hull.0.current = hull.0.current.saturating_sub(damage);
                    if hull.0.current == 0 {
                        is_dead = true;
                    }
                }

                if is_dead {
                    deaths.push(target_entity);
                }
            }
        }
    }

    // 3. Handle Deaths
    for dead_entity in deaths {
        if let Some(&target_id) = bimap.get_by_right(&dead_entity) {
            damage_events.push((None, GameEvent::DeathEvent { target: target_id }));

            // Spawn CargoDrop at position
            if let Some(_p) = world.get::<TransformComponent>(dead_entity) {
                // Return to adapter for spawning
                to_despawn.push(dead_entity);
            }
        }
    }

    // 4. Queue events
    if let Some(mut reliable) = world.get_resource_mut::<ReliableEvents>() {
        reliable.queue.extend(damage_events);
    }

    to_despawn
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
            // Regen 5 per tick if ready (simple for VS-03)
            shield.0.current = (shield.0.current + 1).min(shield.0.max);
        }
    }
}

/// Processes training dummy respawns.
pub fn process_dummy_respawn(world: &mut World) -> Vec<(f32, f32)> {
    let mut to_spawn = Vec::new();
    let mut to_despawn = Vec::new();

    let mut query = world.query::<(Entity, &mut crate::components::RespawnTimer, &TrainingDummy)>();
    for (entity, mut timer, _) in query.iter_mut(world) {
        timer.delay_ticks = timer.delay_ticks.saturating_sub(1);
        if timer.delay_ticks == 0 {
            // Respawn at a fixed location or where it died?
            // Let's say (50, 50) for now or random.
            // Using (50, 50) as per VS-03 target area.
            to_spawn.push((50.0, 50.0));
            to_despawn.push(entity);
        }
    }

    for entity in to_despawn {
        world.despawn(entity);
    }

    to_spawn
}

use crate::components::{
    BeamMarker, IntegrityPoolComponent, LatestInput, PriorityPoolComponent, ReliableEvents,
    RespawnTimer, ServerTick, ToolComponent, TransformComponent,
};
use aetheris_protocol::events::PlatformEvent;
use aetheris_protocol::types::{ACTION_USE_TOOL, NetworkId};
use bevy_ecs::prelude::{Entity, Resource, World};
use bimap::BiHashMap;

#[derive(Resource, Default)]
pub struct BeamSpawnRequests(pub Vec<BeamSpawn>);

pub struct BeamSpawn {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub owner: NetworkId,
}

/// Processes authoritative interaction (formerly combat) logic.
///
/// # Panics
///
/// Panics if a target entity is missing its `TransformComponent` during direction calculation.
pub fn process_interaction(world: &mut World, bimap: &BiHashMap<NetworkId, Entity>) -> Vec<Entity> {
    let mut use_attempts = Vec::new();
    let server_tick = world.get_resource::<ServerTick>().map_or(0, |t| t.0);

    // 1. Collect tool use attempts
    {
        let mut query = world.query::<(
            Entity,
            &LatestInput,
            &mut ToolComponent,
            &TransformComponent,
        )>();
        for (entity, latest, mut tool, transform) in query.iter_mut(world) {
            if (latest.command.actions_mask & ACTION_USE_TOOL) != 0 {
                // Check cooldown
                if server_tick >= tool.0.last_fired_tick + u64::from(tool.0.cooldown_ticks) {
                    tool.0.last_fired_tick = server_tick;
                    use_attempts.push((entity, *transform));
                }
            }
        }
    }

    // 2. Spawn Beams (Auto-aim nearest target within 200m)
    for (attacker_entity, attacker_transform) in use_attempts {
        let mut best_target = None;
        let mut closest_dist_sq = 200.0 * 200.0;

        let mut target_query = world.query::<(
            Entity,
            &TransformComponent,
            Option<&IntegrityPoolComponent>,
            Option<&crate::components::ResourceIntegrity>,
        )>();

        for (t_entity, t_transform, opt_integrity, opt_resource) in target_query.iter(world) {
            if t_entity == attacker_entity {
                continue;
            }

            if opt_integrity.is_none() && opt_resource.is_none() {
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

        let beam_velocity = [direction[0] * 35.0, direction[1] * 35.0];

        if let Some(mut requests) = world.get_resource_mut::<BeamSpawnRequests>() {
            requests.0.push(BeamSpawn {
                pos: [attacker_transform.0.x, attacker_transform.0.y],
                vel: [beam_velocity[0], beam_velocity[1]],
                owner: bimap
                    .get_by_right(&attacker_entity)
                    .copied()
                    .unwrap_or(NetworkId(0)),
            });
        }
    }

    Vec::new()
}

/// Processes beam movement, collision and lifetime.
pub fn process_beams(
    world: &mut World,
    bimap: &BiHashMap<NetworkId, Entity>,
) -> (Vec<Entity>, Vec<Entity>) {
    let mut to_despawn = Vec::new();
    let mut terminations = Vec::new();
    let mut interaction_events = Vec::new();

    // 1. Beam collision & lifetime
    {
        let mut beams = world.query::<(Entity, &BeamMarker, &TransformComponent)>();
        let beam_data: Vec<_> = beams
            .iter(world)
            .map(|(e, m, t)| {
                (
                    e,
                    m.spawn_pos,
                    m.max_range,
                    m.owner,
                    m.lifetime_ticks,
                    t.0.x,
                    t.0.y,
                )
            })
            .collect();

        let mut target_query = world.query::<(
            Entity,
            Option<&mut PriorityPoolComponent>,
            Option<&mut IntegrityPoolComponent>,
            Option<&mut crate::components::ResourceIntegrity>,
            &TransformComponent,
        )>();

        for (b_entity, spawn_pos, max_range, owner_nid, lifetime, px, py) in beam_data {
            if lifetime == 0
                || (px - spawn_pos[0]).powi(2) + (py - spawn_pos[1]).powi(2) > max_range.powi(2)
            {
                to_despawn.push(b_entity);
                continue;
            }

            if let Some(mut marker) = world.get_mut::<BeamMarker>(b_entity) {
                marker.lifetime_ticks = marker.lifetime_ticks.saturating_sub(1);
            }

            for (t_entity, opt_priority, opt_integrity, opt_resource, t_transform) in
                target_query.iter_mut(world)
            {
                if t_entity == b_entity
                    || opt_priority.is_none() && opt_integrity.is_none() && opt_resource.is_none()
                {
                    continue;
                }

                if bimap
                    .get_by_right(&t_entity)
                    .is_some_and(|&tn| tn == owner_nid)
                {
                    continue;
                }

                if (t_transform.0.x - px).powi(2) + (t_transform.0.y - py).powi(2) < 16.0 {
                    let mut term = false;
                    apply_beam_hit(opt_priority, opt_integrity, opt_resource, &mut term);
                    if term {
                        terminations.push(t_entity);
                    }

                    if let Some(&target_id) = bimap.get_by_right(&t_entity) {
                        interaction_events.push((
                            None,
                            PlatformEvent::Interaction {
                                source: owner_nid,
                                target: target_id,
                                amount: 25,
                            },
                        ));
                    }
                    to_despawn.push(b_entity);
                    break;
                }
            }
        }
    }

    if !interaction_events.is_empty()
        && let Some(mut reliable) = world.get_resource_mut::<ReliableEvents>()
    {
        for (cid, event) in interaction_events {
            reliable.queue.push((
                cid,
                aetheris_protocol::events::WireEvent::PlatformEvent(event),
            ));
        }
    }

    (to_despawn, terminations)
}

fn apply_beam_hit(
    mut opt_priority: Option<bevy_ecs::change_detection::Mut<PriorityPoolComponent>>,
    mut opt_integrity: Option<bevy_ecs::change_detection::Mut<IntegrityPoolComponent>>,
    mut opt_resource: Option<bevy_ecs::change_detection::Mut<crate::components::ResourceIntegrity>>,
    terminated: &mut bool,
) {
    let amount = 25u16;
    let mut remaining = amount;

    if let Some(priority) = opt_priority.as_mut() {
        if priority.0.current >= remaining {
            priority.0.current -= remaining;
            remaining = 0;
        } else {
            remaining -= priority.0.current;
            priority.0.current = 0;
        }
    }

    if remaining > 0 {
        if let Some(integrity) = opt_integrity.as_mut() {
            if integrity.0.current >= remaining {
                integrity.0.current -= remaining;
            } else {
                integrity.0.current = 0;
            }
            if integrity.0.current == 0 {
                *terminated = true;
            }
        } else if let Some(resource) = opt_resource.as_mut() {
            if resource.integrity >= remaining {
                resource.integrity -= remaining;
            } else {
                resource.integrity = 0;
            }
            if resource.integrity == 0 {
                *terminated = true;
            }
        }
    }
}

/// Processes priority pool regeneration.
pub fn process_regen(world: &mut World) {
    let mut query = world.query::<(
        &mut PriorityPoolComponent,
        &mut crate::components::PriorityRegenTimer,
    )>();
    for (mut priority, mut timer) in query.iter_mut(world) {
        if timer.ticks_until_regen > 0 {
            timer.ticks_until_regen = timer.ticks_until_regen.saturating_sub(1);
        } else if priority.0.current < priority.0.max {
            priority.0.current = (priority.0.current + 1).min(priority.0.max);
        }
    }
}

/// Processes training target reinitialization.
pub fn process_target_reinitialization(world: &mut World) -> Vec<(f32, f32)> {
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

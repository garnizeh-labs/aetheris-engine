use aetheris_ecs_bevy::BevyWorldAdapter;
use aetheris_ecs_bevy::components::*;
use aetheris_protocol::traits::WorldState;
use aetheris_protocol::types::{ACTION_FIRE_WEAPON, InputCommand};
use bevy_ecs::prelude::World;

#[test]
fn test_weapon_cooldown_enforcement() {
    let mut adapter = BevyWorldAdapter::new(World::new(), 60);
    let nid = adapter.spawn_kind(1, 0.0, 0.0, 0.0); // Ship kind 1

    // Set weapon state to allow immediate fire
    {
        let entity = adapter.get_local_id(nid).unwrap();
        let world = adapter.world_mut();
        let mut weapon = world
            .get_mut::<WeaponComponent>(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .unwrap();
        weapon.0.cooldown_ticks = 10;
        weapon.0.last_fired_tick = 0;
    }

    // Advance to tick 11 to clear initial cooldown (since 11 >= 0 + 10)
    for _ in 0..11 {
        adapter.advance_tick();
    }

    // 1. Fire at tick 11 -> OK
    {
        let entity = adapter.get_local_id(nid).unwrap();
        adapter
            .world_mut()
            .entity_mut(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .insert(LatestInput {
                command: InputCommand {
                    tick: 11,
                    actions: vec![],
                    actions_mask: ACTION_FIRE_WEAPON,
                    last_seen_input_tick: None,
                },
                last_client_tick: 11,
            });
    }
    adapter.simulate();

    {
        let entity = adapter.get_local_id(nid).unwrap();
        let weapon = adapter
            .world()
            .get::<WeaponComponent>(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .unwrap();
        assert_eq!(weapon.0.last_fired_tick, 11);
    }

    // 2. Fire at tick 15 -> Cooldown should block
    for _ in 0..4 {
        adapter.advance_tick();
    } // Tick 15
    {
        let entity = adapter.get_local_id(nid).unwrap();
        adapter
            .world_mut()
            .entity_mut(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .insert(LatestInput {
                command: InputCommand {
                    tick: 15,
                    actions: vec![],
                    actions_mask: ACTION_FIRE_WEAPON,
                    last_seen_input_tick: None,
                },
                last_client_tick: 15,
            });
    }
    adapter.simulate();

    {
        let entity = adapter.get_local_id(nid).unwrap();
        let weapon = adapter
            .world()
            .get::<WeaponComponent>(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .unwrap();
        assert_eq!(weapon.0.last_fired_tick, 11); // Still 11, didn't update
    }

    // 3. Fire at tick 21 -> OK (21 >= 11 + 10)
    for _ in 0..6 {
        adapter.advance_tick();
    } // Tick 21
    {
        let entity = adapter.get_local_id(nid).unwrap();
        adapter
            .world_mut()
            .entity_mut(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .insert(LatestInput {
                command: InputCommand {
                    tick: 21,
                    actions: vec![],
                    actions_mask: ACTION_FIRE_WEAPON,
                    last_seen_input_tick: None,
                },
                last_client_tick: 21,
            });
    }
    adapter.simulate();

    {
        let entity = adapter.get_local_id(nid).unwrap();
        let weapon = adapter
            .world()
            .get::<WeaponComponent>(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .unwrap();
        assert_eq!(weapon.0.last_fired_tick, 21);
    }
}

#[test]
fn test_hitscan_range() {
    let mut adapter = BevyWorldAdapter::new(World::new(), 60);
    let attacker_nid = adapter.spawn_kind(1, 0.0, 0.0, 0.0);
    let dummy_nid = adapter.spawn_kind(10, 250.0, 0.0, 0.0); // Outside 200m range

    // Advance to clear initial 30-tick cooldown
    for _ in 0..31 {
        adapter.advance_tick();
    }

    // Fire at tick 31
    {
        let entity = adapter.get_local_id(attacker_nid).unwrap();
        adapter
            .world_mut()
            .entity_mut(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .insert(LatestInput {
                command: InputCommand {
                    tick: 31,
                    actions: vec![],
                    actions_mask: ACTION_FIRE_WEAPON,
                    last_seen_input_tick: None,
                },
                last_client_tick: 31,
            });
    }
    adapter.simulate();

    // Verify no damage (Regen will keep it at max 50)
    let dummy_entity = adapter.get_local_id(dummy_nid).unwrap();
    let shield = adapter
        .world()
        .get::<ShieldPoolComponent>(bevy_ecs::prelude::Entity::from_bits(dummy_entity.0))
        .unwrap();
    assert_eq!(shield.0.current, 50);

    // Move dummy closer (1m) to hit immediately
    {
        let world = adapter.world_mut();
        let mut t = world
            .get_mut::<TransformComponent>(bevy_ecs::prelude::Entity::from_bits(dummy_entity.0))
            .unwrap();
        t.0.x = 1.0;
        // Disable regen for test predictability
        let mut timer = world
            .get_mut::<ShieldRegenTimer>(bevy_ecs::prelude::Entity::from_bits(dummy_entity.0))
            .unwrap();
        timer.ticks_until_regen = 1000;
    }

    // Clear cooldown (30 ticks)
    for _ in 0..31 {
        adapter.advance_tick();
    }
    let tick = 62;
    {
        let entity = adapter.get_local_id(attacker_nid).unwrap();
        adapter
            .world_mut()
            .entity_mut(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .insert(LatestInput {
                command: InputCommand {
                    tick,
                    actions: vec![],
                    actions_mask: ACTION_FIRE_WEAPON,
                    last_seen_input_tick: None,
                },
                last_client_tick: tick,
            });
    }
    adapter.simulate();
    // Clear firing mask to prevent multiple shots during the travel time wait
    {
        let entity = adapter.get_local_id(attacker_nid).unwrap();
        if let Some(mut latest) = adapter
            .world_mut()
            .get_mut::<LatestInput>(bevy_ecs::prelude::Entity::from_bits(entity.0))
        {
            latest.command.actions_mask = 0;
        }
    }

    // Simulate enough ticks
    for _ in 0..10 {
        adapter.simulate();
    }

    // Verify damage (Dummy has 50 shield. 25 damage should hit shield -> 25)
    let shield = adapter
        .world()
        .get::<ShieldPoolComponent>(bevy_ecs::prelude::Entity::from_bits(dummy_entity.0))
        .unwrap();
    assert_eq!(shield.0.current, 0);
}

#[test]
fn test_shield_hull_overflow() {
    let mut adapter = BevyWorldAdapter::new(World::new(), 60);
    let attacker_nid = adapter.spawn_kind(1, 0.0, 0.0, 0.0);
    let dummy_nid = adapter.spawn_kind(10, 1.0, 0.0, 0.0); // Move closer
    let dummy_entity =
        bevy_ecs::prelude::Entity::from_bits(adapter.get_local_id(dummy_nid).unwrap().0);

    // Set dummy to low shield (10) and high regen delay to prevent regen during test
    {
        let world = adapter.world_mut();
        let mut shield = world.get_mut::<ShieldPoolComponent>(dummy_entity).unwrap();
        shield.0.current = 10;
        let mut timer = world.get_mut::<ShieldRegenTimer>(dummy_entity).unwrap();
        timer.ticks_until_regen = 1000;
    }

    // Clear cooldown
    for _ in 0..31 {
        adapter.advance_tick();
    }
    let tick = 31;

    // Fire (25 damage)
    {
        let entity = adapter.get_local_id(attacker_nid).unwrap();
        adapter
            .world_mut()
            .entity_mut(bevy_ecs::prelude::Entity::from_bits(entity.0))
            .insert(LatestInput {
                command: InputCommand {
                    tick,
                    actions: vec![],
                    actions_mask: ACTION_FIRE_WEAPON,
                    last_seen_input_tick: None,
                },
                last_client_tick: tick,
            });
    }
    adapter.simulate();
    // Clear firing mask
    {
        let entity = adapter.get_local_id(attacker_nid).unwrap();
        if let Some(mut latest) = adapter
            .world_mut()
            .get_mut::<LatestInput>(bevy_ecs::prelude::Entity::from_bits(entity.0))
        {
            latest.command.actions_mask = 0;
        }
    }
    for _ in 0..9 {
        adapter.simulate();
    }

    // Shield should be 0, Hull should be 100 - (25 - 10) = 85
    let shield = adapter
        .world()
        .get::<ShieldPoolComponent>(dummy_entity)
        .unwrap();
    let hull = adapter
        .world()
        .get::<HullPoolComponent>(dummy_entity)
        .unwrap();
    assert_eq!(shield.0.current, 0);
    assert_eq!(hull.0.current, 85);
}

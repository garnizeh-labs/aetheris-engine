use aetheris_ecs_bevy::BevyWorldAdapter;
use aetheris_ecs_bevy::components::TransformComponent;
use aetheris_ecs_bevy::registry::register_void_rush_components;
use aetheris_protocol::traits::WorldState;
use alloc_counter::count_alloc;
use bevy_ecs::prelude::World;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::cell::RefCell;
use std::hint::black_box;

fn bench_ecs_extract_dirty(c: &mut Criterion) {
    let world = World::new();
    let mut adapter = BevyWorldAdapter::new(world, 60);
    let mut registry = aetheris_ecs_bevy::registry::ComponentRegistry::new();
    register_void_rush_components(&mut registry);
    for descriptor in registry.components.values() {
        adapter.register_replicator(descriptor.replicator.clone());
    }

    // Spawn 1000 entities
    let mut entities = Vec::new();
    for _ in 0..1000 {
        let nid = adapter.spawn_kind(1, 0.0, 0.0, 0.0); // Player Interceptor
        entities.push(nid);
    }

    // Warmup: 100 ticks to satisfy MEMORY_MANAGEMENT_DESIGN D4
    for _ in 0..100 {
        adapter.simulate();
        let _ = adapter.extract_deltas();
        adapter.post_extract();
    }

    let adapter = RefCell::new(adapter);

    c.bench_function("ecs_extract_dirty_7_of_1000", |b| {
        b.iter_batched(
            || {
                // Setup: Dirty 7 entities
                let mut adapter = adapter.borrow_mut();
                for nid in entities.iter().take(7) {
                    let local_id = adapter.get_local_id(*nid).unwrap();
                    let entity = bevy_ecs::prelude::Entity::from_bits(local_id.0);
                    if let Some(mut transform) =
                        adapter.world_mut().get_mut::<TransformComponent>(entity)
                    {
                        transform.0.x += 1.0;
                    }
                }
            },
            |_| {
                // VS-07 §3.2: Zero-alloc assertion
                let mut adapter = adapter.borrow_mut();
                let (allocs, _) = count_alloc(|| black_box(adapter.extract_deltas()));
                if allocs.0 > 0 || allocs.1 > 0 || allocs.2 > 0 {
                    panic!(
                        "ECS extraction allocated {:?} times during hot path!",
                        allocs
                    );
                }
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_ecs_extract_dirty);
criterion_main!(benches);

use aetheris_ecs_bevy::BevyWorldAdapter;
use aetheris_encoder_serde::SerdeEncoder;
use aetheris_protocol::error::TransportError;
use aetheris_protocol::events::NetworkEvent;
use aetheris_protocol::traits::{GameTransport, WorldState};
use aetheris_protocol::types::ClientId;
use aetheris_server::tick::TickScheduler;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug)]
struct NoOpTransport;
#[async_trait]
impl GameTransport for NoOpTransport {
    async fn send_unreliable(&self, _: ClientId, _: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }
    async fn send_reliable(&self, _: ClientId, _: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }
    async fn broadcast_unreliable(&self, _: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }
    async fn poll_events(&mut self) -> Result<Vec<NetworkEvent>, TransportError> {
        Ok(vec![])
    }
    async fn connected_client_count(&self) -> usize {
        0
    }
}

#[derive(Debug)]
struct NoOpAuth;
impl aetheris_server::auth::AuthSessionVerifier for NoOpAuth {
    fn verify_session(
        &self,
        _: &str,
        _: Option<u64>,
    ) -> Result<aetheris_server::auth::VerifiedSession, aetheris_server::auth::AuthError> {
        Ok(aetheris_server::auth::VerifiedSession {
            player_id: "user".to_string(),
            jti: "test".to_string(),
        })
    }
    fn is_session_authorized(&self, _: &str, _: Option<u64>) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_determinism_golden_replay() {
    // 1. Load golden file
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../golden_600ticks.bin");
    let golden_data =
        std::fs::read(path).expect("Missing golden_600ticks.bin. Run 'just record-golden' first.");

    let expected_hashes: Vec<u64> = golden_data
        .chunks_exact(8)
        .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()))
        .collect();

    assert_eq!(
        expected_hashes.len(),
        600,
        "Golden file should contain exactly 600 hashes"
    );

    // 2. Setup simulation environment (Strict Determinism)
    let tick_rate = 60;
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let mut world = BevyWorldAdapter::new(bevy_ecs::world::World::new(), tick_rate);
    world.setup_world();
    let mut registry = aetheris_ecs_bevy::registry::ComponentRegistry::new();
    aetheris_ecs_bevy::registry::register_void_rush_components(&mut registry);
    for descriptor in registry.components.values() {
        world.register_replicator(descriptor.replicator.clone());
    }

    let transport = Arc::new(RwLock::new(
        Box::new(NoOpTransport) as Box<dyn GameTransport>
    ));
    let encoder = SerdeEncoder::new();
    let mut scheduler =
        TickScheduler::new(tick_rate, Arc::new(NoOpAuth), pool).with_spawn_at_zero(true);

    // 3. Replay 600 ticks and compare hashes
    for (i, &expected) in expected_hashes.iter().enumerate() {
        scheduler.tick_step(&transport, &mut world, &encoder).await;
        let actual = world.state_hash();
        assert_eq!(
            actual, expected,
            "Determinism mismatch at tick {}! State diverged.",
            i
        );
    }

    println!("Determinism validation PASSED for 600 ticks.");
}

extern crate aetheris_server;
extern crate rmp_serde;

use aetheris_protocol::error::{EncodeError, TransportError};
use aetheris_protocol::events::{NetworkEvent, ReplicationEvent};
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};
use aetheris_protocol::types::{ClientId, ComponentKind, NetworkId};
use aetheris_server::tick::TickScheduler;
use async_trait::async_trait;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use tokio::sync::RwLock;

// --- Mocks ---

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
struct NoOpWorld;
impl WorldState for NoOpWorld {
    fn get_local_id(&self, _: NetworkId) -> Option<aetheris_protocol::types::LocalId> {
        None
    }
    fn get_network_id(&self, _: aetheris_protocol::types::LocalId) -> Option<NetworkId> {
        None
    }
    fn extract_deltas(&mut self) -> Vec<ReplicationEvent> {
        vec![]
    }
    fn apply_updates(&mut self, _: &[(ClientId, aetheris_protocol::events::ComponentUpdate)]) {}
    fn spawn_networked(&mut self) -> NetworkId {
        NetworkId(1)
    }
    fn despawn_networked(
        &mut self,
        _: NetworkId,
    ) -> Result<(), aetheris_protocol::error::WorldError> {
        Ok(())
    }
    fn state_hash(&self) -> u64 {
        0
    }
}

#[derive(Debug)]
struct NoOpEncoder;
impl Encoder for NoOpEncoder {
    fn codec_id(&self) -> u32 {
        0
    }
    fn encode(&self, _: &ReplicationEvent, _: &mut [u8]) -> Result<usize, EncodeError> {
        Ok(0)
    }
    fn decode(&self, _: &[u8]) -> Result<aetheris_protocol::events::ComponentUpdate, EncodeError> {
        Err(EncodeError::MalformedPayload {
            offset: 0,
            message: "noop".to_string(),
        })
    }
    fn encode_event(&self, _: &NetworkEvent) -> Result<Vec<u8>, EncodeError> {
        Ok(vec![])
    }
    fn encode_event_into(&self, _: &NetworkEvent, _: &mut [u8]) -> Result<usize, EncodeError> {
        Ok(0)
    }
    fn decode_event(&self, _: &[u8]) -> Result<NetworkEvent, EncodeError> {
        Err(EncodeError::MalformedPayload {
            offset: 0,
            message: "noop".to_string(),
        })
    }
    fn max_encoded_size(&self) -> usize {
        1024
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

// --- Benchmarks ---

fn bench_tick_scheduler(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );

    let transport = Arc::new(RwLock::new(
        Box::new(NoOpTransport) as Box<dyn GameTransport>
    ));
    let encoder = Arc::new(NoOpEncoder);

    c.bench_function("tick_scheduler_step_noop", |b| {
        b.to_async(&rt).iter_batched(
            || {
                let scheduler = TickScheduler::new(60, Arc::new(NoOpAuth), pool.clone());
                let world = NoOpWorld;
                (scheduler, world)
            },
            |(mut scheduler, mut world)| {
                let transport = transport.clone();
                let encoder = encoder.clone();
                async move {
                    black_box(scheduler.tick_step(
                        &transport,
                        &mut world,
                        &*encoder,
                    )).await
                }
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_encode_rmp_serde(c: &mut Criterion) {
    use aetheris_encoder_serde::SerdeEncoder;
    use aetheris_protocol::types::Transform;
    use alloc_counter::count_alloc;

    let encoder = SerdeEncoder::new();
    let event = ReplicationEvent {
        network_id: NetworkId(42),
        component_kind: ComponentKind(1), // Transform
        tick: 600,
        payload: rmp_serde::to_vec(&Transform {
            x: 10.0,
            y: 5.0,
            z: 0.0,
            rotation: 1.57,
            entity_type: 1,
        })
        .unwrap(),
    };
    let mut buffer = vec![0u8; encoder.max_encoded_size()];

    // Warmup
    for _ in 0..100 {
        let _ = encoder.encode(&event, &mut buffer);
    }

    c.bench_function("encode_rmp_serde_transform", |b| {
        b.iter(|| {
            let (allocs, _) =
                count_alloc(|| black_box(encoder.encode(&event, &mut buffer)).unwrap());
            if allocs.0 > 0 || allocs.1 > 0 || allocs.2 > 0 {
                panic!(
                    "SerdeEncoder::encode allocated {:?} times during hot path!",
                    allocs
                );
            }
        })
    });
}

criterion_group!(benches, bench_tick_scheduler, bench_encode_rmp_serde);
criterion_main!(benches);

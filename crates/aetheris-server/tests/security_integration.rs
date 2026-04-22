//! Security integration tests for the Aetheris Server.
//!
//! Verifies:
//! 1. Entity Hijacking prevention (Ownership checks).
//! 2. gRPC Control Plane message size limits.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aetheris_ecs_bevy::BevyWorldAdapter;
use aetheris_protocol::auth::v1::{OtpRequest, OtpRequestAck};
use aetheris_protocol::events::{ComponentUpdate, NetworkEvent, ReplicationEvent};
use aetheris_protocol::test_doubles::MockTransport;
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};
use aetheris_protocol::types::{ClientId, ComponentKind, NetworkId};
use aetheris_server::TickScheduler;
use aetheris_server::auth::AuthServiceImpl;
use bevy_ecs::prelude::{Component, World};
use tonic::{Response, Status};

#[derive(Component, Clone, Debug, PartialEq)]
struct MockPos(u32);

impl From<MockPos> for Vec<u8> {
    fn from(pos: MockPos) -> Self {
        pos.0.to_le_bytes().to_vec()
    }
}

impl TryFrom<Vec<u8>> for MockPos {
    type Error = ();
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value.len() == 4 {
            let bytes: [u8; 4] = value.try_into().unwrap();
            Ok(MockPos(u32::from_le_bytes(bytes)))
        } else {
            Err(())
        }
    }
}

#[tokio::test]
async fn test_entity_hijacking_prevention() {
    let _ = tracing_subscriber::fmt::try_init();
    let bevy_world = World::new();
    let mut adapter = BevyWorldAdapter::new(bevy_world, 100);
    adapter.register_replicator(std::sync::Arc::new(aetheris_ecs_bevy::DefaultReplicator::<
        MockPos,
    >::new(ComponentKind(1))));
    adapter.setup_world();

    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        encoder: Arc::new(aetheris_encoder_serde::SerdeEncoder::new()),
    };

    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let cid_a = ClientId(1);
    let cid_b = ClientId(2);

    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    let mut scheduler = TickScheduler::new(100, auth_service.clone());

    {
        let t = state.transport.lock().await;
        t.inject_event(NetworkEvent::ClientConnected(cid_a));
        t.inject_event(NetworkEvent::ClientConnected(cid_b));

        let (token_a, _) = auth_service.mint_session_token_for_test("user_a").unwrap();
        let (token_b, _) = auth_service.mint_session_token_for_test("user_b").unwrap();

        let serde_encoder = aetheris_encoder_serde::SerdeEncoder::new();

        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid_a,
            data: serde_encoder
                .encode_event(&NetworkEvent::Auth {
                    session_token: token_a,
                })
                .unwrap(),
        });
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid_b,
            data: serde_encoder
                .encode_event(&NetworkEvent::Auth {
                    session_token: token_b,
                })
                .unwrap(),
        });
    }

    struct RealWorldRef {
        adapter: Arc<Mutex<BevyWorldAdapter>>,
    }
    impl WorldState for RealWorldRef {
        fn get_local_id(&self, nid: NetworkId) -> Option<aetheris_protocol::types::LocalId> {
            self.adapter.lock().unwrap().get_local_id(nid)
        }
        fn get_network_id(&self, lid: aetheris_protocol::types::LocalId) -> Option<NetworkId> {
            self.adapter.lock().unwrap().get_network_id(lid)
        }
        fn extract_deltas(&mut self) -> Vec<ReplicationEvent> {
            self.adapter.lock().unwrap().extract_deltas()
        }
        fn apply_updates(&mut self, updates: &[(ClientId, ComponentUpdate)]) {
            self.adapter.lock().unwrap().apply_updates(updates)
        }
        fn extract_reliable_events(
            &mut self,
        ) -> Vec<(Option<ClientId>, aetheris_protocol::events::WireEvent)> {
            self.adapter.lock().unwrap().extract_reliable_events()
        }
        fn simulate(&mut self) {
            self.adapter.lock().unwrap().simulate()
        }
        fn spawn_networked(&mut self) -> NetworkId {
            self.adapter.lock().unwrap().spawn_networked()
        }
        fn spawn_networked_for(&mut self, cid: ClientId) -> NetworkId {
            self.adapter.lock().unwrap().spawn_networked_for(cid)
        }
        fn despawn_networked(
            &mut self,
            nid: NetworkId,
        ) -> Result<(), aetheris_protocol::error::WorldError> {
            self.adapter.lock().unwrap().despawn_networked(nid)
        }
        fn stress_test(&mut self, count: u16, rotate: bool) {
            self.adapter.lock().unwrap().stress_test(count, rotate);
        }
        fn spawn_kind(&mut self, kind: u16, x: f32, y: f32, rot: f32) -> NetworkId {
            self.adapter.lock().unwrap().spawn_kind(kind, x, y, rot)
        }
        fn spawn_kind_for(
            &mut self,
            kind: u16,
            x: f32,
            y: f32,
            rot: f32,
            client_id: ClientId,
        ) -> NetworkId {
            self.adapter
                .lock()
                .unwrap()
                .spawn_kind_for(kind, x, y, rot, client_id)
        }
        fn spawn_session_ship(
            &mut self,
            kind: u16,
            x: f32,
            y: f32,
            rot: f32,
            client_id: ClientId,
        ) -> NetworkId {
            self.adapter
                .lock()
                .unwrap()
                .spawn_session_ship(kind, x, y, rot, client_id)
        }
        fn clear_world(&mut self) {
            self.adapter.lock().unwrap().clear_world();
        }
    }

    let shared_adapter = Arc::new(Mutex::new(adapter));
    let loop_transport = Box::new(TransportRef(state.clone()));
    let loop_world = Box::new(RealWorldRef {
        adapter: shared_adapter.clone(),
    });
    let loop_encoder = Box::new(EncoderRef(state.clone()));

    let handle = tokio::spawn(async move {
        scheduler
            .run(loop_transport, loop_world, loop_encoder, shutdown_rx)
            .await;
    });

    // Wait for auth & spawn (be more generous and wait until entities exist)
    let nid_a = NetworkId(2);
    let nid_b = NetworkId(3);

    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let t = state.transport.lock().await;
        let serde_encoder = aetheris_encoder_serde::SerdeEncoder::new();
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid_a,
            data: serde_encoder
                .encode_event(&NetworkEvent::StartSession { client_id: cid_a })
                .unwrap(),
        });
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid_b,
            data: serde_encoder
                .encode_event(&NetworkEvent::StartSession { client_id: cid_b })
                .unwrap(),
        });
    }

    let mut attempts = 0;
    loop {
        {
            let adapter = shared_adapter.lock().unwrap();
            if adapter.get_local_id(nid_a).is_some() && adapter.get_local_id(nid_b).is_some() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        attempts += 1;
        if attempts > 20 {
            let adapter = shared_adapter.lock().unwrap();
            panic!(
                "Entities did not spawn in time! A: {:?}, B: {:?}",
                adapter.get_local_id(nid_a),
                adapter.get_local_id(nid_b)
            );
        }
    }

    // Client A owns Entity 1, Client B owns Entity 2.

    {
        let mut adapter = shared_adapter.lock().unwrap();
        let ent_b = adapter.get_local_id(nid_b).unwrap();
        let bevy_ent_b = bevy_ecs::entity::Entity::from_bits(ent_b.0);
        adapter
            .world_mut()
            .entity_mut(bevy_ent_b)
            .insert(MockPos(10));

        let ent_a = adapter.get_local_id(nid_a).unwrap();
        let bevy_ent_a = bevy_ecs::entity::Entity::from_bits(ent_a.0);
        adapter
            .world_mut()
            .entity_mut(bevy_ent_a)
            .insert(MockPos(0));
    }

    // Attempt: Client A tries to update Entity B (Owned by B)
    let mut buf = vec![0u8; 1200];
    let size = state
        .encoder
        .encode(
            &ReplicationEvent {
                network_id: nid_b,
                component_kind: ComponentKind(1),
                payload: vec![66, 0, 0, 0], // New pos
                tick: 10,
            },
            &mut buf,
        )
        .unwrap();

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::UnreliableMessage {
            client_id: cid_a,
            data: buf[..size].to_vec(),
        });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify: Entity B should STILL have pos 10, NOT 66
    {
        let adapter = shared_adapter.lock().unwrap();
        let ent_b = adapter.get_local_id(nid_b).unwrap();
        let bevy_ent_b = bevy_ecs::entity::Entity::from_bits(ent_b.0);
        let pos = adapter.world().get::<MockPos>(bevy_ent_b).unwrap();
        assert_eq!(
            pos.0, 10,
            "Security Failure: Client A updated Client B's entity!"
        );
    }

    // Success: Token A updating Entity A should work
    let size = state
        .encoder
        .encode(
            &ReplicationEvent {
                network_id: nid_a,
                component_kind: ComponentKind(1),
                payload: vec![100, 0, 0, 0],
                tick: 11,
            },
            &mut buf,
        )
        .unwrap();

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::UnreliableMessage {
            client_id: cid_a,
            data: buf[..size].to_vec(),
        });

    tokio::time::sleep(Duration::from_millis(200)).await;

    {
        let adapter = shared_adapter.lock().unwrap();
        let ent_a = adapter.get_local_id(nid_a).unwrap();
        let bevy_ent_a = bevy_ecs::entity::Entity::from_bits(ent_a.0);
        let pos = adapter.world().get::<MockPos>(bevy_ent_a).unwrap();
        assert_eq!(pos.0, 100, "Update from owner should have been applied");
    }

    handle.abort();
}

#[tokio::test]
async fn test_grpc_message_size_limit() -> Result<(), Box<dyn std::error::Error>> {
    use aetheris_protocol::auth::v1::auth_service_client::AuthServiceClient;
    use aetheris_protocol::auth::v1::auth_service_server::AuthServiceServer;
    use std::net::SocketAddr;

    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    let addr: SocketAddr = "127.0.0.1:0".parse()?;
    let listener = std::net::TcpListener::bind(addr)?;
    let addr = listener.local_addr()?;
    drop(listener);

    let grpc_auth_service = auth_service.clone();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AuthServiceServer::new(grpc_auth_service).max_decoding_message_size(4096))
            .serve(addr)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let endpoint = format!("http://{}", addr);
    let mut channel = None;
    for _ in 0..10 {
        if let Ok(c) = tonic::transport::Channel::from_shared(endpoint.clone())?
            .connect()
            .await
        {
            channel = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let channel = channel.expect("Failed to connect to gRPC server after retries");
    let mut client = AuthServiceClient::new(channel);

    // Create an oversized request (email > 4KB, using 8KB for clear overflow)
    let large_email = "a".repeat(8192);
    let request = tonic::Request::new(OtpRequest { email: large_email });

    let result: Result<Response<OtpRequestAck>, Status> = client.request_otp(request).await;

    assert!(result.is_err());
    let code = result.unwrap_err().code();
    // Tonic returns ResourceExhausted or OutOfRange for message size limits
    assert!(
        code == tonic::Code::ResourceExhausted || code == tonic::Code::OutOfRange,
        "Expected ResourceExhausted or OutOfRange, got {:?}",
        code
    );

    Ok(())
}

// Boilerplate for Mocking
#[derive(Clone)]
struct SharedState {
    transport: Arc<tokio::sync::Mutex<MockTransport>>,
    encoder: Arc<dyn Encoder>,
}

struct TransportRef(SharedState);
#[async_trait::async_trait]
impl GameTransport for TransportRef {
    async fn send_unreliable(
        &self,
        id: ClientId,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        self.0
            .transport
            .lock()
            .await
            .send_unreliable(id, data)
            .await
    }
    async fn send_reliable(
        &self,
        id: ClientId,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        self.0.transport.lock().await.send_reliable(id, data).await
    }
    async fn broadcast_unreliable(
        &self,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        self.0
            .transport
            .lock()
            .await
            .broadcast_unreliable(data)
            .await
    }
    async fn poll_events(
        &mut self,
    ) -> Result<Vec<NetworkEvent>, aetheris_protocol::error::TransportError> {
        Ok(self.0.transport.lock().await.poll_events().await?)
    }
    async fn connected_client_count(&self) -> usize {
        self.0.transport.lock().await.connected_client_count().await
    }
}

struct EncoderRef(SharedState);
impl Encoder for EncoderRef {
    fn codec_id(&self) -> u32 {
        1
    }

    fn encode(
        &self,
        ev: &ReplicationEvent,
        buf: &mut [u8],
    ) -> Result<usize, aetheris_protocol::error::EncodeError> {
        self.0.encoder.encode(ev, buf)
    }
    fn decode(&self, buf: &[u8]) -> Result<ComponentUpdate, aetheris_protocol::error::EncodeError> {
        self.0.encoder.decode(buf)
    }
    fn encode_event(
        &self,
        ev: &NetworkEvent,
    ) -> Result<Vec<u8>, aetheris_protocol::error::EncodeError> {
        let serde_encoder = aetheris_encoder_serde::SerdeEncoder::new();
        serde_encoder.encode_event(ev)
    }
    fn decode_event(
        &self,
        data: &[u8],
    ) -> Result<NetworkEvent, aetheris_protocol::error::EncodeError> {
        let serde_encoder = aetheris_encoder_serde::SerdeEncoder::new();
        serde_encoder.decode_event(data)
    }
    fn max_encoded_size(&self) -> usize {
        self.0.encoder.max_encoded_size()
    }
}

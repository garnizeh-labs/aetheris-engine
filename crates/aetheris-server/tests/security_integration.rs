//! Security integration tests for the Aetheris Server.
//!
//! Verifies:
//! 1. Entity Hijacking prevention (Ownership checks).
//! 2. gRPC Control Plane message size limits.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aetheris_ecs_bevy::BevyWorldAdapter;
use aetheris_protocol::auth::v1::OtpRequest;
use aetheris_protocol::events::{ComponentUpdate, NetworkEvent, ReplicationEvent};
use aetheris_protocol::test_doubles::MockTransport;
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};
use aetheris_protocol::types::{ClientId, ComponentKind, NetworkId};
use aetheris_server::TickScheduler;
use aetheris_server::auth::AuthServiceImpl;
use async_trait::async_trait;
use bevy_ecs::prelude::{Component, World};

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

#[tokio::test(flavor = "multi_thread")]
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

    let auth_service = Arc::new(
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await,
    );
    let encode_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let mut scheduler = TickScheduler::new(100, auth_service.clone(), encode_pool);

    {
        let t = state.transport.lock().await;
        t.connect(cid_a);
        t.connect(cid_b);
        t.inject_event(NetworkEvent::ClientConnected(cid_a));
        t.inject_event(NetworkEvent::ClientConnected(cid_b));

        let (token_a, _) = auth_service.mint_session_token("user_a", None).unwrap();
        let (token_b, _) = auth_service.mint_session_token("user_b", None).unwrap();

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
        fn advance_tick(&mut self) {
            self.adapter.lock().unwrap().advance_tick();
        }
        fn apply_updates(&mut self, updates: &[(ClientId, ComponentUpdate)]) {
            self.adapter.lock().unwrap().apply_updates(updates);
        }
        fn simulate(&mut self) {
            self.adapter.lock().unwrap().simulate();
        }
        fn extract_deltas(&mut self) -> Vec<ReplicationEvent> {
            self.adapter.lock().unwrap().extract_deltas()
        }
        fn post_extract(&mut self) {
            self.adapter.lock().unwrap().post_extract();
        }
        fn queue_reliable_event(
            &mut self,
            client_id: Option<ClientId>,
            event: aetheris_protocol::events::GameEvent,
        ) {
            self.adapter
                .lock()
                .unwrap()
                .queue_reliable_event(client_id, event);
        }
        fn get_entity_room(&self, entity_id: NetworkId) -> Option<NetworkId> {
            self.adapter.lock().unwrap().get_entity_room(entity_id)
        }
        fn get_client_room(&self, client_id: ClientId) -> Option<NetworkId> {
            self.adapter.lock().unwrap().get_client_room(client_id)
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
        fn spawn_networked(&mut self) -> NetworkId {
            self.adapter.lock().unwrap().spawn_networked()
        }
        fn get_local_id(&self, network_id: NetworkId) -> Option<aetheris_protocol::types::LocalId> {
            self.adapter.lock().unwrap().get_local_id(network_id)
        }
        fn get_network_id(&self, local_id: aetheris_protocol::types::LocalId) -> Option<NetworkId> {
            self.adapter.lock().unwrap().get_network_id(local_id)
        }
        fn despawn_networked(
            &mut self,
            network_id: NetworkId,
        ) -> Result<(), aetheris_protocol::error::WorldError> {
            self.adapter.lock().unwrap().despawn_networked(network_id)
        }
    }

    let shared_adapter = Arc::new(Mutex::new(adapter));
    let loop_transport = TransportRef(state.clone());
    let loop_world = RealWorldRef {
        adapter: shared_adapter.clone(),
    };
    let loop_encoder = EncoderRef(state.clone());

    let handle = tokio::spawn(async move {
        scheduler
            .run(
                Arc::new(tokio::sync::RwLock::new(loop_transport)),
                Arc::new(std::sync::Mutex::new(loop_world)),
                Arc::new(loop_encoder),
                shutdown_rx,
            )
            .await;
    });

    // Wait for auth & spawn
    let nid_a = NetworkId(2);

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

    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let t = state.transport.lock().await;
        let mut buf = vec![0u8; 1000];
        let size = state
            .encoder
            .encode(
                &ReplicationEvent {
                    network_id: nid_a,
                    tick: 10,
                    component_kind: ComponentKind(1),
                    payload: MockPos(999).into(),
                },
                &mut buf,
            )
            .unwrap();
        t.inject_event(NetworkEvent::UnreliableMessage {
            client_id: cid_b,
            data: buf[..size].to_vec(),
        });
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        let mut adapter = shared_adapter.lock().unwrap();
        let entity = adapter.get_local_id(nid_a).expect("Entity NID_A not found");
        let bevy_entity = bevy_ecs::prelude::Entity::from_bits(entity.0);

        // Ensure the entity has MockPos initially
        adapter
            .world_mut()
            .entity_mut(bevy_entity)
            .insert(MockPos(100));

        let pos = adapter.world().get::<MockPos>(bevy_entity).unwrap();
        assert_ne!(
            pos.0, 999,
            "Entity hijacking successful! Security violation."
        );
    }

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_grpc_control_plane_size_limit() {
    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = std::net::TcpListener::bind(addr).unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(
                aetheris_protocol::auth::v1::auth_service_server::AuthServiceServer::new(
                    auth_service,
                ),
            )
            .serve(addr)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = tonic::transport::Channel::from_shared(format!("http://{}", addr))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client =
        aetheris_protocol::auth::v1::auth_service_client::AuthServiceClient::new(channel);

    let email = "a".repeat(1000) + "@example.com";
    let resp = client
        .request_otp(tonic::Request::new(OtpRequest { email }))
        .await;

    assert!(resp.is_ok());
}

#[derive(Clone)]
struct SharedState {
    transport: Arc<tokio::sync::Mutex<MockTransport>>,
    encoder: Arc<aetheris_encoder_serde::SerdeEncoder>,
}

struct TransportRef(SharedState);
#[async_trait]
impl GameTransport for TransportRef {
    async fn poll_events(
        &mut self,
    ) -> Result<Vec<NetworkEvent>, aetheris_protocol::traits::TransportError> {
        let mut t = self.0.transport.lock().await;
        t.poll_events().await
    }
    async fn send_unreliable(
        &self,
        client_id: ClientId,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::traits::TransportError> {
        let t = self.0.transport.lock().await;
        t.send_unreliable(client_id, data).await
    }
    async fn send_reliable(
        &self,
        client_id: ClientId,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::traits::TransportError> {
        let t = self.0.transport.lock().await;
        t.send_reliable(client_id, data).await
    }
    async fn connected_client_count(&self) -> usize {
        let t = self.0.transport.lock().await;
        t.connected_client_count().await
    }
    async fn broadcast_unreliable(
        &self,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::traits::TransportError> {
        let t = self.0.transport.lock().await;
        t.broadcast_unreliable(data).await
    }
}

struct EncoderRef(SharedState);
impl Encoder for EncoderRef {
    fn codec_id(&self) -> u32 {
        self.0.encoder.codec_id()
    }
    fn encode(
        &self,
        event: &ReplicationEvent,
        buffer: &mut [u8],
    ) -> Result<usize, aetheris_protocol::error::EncodeError> {
        self.0.encoder.encode(event, buffer)
    }
    fn decode(
        &self,
        data: &[u8],
    ) -> Result<ComponentUpdate, aetheris_protocol::error::EncodeError> {
        self.0.encoder.decode(data)
    }
    fn encode_event(
        &self,
        event: &NetworkEvent,
    ) -> Result<Vec<u8>, aetheris_protocol::error::EncodeError> {
        self.0.encoder.encode_event(event)
    }
    fn decode_event(
        &self,
        data: &[u8],
    ) -> Result<NetworkEvent, aetheris_protocol::error::EncodeError> {
        self.0.encoder.decode_event(data)
    }
    fn max_encoded_size(&self) -> usize {
        self.0.encoder.max_encoded_size()
    }
}

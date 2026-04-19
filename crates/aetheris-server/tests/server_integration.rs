//! Integration tests for the Aetheris Server loop.

use aetheris_ecs_bevy::BevyWorldAdapter;
use bevy_ecs::prelude::World;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aetheris_protocol::auth::v1::auth_service_client::AuthServiceClient;
use aetheris_protocol::auth::v1::auth_service_server::AuthServiceServer;
use aetheris_protocol::auth::v1::*;
use aetheris_protocol::events::{ComponentUpdate, NetworkEvent, ReplicationEvent};
use aetheris_protocol::test_doubles::{
    MockEncoder, MockEncoder as ME, MockTransport, MockWorldState,
};
use aetheris_protocol::traits::{Encoder, GameTransport, WorldError, WorldState};
use aetheris_protocol::types::{ClientId, ComponentKind, NetworkId};
use aetheris_server::TickScheduler;
use aetheris_server::auth::AuthServiceImpl;
use aetheris_server::auth::email::EmailSender;
use tonic::transport::Channel;
use tonic::{Response, Status};

#[tokio::test]
async fn test_grpc_auth_flow() -> Result<(), Box<dyn std::error::Error>> {
    use std::net::SocketAddr;
    use tonic::transport::Server;

    #[derive(Default, Clone)]
    struct MockEmailSender {
        last_code: Arc<Mutex<Option<String>>>,
    }

    #[async_trait::async_trait]
    impl EmailSender for MockEmailSender {
        async fn send(
            &self,
            _to: &str,
            _subject: &str,
            plaintext: &str,
            _html: &str,
        ) -> Result<(), String> {
            // Body: "Code: 123456"
            // Extract the first 6-digit sequence found in the plaintext
            if let Some(code) = plaintext
                .as_bytes()
                .windows(6)
                .find(|w| w.iter().all(u8::is_ascii_digit))
                .map(|w| String::from_utf8_lossy(w).into_owned())
            {
                let mut lock = self.last_code.lock().unwrap();
                *lock = Some(code);
            }
            Ok(())
        }
    }

    let email_sender = Arc::new(MockEmailSender::default());
    let auth_service = AuthServiceImpl::new(email_sender.clone()).await;
    let addr: SocketAddr = "127.0.0.1:0".parse()?;
    let listener = std::net::TcpListener::bind(addr)?;
    let addr = listener.local_addr()?;
    drop(listener);

    let grpc_auth_service = auth_service.clone();
    tokio::spawn(async move {
        Server::builder()
            .add_service(AuthServiceServer::new(grpc_auth_service))
            .serve(addr)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let endpoint = format!("http://{}", addr);
    let channel = Channel::from_shared(endpoint)?.connect().await?;
    let mut client = AuthServiceClient::new(channel);

    // Flow: Request OTP -> Get Code -> Login
    let email = "test@example.com";
    let otp_resp: Response<OtpRequestAck> = client
        .request_otp(tonic::Request::new(OtpRequest {
            email: email.to_string(),
        }))
        .await?;
    let otp_ack = otp_resp.into_inner();

    let code = {
        let mut attempts = 0;
        loop {
            if let Some(c) = email_sender.last_code.lock().unwrap().clone() {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            attempts += 1;
            if attempts > 20 {
                panic!("OTP code never received by MockEmailSender");
            }
        }
    };

    let login_resp_raw: Response<LoginResponse> = client
        .login(tonic::Request::new(LoginRequest {
            method: Some(login_request::Method::Otp(OtpLoginRequest {
                request_id: otp_ack.request_id,
                code,
            })),
            metadata: Some(ClientMetadata {
                client_version: "0.1.0".to_string(),
                platform: "test".to_string(),
            }),
        }))
        .await?;
    let login_resp = login_resp_raw.into_inner();

    let token = login_resp.session_token;
    assert!(!token.is_empty(), "Token should not be empty");
    assert!(
        auth_service.is_authorized(&token),
        "Token should be authorized in service"
    );

    // Negative case: Invalid code
    let result: Result<Response<LoginResponse>, Status> = client
        .login(tonic::Request::new(LoginRequest {
            method: Some(login_request::Method::Otp(OtpLoginRequest {
                request_id: "wrong-id".to_string(),
                code: "000000".to_string(),
            })),
            metadata: None,
        }))
        .await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);

    Ok(())
}

#[tokio::test]
async fn test_server_loop_1000_ticks() {
    let transport = Box::new(MockTransport::new());
    let world = Box::new(MockWorldState::new());
    let mut adapter = BevyWorldAdapter::new(World::new());
    adapter.register_replicator(std::sync::Arc::new(aetheris_ecs_bevy::DefaultReplicator::<
        aetheris_ecs_bevy::Transform,
    >::new(
        aetheris_protocol::types::ComponentKind(1),
    )));
    let encoder = Box::new(MockEncoder::new());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

    let tick_rate = 1000;
    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    let mut scheduler = TickScheduler::new(tick_rate, auth_service);

    let handle = tokio::spawn(async move {
        scheduler.run(transport, world, encoder, shutdown_rx).await;
    });

    tokio::time::sleep(Duration::from_millis(1500)).await;
    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => panic!("Scheduler task panicked: {e:?}"),
    }
}

#[derive(Clone)]
struct SharedState {
    transport: Arc<tokio::sync::Mutex<MockTransport>>,
    world: Arc<Mutex<MockWorldState>>,
    encoder: Arc<MockEncoder>,
}

struct TransportRef(SharedState);

#[async_trait::async_trait]
impl GameTransport for TransportRef {
    async fn send_unreliable(
        &self,
        id: ClientId,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        let t = self.0.transport.lock().await;
        t.send_unreliable(id, data).await
    }
    async fn send_reliable(
        &self,
        id: ClientId,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        let t = self.0.transport.lock().await;
        t.send_reliable(id, data).await
    }
    async fn broadcast_unreliable(
        &self,
        data: &[u8],
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        let t = self.0.transport.lock().await;
        t.broadcast_unreliable(data).await
    }
    async fn poll_events(
        &mut self,
    ) -> Result<Vec<NetworkEvent>, aetheris_protocol::error::TransportError> {
        let mut t = self.0.transport.lock().await;
        t.poll_events().await
    }
    async fn connected_client_count(&self) -> usize {
        let t = self.0.transport.lock().await;
        t.connected_client_count().await
    }
}

struct WorldRef(SharedState);
impl WorldState for WorldRef {
    fn get_local_id(&self, nid: NetworkId) -> Option<aetheris_protocol::types::LocalId> {
        self.0.world.lock().unwrap().get_local_id(nid)
    }
    fn get_network_id(&self, lid: aetheris_protocol::types::LocalId) -> Option<NetworkId> {
        self.0.world.lock().unwrap().get_network_id(lid)
    }
    fn extract_deltas(&mut self) -> Vec<ReplicationEvent> {
        self.0.world.lock().unwrap().extract_deltas()
    }
    fn apply_updates(&mut self, updates: &[(ClientId, ComponentUpdate)]) {
        self.0.world.lock().unwrap().apply_updates(updates)
    }
    fn simulate(&mut self) {
        self.0.world.lock().unwrap().simulate()
    }
    fn spawn_networked(&mut self) -> NetworkId {
        self.0.world.lock().unwrap().spawn_networked()
    }
    fn spawn_networked_for(&mut self, client_id: ClientId) -> NetworkId {
        self.0.world.lock().unwrap().spawn_networked_for(client_id)
    }
    fn despawn_networked(&mut self, network_id: NetworkId) -> Result<(), WorldError> {
        self.0.world.lock().unwrap().despawn_networked(network_id)
    }
    fn stress_test(&mut self, count: u16, rotate: bool) {
        self.0.world.lock().unwrap().stress_test(count, rotate);
    }
    fn spawn_kind(&mut self, kind: u16, x: f32, y: f32, rot: f32) -> NetworkId {
        self.0.world.lock().unwrap().spawn_kind(kind, x, y, rot)
    }
    fn clear_world(&mut self) {
        self.0.world.lock().unwrap().clear_world();
    }
}

struct EncoderRef(SharedState);
impl Encoder for EncoderRef {
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
        self.0.encoder.encode_event(ev)
    }
    fn decode_event(
        &self,
        data: &[u8],
    ) -> Result<NetworkEvent, aetheris_protocol::error::EncodeError> {
        if let Ok(ev) = self.0.encoder.decode_event(data) {
            Ok(ev)
        } else {
            let serde_encoder = aetheris_encoder_serde::SerdeEncoder::new();
            serde_encoder.decode_event(data)
        }
    }
    fn max_encoded_size(&self) -> usize {
        self.0.encoder.max_encoded_size()
    }
}

async fn inject_auth_handshake(
    transport: &Arc<tokio::sync::Mutex<MockTransport>>,
    client_id: ClientId,
    auth_service: &AuthServiceImpl,
) {
    let player_id = "test-player";
    let (session_token, _) = auth_service.mint_session_token_for_test(player_id).unwrap();

    let serde_encoder = aetheris_encoder_serde::SerdeEncoder::new();
    let auth_packet = serde_encoder
        .encode_event(&NetworkEvent::Auth { session_token })
        .unwrap();

    transport
        .lock()
        .await
        .inject_event(NetworkEvent::ReliableMessage {
            client_id,
            data: auth_packet,
        });
}

#[tokio::test]
async fn test_client_connect_and_replication() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(MockEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let cid = ClientId(1);

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::ClientConnected(cid));
    state.transport.lock().await.connect(cid);
    state
        .transport
        .lock()
        .await
        .per_client_unreliable
        .lock()
        .unwrap()
        .insert(cid, Vec::new());

    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    inject_auth_handshake(&state.transport, cid, &auth_service).await;

    // Spawn an entity to trigger replication
    {
        let mut w = state.world.lock().unwrap();
        let nid = w.spawn_networked();
        w.queue_delta(ReplicationEvent {
            network_id: nid,
            component_kind: ComponentKind(101),
            payload: vec![1, 2, 3],
            tick: 1,
        });
    }

    let mut scheduler = TickScheduler::new(100, auth_service);
    let loop_transport = Box::new(TransportRef(state.clone()));
    let loop_world = Box::new(WorldRef(state.clone()));
    let loop_encoder = Box::new(EncoderRef(state.clone()));

    let handle = tokio::spawn(async move {
        scheduler
            .run(loop_transport, loop_world, loop_encoder, shutdown_rx)
            .await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    let packets = state.transport.lock().await.take_unreliable(cid);
    assert!(
        !packets.is_empty(),
        "Expected broadcast packets to have been received by client"
    );
    let p = &packets[0];
    assert_eq!(p[0], ME::MOCK_SENTINEL);
    // New MockEncoder format: Header(19) + Payload
    assert_eq!(&p[19..], &[1, 2, 3]);

    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => panic!("Scheduler task panicked: {e:?}"),
    }
}

#[tokio::test]
async fn test_full_integration_suite() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(MockEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let cid = ClientId(1);

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::ClientConnected(cid));
    state.transport.lock().await.connect(cid);
    state
        .transport
        .lock()
        .await
        .per_client_unreliable
        .lock()
        .unwrap()
        .insert(cid, Vec::new());

    // Security: Satisfy mandatory auth handshake
    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    inject_auth_handshake(&state.transport, cid, &auth_service).await;

    let nid = state.world.lock().unwrap().spawn_networked();

    let mut scheduler = TickScheduler::new(100, auth_service);
    let loop_transport = Box::new(TransportRef(state.clone()));
    let loop_world = Box::new(WorldRef(state.clone()));
    let loop_encoder = Box::new(EncoderRef(state.clone()));

    let handle = tokio::spawn(async move {
        scheduler
            .run(loop_transport, loop_world, loop_encoder, shutdown_rx)
            .await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    state.world.lock().unwrap().despawn_networked(nid).unwrap();
    assert!(state.world.lock().unwrap().get_local_id(nid).is_none());

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::ReliableMessage {
            client_id: cid,
            data: vec![ME::MOCK_SENTINEL, 0xAA, 0xBB],
        });

    tokio::time::sleep(Duration::from_millis(200)).await;

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::UnreliableMessage {
            client_id: cid,
            data: vec![0x00, 0x00],
        });

    tokio::time::sleep(Duration::from_millis(200)).await;

    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => panic!("Scheduler task panicked: {e:?}"),
    }
}

#[tokio::test]
async fn test_consecutive_dropped_packets_interpolation() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(MockEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let cid = ClientId(1);

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::ClientConnected(cid));
    state.transport.lock().await.connect(cid);
    state
        .transport
        .lock()
        .await
        .per_client_unreliable
        .lock()
        .unwrap()
        .insert(cid, Vec::new());

    // Security: Satisfy mandatory auth handshake
    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    inject_auth_handshake(&state.transport, cid, &auth_service).await;

    let nid = state.world.lock().unwrap().spawn_networked();

    let mut scheduler = TickScheduler::new(100, auth_service); // 10ms ticks
    let loop_transport = Box::new(TransportRef(state.clone()));
    let loop_world = Box::new(WorldRef(state.clone()));
    let loop_encoder = Box::new(EncoderRef(state.clone()));

    let handle = tokio::spawn(async move {
        scheduler
            .run(loop_transport, loop_world, loop_encoder, shutdown_rx)
            .await;
    });

    // Simulate 10 ticks of state updates
    for i in 1..=10 {
        state.world.lock().unwrap().queue_delta(ReplicationEvent {
            network_id: nid,
            component_kind: ComponentKind(1), // Use a dummy component kind
            payload: vec![i as u8],           // Store the tick value in payload for verification
            tick: i,
        });
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    let all_packets = state.transport.lock().await.take_unreliable(cid);

    // Ensure we have at least 10 packets
    assert!(
        all_packets.len() >= 10,
        "Expected at least 10 packets, got {}",
        all_packets.len()
    );

    let tick1_packet = &all_packets[0];
    assert_eq!(tick1_packet[0], ME::MOCK_SENTINEL);
    // LSB of tick 1 is at index 11.
    assert_eq!(tick1_packet[11], 1, "First packet should be tick 1");

    let tick7_packet = &all_packets[6];
    assert_eq!(tick7_packet[0], ME::MOCK_SENTINEL);
    assert_eq!(tick7_packet[11], 7, "Seventh packet should be tick 7");

    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => panic!("Scheduler task panicked: {e:?}"),
    }
}

#[tokio::test]
async fn test_wasm_mtu_handling_simulation() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(MockEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let cid = ClientId(1);

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::ClientConnected(cid));
    state
        .transport
        .lock()
        .await
        .per_client_unreliable
        .lock()
        .unwrap()
        .insert(cid, Vec::new());

    // Security: Satisfy mandatory auth handshake
    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    inject_auth_handshake(&state.transport, cid, &auth_service).await;

    let mut scheduler = TickScheduler::new(100, auth_service);
    let loop_transport = Box::new(TransportRef(state.clone()));
    let loop_world = Box::new(WorldRef(state.clone()));
    let loop_encoder = Box::new(EncoderRef(state.clone()));

    let handle = tokio::spawn(async move {
        scheduler
            .run(loop_transport, loop_world, loop_encoder, shutdown_rx)
            .await;
    });

    let large_data = vec![0u8; aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE + 1];
    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::UnreliableMessage {
            client_id: cid,
            data: large_data.clone(),
        });

    tokio::time::sleep(Duration::from_millis(200)).await;

    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => panic!("Scheduler task panicked: {e:?}"),
    }
}

#[tokio::test]
async fn test_large_delta_fragmentation() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(MockEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let cid = ClientId(1);

    state
        .transport
        .lock()
        .await
        .inject_event(NetworkEvent::ClientConnected(cid));
    state.transport.lock().await.connect(cid);
    state
        .transport
        .lock()
        .await
        .per_client_unreliable
        .lock()
        .unwrap()
        .insert(cid, Vec::new());

    let auth_service =
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await;
    inject_auth_handshake(&state.transport, cid, &auth_service).await;

    // Use a real encoder to test actual fragmentation logic
    let real_encoder = Arc::new(aetheris_encoder_serde::SerdeEncoder::new());

    // Create an entity with a large payload (exceeding 1200 byte MTU)
    let nid = state.world.lock().unwrap().spawn_networked();
    let large_payload = vec![0xAA; 3000]; // 3000 bytes > 1200 MTU

    // We use a custom encoder ref that delegates to real_encoder for some parts
    struct LargeEncoderRef {
        real: Arc<aetheris_encoder_serde::SerdeEncoder>,
    }
    impl Encoder for LargeEncoderRef {
        fn encode(
            &self,
            ev: &ReplicationEvent,
            buf: &mut [u8],
        ) -> Result<usize, aetheris_protocol::error::EncodeError> {
            self.real.encode(ev, buf)
        }
        fn decode(
            &self,
            buf: &[u8],
        ) -> Result<ComponentUpdate, aetheris_protocol::error::EncodeError> {
            self.real.decode(buf)
        }
        fn encode_event(
            &self,
            ev: &NetworkEvent,
        ) -> Result<Vec<u8>, aetheris_protocol::error::EncodeError> {
            self.real.encode_event(ev)
        }
        fn decode_event(
            &self,
            data: &[u8],
        ) -> Result<NetworkEvent, aetheris_protocol::error::EncodeError> {
            self.real.decode_event(data)
        }
        fn max_encoded_size(&self) -> usize {
            1200 // Threshold for fragmentation
        }
    }

    {
        let w = state.world.lock().unwrap();
        w.queue_delta(ReplicationEvent {
            network_id: nid,
            component_kind: ComponentKind(99),
            payload: large_payload.clone(),
            tick: 1,
        });
    }

    let mut scheduler = TickScheduler::new(100, auth_service);
    let loop_transport = Box::new(TransportRef(state.clone()));
    let loop_world = Box::new(WorldRef(state.clone()));
    let loop_encoder = Box::new(LargeEncoderRef {
        real: real_encoder.clone(),
    });

    let handle = tokio::spawn(async move {
        scheduler
            .run(loop_transport, loop_world, loop_encoder, shutdown_rx)
            .await;
    });

    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify fragments were sent
    let packets = state.transport.lock().await.take_unreliable(cid);

    // 3000 bytes / (1200 - 64) ~= 3 fragments
    assert!(
        packets.len() >= 3,
        "Expected at least 3 fragments, got {}",
        packets.len()
    );

    // Inject fragments back in random order to simulate network jitter and verify reassembly
    use rand::seq::SliceRandom;
    let mut packets = packets;
    let mut rng = rand::rng();
    packets.shuffle(&mut rng);

    for packet in packets {
        state
            .transport
            .lock()
            .await
            .inject_event(NetworkEvent::UnreliableMessage {
                client_id: cid,
                data: packet,
            });
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the world applied the reassembled update
    // In our MockWorldState, apply_updates just records them
    let applied = state
        .world
        .lock()
        .unwrap()
        .applied_updates
        .lock()
        .unwrap()
        .clone();
    let found = applied.iter().any(|(id, update)| {
        *id == cid && update.payload == large_payload && update.component_kind == ComponentKind(99)
    });

    assert!(found, "Reassembled delta was not applied to the world");

    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => panic!("Scheduler task panicked: {e:?}"),
    }
}

//! Integration tests for the Aetheris Server loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aetheris_protocol::auth::v1::auth_service_client::AuthServiceClient;
use aetheris_protocol::auth::v1::auth_service_server::AuthServiceServer;
use aetheris_protocol::auth::v1::*;
use aetheris_protocol::events::{NetworkEvent, ReplicationEvent};
use aetheris_protocol::test_doubles::{MockTransport, MockWorldState};
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};
use aetheris_protocol::types::{ClientId, ComponentKind, NetworkId};
use aetheris_server::TickScheduler;
use aetheris_server::auth::AuthServiceImpl;
use aetheris_server::auth::email::EmailSender;
use async_trait::async_trait;
use tonic::Response;
use tonic::transport::Channel;

#[tokio::test(flavor = "multi_thread")]
async fn test_grpc_auth_flow() -> Result<(), Box<dyn std::error::Error>> {
    use std::net::SocketAddr;
    use tonic::transport::Server;

    #[derive(Default, Clone)]
    struct MockEmailSender {
        last_code: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl EmailSender for MockEmailSender {
        async fn send(
            &self,
            _to: &str,
            _subject: &str,
            plaintext: &str,
            _html: &str,
        ) -> Result<(), String> {
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

    let email = "test@example.com";
    let otp_resp: Response<OtpRequestAck> = client
        .request_otp(tonic::Request::new(OtpRequest {
            email: email.to_string(),
        }))
        .await?;

    let otp_ack = otp_resp.into_inner();
    let request_id = otp_ack.request_id;

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

    let login_resp: Response<LoginResponse> = client
        .login(tonic::Request::new(LoginRequest {
            method: Some(login_request::Method::Otp(OtpLoginRequest {
                request_id,
                code,
            })),
            metadata: None,
        }))
        .await?;

    let login_data = login_resp.into_inner();
    assert!(!login_data.session_token.is_empty());
    assert!(!login_data.player_id.is_empty());

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_server_loop_1000_ticks() {
    let transport = Arc::new(tokio::sync::RwLock::new(MockTransport::new()));
    let world = Arc::new(std::sync::Mutex::new(MockWorldState::new()));
    let encoder = Arc::new(aetheris_encoder_serde::SerdeEncoder::new());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

    let tick_rate = 1000;
    let auth_service = Arc::new(
        AuthServiceImpl::new(Arc::new(aetheris_server::auth::email::LogEmailSender)).await,
    );
    let encode_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let mut scheduler = TickScheduler::new(tick_rate, auth_service, encode_pool);

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

struct WorldRef {
    state: SharedState,
}
impl WorldState for WorldRef {
    fn advance_tick(&mut self) {
        self.state.world.lock().unwrap().advance_tick();
    }
    fn apply_updates(
        &mut self,
        updates: &[(ClientId, aetheris_protocol::events::ComponentUpdate)],
    ) {
        self.state.world.lock().unwrap().apply_updates(updates);
    }
    fn simulate(&mut self) {
        self.state.world.lock().unwrap().simulate();
    }
    fn extract_deltas(&mut self) -> Vec<aetheris_protocol::events::ReplicationEvent> {
        self.state.world.lock().unwrap().extract_deltas()
    }
    fn post_extract(&mut self) {
        self.state.world.lock().unwrap().post_extract();
    }
    fn extract_reliable_events(
        &mut self,
    ) -> Vec<(Option<ClientId>, aetheris_protocol::events::WireEvent)> {
        self.state.world.lock().unwrap().extract_reliable_events()
    }
    fn queue_reliable_event(
        &mut self,
        client_id: Option<ClientId>,
        event: aetheris_protocol::events::GameEvent,
    ) {
        self.state
            .world
            .lock()
            .unwrap()
            .queue_reliable_event(client_id, event);
    }
    fn spawn_networked(&mut self) -> NetworkId {
        self.state.world.lock().unwrap().spawn_networked()
    }
    fn spawn_kind_for(
        &mut self,
        kind: u16,
        x: f32,
        y: f32,
        rot: f32,
        client_id: ClientId,
    ) -> NetworkId {
        self.state
            .world
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
        self.state
            .world
            .lock()
            .unwrap()
            .spawn_session_ship(kind, x, y, rot, client_id)
    }
    fn clear_world(&mut self) {
        self.state.world.lock().unwrap().clear_world();
    }
    fn get_entity_room(&self, entity_id: NetworkId) -> Option<NetworkId> {
        self.state.world.lock().unwrap().get_entity_room(entity_id)
    }
    fn get_client_room(&self, client_id: ClientId) -> Option<NetworkId> {
        self.state.world.lock().unwrap().get_client_room(client_id)
    }
    fn get_local_id(&self, network_id: NetworkId) -> Option<aetheris_protocol::types::LocalId> {
        self.state.world.lock().unwrap().get_local_id(network_id)
    }
    fn get_network_id(&self, local_id: aetheris_protocol::types::LocalId) -> Option<NetworkId> {
        self.state.world.lock().unwrap().get_network_id(local_id)
    }
    fn despawn_networked(
        &mut self,
        network_id: NetworkId,
    ) -> Result<(), aetheris_protocol::error::WorldError> {
        self.state
            .world
            .lock()
            .unwrap()
            .despawn_networked(network_id)
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
    ) -> Result<aetheris_protocol::events::ComponentUpdate, aetheris_protocol::error::EncodeError>
    {
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

#[tokio::test(flavor = "multi_thread")]
async fn test_replication_splitting() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(aetheris_encoder_serde::SerdeEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

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

    let cid = ClientId(1);
    {
        let t = state.transport.lock().await;
        t.connect(cid);
        t.inject_event(NetworkEvent::ClientConnected(cid));
        let (token, _) = auth_service.mint_session_token("user1", None).unwrap();
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid,
            data: state
                .encoder
                .encode_event(&NetworkEvent::Auth {
                    session_token: token,
                })
                .unwrap(),
        });
    }

    let loop_transport = TransportRef(state.clone());
    let loop_world = WorldRef {
        state: state.clone(),
    };
    let loop_encoder = EncoderRef(state.clone());

    scheduler
        .tick_step(
            Arc::new(tokio::sync::RwLock::new(loop_transport)),
            Arc::new(std::sync::Mutex::new(loop_world)),
            Arc::new(loop_encoder),
        )
        .await;

    {
        let w = state.world.lock().unwrap();
        for i in 0..50 {
            w.queue_delta(ReplicationEvent {
                network_id: NetworkId(i + 10),
                tick: 1,
                component_kind: ComponentKind(1),
                payload: vec![0u8; 100], // Larger payload to trigger MTU splitting faster
            });
        }
    }

    let loop_transport = TransportRef(state.clone());
    let loop_world = WorldRef {
        state: state.clone(),
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

    tokio::time::sleep(Duration::from_millis(200)).await;

    {
        let t = state.transport.lock().await;
        let sent = t.take_unreliable(cid);
        assert!(sent.len() >= 4, "Expected >= 4 batches, got {}", sent.len());
    }

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_manual_fragmentation_fallback() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(aetheris_encoder_serde::SerdeEncoder::new()),
    };
    let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

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

    let cid = ClientId(1);
    {
        let t = state.transport.lock().await;
        t.connect(cid);
        t.inject_event(NetworkEvent::ClientConnected(cid));
        let (token, _) = auth_service.mint_session_token("user1", None).unwrap();
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid,
            data: state
                .encoder
                .encode_event(&NetworkEvent::Auth {
                    session_token: token,
                })
                .unwrap(),
        });
    }

    let loop_transport = TransportRef(state.clone());
    let loop_world = WorldRef {
        state: state.clone(),
    };
    let loop_encoder = EncoderRef(state.clone());

    scheduler
        .tick_step(
            Arc::new(tokio::sync::RwLock::new(loop_transport)),
            Arc::new(std::sync::Mutex::new(loop_world)),
            Arc::new(loop_encoder),
        )
        .await;

    {
        let w = state.world.lock().unwrap();
        w.queue_delta(ReplicationEvent {
            network_id: NetworkId(999),
            tick: 1,
            component_kind: ComponentKind(1),
            payload: vec![0u8; 3000], // Jumbo payload
        });
    }

    let loop_transport = TransportRef(state.clone());
    let loop_world = WorldRef {
        state: state.clone(),
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

    tokio::time::sleep(Duration::from_millis(200)).await;

    {
        let t = state.transport.lock().await;
        let sent = t.take_unreliable(cid);
        let mut fragment_count = 0;
        let decoder = aetheris_encoder_serde::SerdeEncoder::new();
        for data in sent {
            if let Ok(NetworkEvent::Fragment { .. }) = decoder.decode_event(&data) {
                fragment_count += 1;
            }
        }
        assert!(
            fragment_count >= 3,
            "Expected at least 3 fragments, got {}",
            fragment_count
        );
    }

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_tick_ingress_auth_v2() {
    let state = SharedState {
        transport: Arc::new(tokio::sync::Mutex::new(MockTransport::new())),
        world: Arc::new(Mutex::new(MockWorldState::new())),
        encoder: Arc::new(aetheris_encoder_serde::SerdeEncoder::new()),
    };
    let (_shutdown_tx, _shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

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

    let cid = ClientId(1);
    {
        let t = state.transport.lock().await;
        t.connect(cid);
        t.inject_event(NetworkEvent::ClientConnected(cid));
        let (token, _) = auth_service.mint_session_token("user1", None).unwrap();
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid,
            data: state
                .encoder
                .encode_event(&NetworkEvent::Auth {
                    session_token: token,
                })
                .unwrap(),
        });
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid,
            data: state
                .encoder
                .encode_event(&NetworkEvent::RequestSystemManifest { client_id: cid })
                .unwrap(),
        });
        t.inject_event(NetworkEvent::ReliableMessage {
            client_id: cid,
            data: state
                .encoder
                .encode_event(&NetworkEvent::StartSession { client_id: cid })
                .unwrap(),
        });
    }

    let loop_transport = TransportRef(state.clone());
    let loop_world = WorldRef {
        state: state.clone(),
    };
    let loop_encoder = EncoderRef(state.clone());

    // Run one tick manually to process everything
    scheduler
        .tick_step(
            Arc::new(tokio::sync::RwLock::new(loop_transport)),
            Arc::new(std::sync::Mutex::new(loop_world)),
            Arc::new(loop_encoder),
        )
        .await;

    // Wait and accumulate reliable messages
    let mut all_sent = Vec::new();
    for _ in 0..20 {
        {
            let t = state.transport.lock().await;
            let batch = t.take_reliable(cid);
            all_sent.extend(batch);
        }
        if all_sent.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        all_sent.len() >= 2,
        "Expected at least 2 reliable messages (Manifest + Possession), got {}",
        all_sent.len()
    );
}

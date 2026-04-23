//! Core tick processing logic for the Aetheris Server.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth::AuthService;
use aetheris_protocol::error::EncodeError;
use aetheris_protocol::events::{GameEvent, NetworkEvent, ReplicationEvent, WireEvent};
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};
use aetheris_protocol::types::{ClientId, NetworkId};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use tokio::sync::{RwLock, broadcast};
use tracing::{debug_span, error, info, warn};

/// Manages the authoritative game loop and replication pipeline.
pub struct TickScheduler {
    tick_rate: u32,
    current_tick: u64,
    authenticated_clients: HashMap<ClientId, String>, // ClientId -> PlayerId
    auth_service: Arc<dyn AuthService>,
    encode_pool: Arc<rayon::ThreadPool>,
    next_message_id: Arc<AtomicU32>,
}

impl TickScheduler {
    /// Creates a new `TickScheduler`.
    pub fn new(
        tick_rate: u32,
        auth_service: Arc<dyn AuthService>,
        encode_pool: Arc<rayon::ThreadPool>,
    ) -> Self {
        Self {
            tick_rate,
            current_tick: 0,
            authenticated_clients: HashMap::new(),
            auth_service,
            encode_pool,
            next_message_id: Arc::new(AtomicU32::new(1)),
        }
    }

    /// Runs the main scheduler loop until a shutdown signal is received.
    pub async fn run(
        &mut self,
        transport: Arc<RwLock<dyn GameTransport>>,
        world: Arc<Mutex<dyn WorldState>>,
        encoder: Arc<dyn Encoder>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) {
        let tick_duration = Duration::from_secs_f64(1.0 / f64::from(self.tick_rate));
        let mut interval = tokio::time::interval(tick_duration);

        info!("Scheduler started at {} Hz", self.tick_rate);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.tick_step(
                        Arc::clone(&transport),
                        Arc::clone(&world),
                        Arc::clone(&encoder),
                    ).await;
                }
                _ = shutdown_rx.recv() => {
                    info!("Scheduler shutting down");
                    break;
                }
            }
        }
    }

    /// Performs a single simulation and replication step.
    ///
    /// # Panics
    ///
    /// Panics if the transport or world mutex is poisoned.
    #[allow(clippy::too_many_lines)]
    pub async fn tick_step(
        &mut self,
        transport: Arc<RwLock<dyn GameTransport>>,
        world: Arc<Mutex<dyn WorldState>>,
        encoder: Arc<dyn Encoder>,
    ) {
        // Stage 1: Poll Network
        let t1 = Instant::now();
        let events = {
            let mut t = transport.write().await;
            t.poll_events().await.unwrap_or_else(|e| {
                error!("Transport poll failed: {e:?}");
                Vec::new()
            })
        };
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "poll")
            .record(t1.elapsed().as_secs_f64());

        // Stage 2: Ingress & Auth
        let t2 = Instant::now();
        let mut updates: Vec<(ClientId, aetheris_protocol::events::ComponentUpdate)> = Vec::new();
        let _reliable_to_process: Vec<(ClientId, Vec<u8>)> = Vec::new();

        // 1. Sequential processing of disconnected clients (rare)
        // Must happen before parallel decode because into_par_iter consumes events
        for event in &events {
            if let NetworkEvent::ClientDisconnected(cid) = event {
                self.authenticated_clients.remove(cid);
            }
        }

        // 2. Parallel decode and categorize events
        // Note: we use a Mutex for reliable_to_process to collect while parallel decoding
        let reliable_to_process_mutex = Arc::new(Mutex::new(Vec::new()));

        let decoded_updates: Vec<(ClientId, aetheris_protocol::events::ComponentUpdate)> = {
            let _span = debug_span!("ingress_decode").entered();
            events
                .into_par_iter()
                .filter_map(|event| match event {
                    NetworkEvent::UnreliableMessage { client_id, data }
                        if self.authenticated_clients.contains_key(&client_id) =>
                    {
                        encoder.decode(&data).ok().map(|update| (client_id, update))
                    }
                    NetworkEvent::ReliableMessage { client_id, data } => {
                        reliable_to_process_mutex
                            .lock()
                            .unwrap()
                            .push((client_id, data));
                        None
                    }
                    NetworkEvent::ClientConnected(cid) => {
                        info!("Client {cid:?} connected");
                        None
                    }
                    _ => None,
                })
                .collect()
        };
        updates.extend(decoded_updates);

        // 3. Process reliable messages (sequential as they are few and involve more logic)
        let reliable_msgs = Arc::try_unwrap(reliable_to_process_mutex)
            .unwrap()
            .into_inner()
            .unwrap();
        for (client_id, data) in reliable_msgs {
            if let Ok(msg) = encoder.decode_event(&data) {
                match msg {
                    NetworkEvent::Auth { session_token } => {
                        let span = debug_span!("ingress_auth", ?client_id);
                        // verify_session is async, so we must NOT hold an entered span across it.
                        // We use in_scope for non-async parts or just don't wrap the await.
                        match self.auth_service.verify_session(&session_token).await {
                            Ok(player_id) => {
                                span.in_scope(|| {
                                    info!("Client {client_id:?} authenticated as {player_id}");
                                    self.authenticated_clients.insert(client_id, player_id);
                                });
                            }
                            Err(e) => {
                                span.in_scope(|| {
                                    warn!("Auth failed for {client_id:?}: {e:?}");
                                });
                            }
                        }
                    }
                    NetworkEvent::StartSession { .. } => {
                        let _span = debug_span!("ingress_start_session", ?client_id).entered();
                        if self.authenticated_clients.contains_key(&client_id) {
                            let mut w = world.lock().unwrap();
                            let nid = w.spawn_session_ship(1, 0.0, 0.0, 0.0, client_id);
                            w.queue_reliable_event(
                                Some(client_id),
                                GameEvent::Possession { network_id: nid },
                            );
                        }
                    }
                    NetworkEvent::RequestSystemManifest { .. } => {
                        let _span = debug_span!("ingress_manifest", ?client_id).entered();
                        let mut w = world.lock().unwrap();
                        let mut manifest = BTreeMap::new();
                        manifest.insert("server_version".to_string(), "0.1.0".to_string());
                        manifest.insert("tick_rate".to_string(), self.tick_rate.to_string());

                        w.queue_reliable_event(
                            Some(client_id),
                            GameEvent::SystemManifest { manifest },
                        );
                    }
                    other => {
                        let _span = debug_span!("ingress_unknown", ?client_id).entered();
                        warn!("Unexpected reliable event from {client_id:?}: {other:?}");
                    }
                }
            } else {
                let _span = debug_span!("ingress_decode_error", ?client_id).entered();
                error!("Failed to decode reliable event from {client_id:?}");
            }
        }

        // 4. Batch apply all updates to the world (Single Mutex Lock)
        if !updates.is_empty() {
            let _span = debug_span!("ingress_apply", count = updates.len()).entered();
            let mut w = world.lock().unwrap();
            w.apply_updates(&updates);
        }

        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "ingress")
            .record(t2.elapsed().as_secs_f64());

        // Stage 3: Simulation
        let t3 = Instant::now();
        {
            let mut w = world.lock().unwrap();
            w.advance_tick();
            w.simulate();
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "simulate")
            .record(t3.elapsed().as_secs_f64());

        // Stage 4: Extraction
        let t4 = Instant::now();
        let (deltas, reliable_events) = {
            let mut w = world.lock().unwrap();
            let d = w.extract_deltas();
            let r = w.extract_reliable_events();
            w.post_extract();
            (d, r)
        };
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "extract")
            .record(t4.elapsed().as_secs_f64());

        // Reliability dispatch
        for (target, event) in reliable_events {
            let network_event = event.into_network_event(target.unwrap_or(ClientId(0)));
            if let Ok(data) = encoder.encode_event(&network_event) {
                let transport_cloned = Arc::clone(&transport);
                let t = transport_cloned.read().await;
                if let Some(cid) = target {
                    let _ = t.send_reliable(cid, &data).await;
                } else {
                    // Broadcast reliable isn't in trait, so we loop for now
                }
            }
        }

        // Stage 5: Send
        let t5 = Instant::now();
        if !deltas.is_empty() {
            let mut client_batches: HashMap<ClientId, Vec<ReplicationEvent>> = HashMap::new();

            {
                let w = world.lock().unwrap();
                for delta in deltas {
                    let targets = Self::get_delta_targets_internal(
                        &*w,
                        &self.authenticated_clients,
                        delta.network_id,
                    );
                    if targets.is_empty() {
                        for &client_id in self.authenticated_clients.keys() {
                            client_batches
                                .entry(client_id)
                                .or_default()
                                .push(delta.clone());
                        }
                    } else {
                        for client_id in targets {
                            client_batches
                                .entry(client_id)
                                .or_default()
                                .push(delta.clone());
                        }
                    }
                }
            }

            if !client_batches.is_empty() {
                let encoder_arc = Arc::clone(&encoder);
                let transport_arc = Arc::clone(&transport);
                let encode_pool = Arc::clone(&self.encode_pool);
                let next_message_id = Arc::clone(&self.next_message_id);

                tokio::task::block_in_place(move || {
                    let handle = tokio::runtime::Handle::current();
                    encode_pool.install(|| {
                        client_batches
                            .into_par_iter()
                            .for_each(|(client_id, events)| {
                                let _span = debug_span!("parallel_dispatch", ?client_id).entered();
                                Self::dispatch_replication(
                                    client_id,
                                    events,
                                    &encoder_arc,
                                    &transport_arc,
                                    &next_message_id,
                                    &handle,
                                );
                            });
                    });
                });
            }
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "send")
            .record(t5.elapsed().as_secs_f64());

        self.current_tick += 1;
    }

    fn dispatch_replication(
        client_id: ClientId,
        events: Vec<ReplicationEvent>,
        encoder: &Arc<dyn Encoder>,
        transport: &Arc<RwLock<dyn GameTransport>>,
        next_message_id: &Arc<AtomicU32>,
        handle: &tokio::runtime::Handle,
    ) {
        let mut current_batch = Vec::new();
        let mut current_size = 32;

        for event in events {
            let estimated_size = 20 + event.payload.len();
            if !current_batch.is_empty()
                && current_size + estimated_size > aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE
            {
                Self::send_batch(
                    client_id,
                    std::mem::take(&mut current_batch),
                    encoder,
                    transport,
                    next_message_id,
                    handle,
                );
                current_size = 32;
            }
            current_size += estimated_size;
            current_batch.push(event);
        }

        if !current_batch.is_empty() {
            Self::send_batch(
                client_id,
                current_batch,
                encoder,
                transport,
                next_message_id,
                handle,
            );
        }
    }

    fn send_batch(
        client_id: ClientId,
        events: Vec<ReplicationEvent>,
        encoder: &Arc<dyn Encoder>,
        transport: &Arc<RwLock<dyn GameTransport>>,
        next_message_id: &Arc<AtomicU32>,
        handle: &tokio::runtime::Handle,
    ) {
        let batch_event = NetworkEvent::ReplicationBatch {
            client_id,
            events: events.clone(),
        };

        match encoder.encode_event(&batch_event) {
            Ok(data) => {
                let t = Arc::clone(transport);
                handle.spawn(async move {
                    let transport_guard = t.read().await;
                    let _ = transport_guard.send_unreliable(client_id, &data).await;
                });
            }
            Err(EncodeError::BufferOverflow { .. }) => {
                let wire_event = WireEvent::ReplicationBatch(events);
                if let Ok(data) = rmp_serde::to_vec(&wire_event) {
                    let t = Arc::clone(transport);
                    let e = Arc::clone(encoder);
                    let message_id = next_message_id.fetch_add(1, Ordering::SeqCst);
                    handle.spawn(async move {
                        let _ =
                            Self::fragment_and_send_static(message_id, &data, &[client_id], &*e, t)
                                .await;
                    });
                }
            }
            Err(e) => {
                error!("Failed to encode replication batch for {client_id:?}: {e:?}");
            }
        }
    }

    fn get_delta_targets_internal(
        world: &dyn WorldState,
        authenticated_clients: &HashMap<ClientId, String>,
        network_id: NetworkId,
    ) -> Vec<ClientId> {
        let mut targets = Vec::new();
        let entity_room = world.get_entity_room(network_id);

        for &client_id in authenticated_clients.keys() {
            let client_room = world.get_client_room(client_id);
            if entity_room == client_room {
                targets.push(client_id);
            }
        }
        targets
    }

    async fn fragment_and_send_static(
        message_id: u32,
        data: &[u8],
        targets: &[ClientId],
        encoder: &dyn Encoder,
        transport: Arc<RwLock<dyn GameTransport>>,
    ) -> Result<(), aetheris_protocol::error::TransportError> {
        let max_size = aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE - 32;
        let total_fragments_usize = data.len().div_ceil(max_size);

        #[allow(clippy::cast_possible_truncation)]
        let total_fragments = total_fragments_usize as u32;

        for i in 0..total_fragments {
            let start = i as usize * max_size;
            let end = (start + max_size).min(data.len());
            let fragment_data = &data[start..end];

            #[allow(clippy::cast_possible_truncation)]
            let fragment = aetheris_protocol::events::FragmentedEvent {
                message_id,
                fragment_index: i as u16,
                total_fragments: total_fragments as u16,
                payload: fragment_data.to_vec(),
            };

            for &client_id in targets {
                let event = NetworkEvent::Fragment {
                    client_id,
                    fragment: fragment.clone(),
                };
                if let Ok(encoded) = encoder.encode_event(&event) {
                    let t = transport.read().await;
                    let _ = t.send_unreliable(client_id, &encoded).await;
                }
            }
        }
        Ok(())
    }
}

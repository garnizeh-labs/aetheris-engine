use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{Instrument, debug_span, error, info_span};

use aetheris_protocol::error::EncodeError;
use aetheris_protocol::events::{FragmentedEvent, NetworkEvent};
use aetheris_protocol::reassembler::Reassembler;
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};
use aetheris_protocol::types::{ClientId, NetworkId};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

/// Messages sent to the dedicated outbound sender task.
pub enum OutboundMessage {
    Unreliable { client_id: ClientId, data: Vec<u8> },
    Reliable { client_id: ClientId, data: Vec<u8> },
    BroadcastUnreliable { data: Vec<u8> },
}

#[derive(Debug, Clone)]
pub enum DeltaTargets {
    Broadcast,
    Recipients(Vec<ClientId>),
    NoRecipients,
}

/// Manages the fixed-timestep execution of the game loop.
#[derive(Debug)]
pub struct TickScheduler {
    tick_rate: u64,
    current_tick: u64,
    auth_service: Arc<dyn crate::auth::AuthSessionVerifier>,

    /// Maps `ClientId` -> (Session JTI, owned session ship `NetworkId`)
    authenticated_clients: HashMap<ClientId, (String, Option<NetworkId>)>,
    /// Tracks when each client was successfully authenticated.
    auth_timestamps: HashMap<ClientId, Instant>,
    reassembler: Reassembler,
    next_message_id: u32,
    encode_pool: Arc<rayon::ThreadPool>,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
}

impl TickScheduler {
    /// Creates a new scheduler with the specified tick rate.
    #[must_use]
    pub fn new(
        tick_rate: u64,
        auth_service: Arc<dyn crate::auth::AuthSessionVerifier>,
        encode_pool: Arc<rayon::ThreadPool>,
    ) -> Self {
        Self {
            tick_rate,
            current_tick: 0,
            auth_service,
            authenticated_clients: HashMap::new(),
            auth_timestamps: HashMap::new(),
            reassembler: Reassembler::new(),
            next_message_id: 1,
            encode_pool,
            outbound_tx: None,
        }
    }

    /// Sets the outbound channel for messages. Used in tests or custom loops.
    pub fn set_outbound_tx(&mut self, tx: tokio::sync::mpsc::Sender<OutboundMessage>) {
        self.outbound_tx = Some(tx);
    }

    /// Runs the infinite game loop until the shutdown token is cancelled.
    pub async fn run(
        &mut self,
        transport: Box<dyn GameTransport>,
        mut world: Box<dyn WorldState>,
        encoder: Box<dyn Encoder>,
        mut shutdown: broadcast::Receiver<()>,
    ) {
        let (tx, mut rx) = mpsc::channel(2048);
        self.outbound_tx = Some(tx.clone());

        let transport = Arc::new(RwLock::new(transport));
        let transport_clone = transport.clone();

        let mut outbound_shutdown = shutdown.resubscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        let Some(msg) = msg else { break; };
                        let transport = transport_clone.read().await;
                        match msg {
                            OutboundMessage::Unreliable { client_id, data } => {
                                if let Err(e) = transport.send_unreliable(client_id, &data).await {
                                    error!(error = ?e, ?client_id, "Outbound task failed to send unreliable message");
                                }
                            }
                            OutboundMessage::Reliable { client_id, data } => {
                                if let Err(e) = transport.send_reliable(client_id, &data).await {
                                    error!(error = ?e, ?client_id, "Outbound task failed to send reliable message");
                                }
                            }
                            OutboundMessage::BroadcastUnreliable { data } => {
                                if let Err(e) = transport.broadcast_unreliable(&data).await {
                                    error!(error = ?e, "Outbound task failed to broadcast unreliable message");
                                }
                            }
                        }
                    }
                    _ = outbound_shutdown.recv() => {
                        break;
                    }
                }
            }
        });

        #[allow(clippy::cast_precision_loss)]
        let tick_duration = Duration::from_secs_f64(1.0 / self.tick_rate as f64);
        let mut interval = interval(tick_duration);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let mut last_tick_wall = Instant::now();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let tick_num = self.current_tick;
                    let start = Instant::now();

                    // Wall-clock tick rate: measured from the previous tick start.
                    let wall_elapsed = start.duration_since(last_tick_wall);
                    if wall_elapsed.as_secs_f64() > 0.0 {
                        metrics::gauge!("aetheris_actual_tick_rate_hz")
                            .set(1.0 / wall_elapsed.as_secs_f64());
                    }
                    last_tick_wall = start;

                    self.tick_step(
                        &transport,
                        world.as_mut(),
                        encoder.as_ref(),
                    )
                    .instrument(info_span!("tick", tick = tick_num))
                    .await;
                    let elapsed = start.elapsed();

                    metrics::histogram!("aetheris_tick_duration_seconds").record(elapsed.as_secs_f64());
                }
                _ = shutdown.recv() => {
                    tracing::info!("Server shutting down gracefully");
                    break;
                }
            }
        }
    }

    /// Executes a single 5-stage tick pipeline.
    #[allow(clippy::too_many_lines)]
    pub async fn tick_step(
        &mut self,
        transport_lock: &RwLock<Box<dyn GameTransport>>,
        world: &mut dyn WorldState,
        encoder: &dyn Encoder,
    ) {
        let tick_start = Instant::now();
        let tick = self.current_tick;
        self.current_tick += 1;

        let mut transport = transport_lock.write().await;
        // Pre-Stage: Advance the world change tick before any inputs are applied.
        // This ensures entities spawned in Stage 2 receive a tick strictly greater than
        // `last_extraction_tick`, which is required for Bevy 0.15+'s `is_changed` check.
        // Without this, newly spawned entities share the same tick as `last_extraction_tick`
        // and are silently skipped by `extract_deltas`, causing them to never be replicated.
        world.advance_tick();

        // Stage 1: Poll
        let t1 = Instant::now();
        let events = match transport
            .poll_events()
            .instrument(debug_span!("stage1_poll"))
            .await
        {
            Ok(e) => e,
            Err(e) => {
                error!(error = ?e, "Fatal transport error during poll; skipping tick");
                return;
            }
        };
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "poll")
            .record(t1.elapsed().as_secs_f64());

        let inbound_count: u64 = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    NetworkEvent::UnreliableMessage { .. } | NetworkEvent::ReliableMessage { .. }
                )
            })
            .count() as u64;
        metrics::counter!("aetheris_packets_inbound_total").increment(inbound_count);

        // Periodic Session Validation (every 60 ticks / ~1s)
        if tick.is_multiple_of(60) {
            let mut to_remove = Vec::new();
            for (&client_id, (jti, _)) in &self.authenticated_clients {
                if !self.auth_service.is_session_authorized(jti, Some(tick)) {
                    tracing::warn!(?client_id, "Session invalidated during periodic check");
                    to_remove.push(client_id);
                }
            }
            for client_id in to_remove {
                if let Some((_, Some(nid))) = self.authenticated_clients.remove(&client_id) {
                    let _ = world.despawn_networked(nid);
                }
                self.auth_timestamps.remove(&client_id);
                metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
            }
        }

        // Stage 2: Apply
        let t2 = Instant::now();
        let mut pong_responses = None;
        let mut clear_ack_targets: Vec<aetheris_protocol::types::ClientId> = Vec::new();
        if !events.is_empty() {
            let _span = debug_span!("stage2_apply", count = events.len()).entered();
            let mut updates = Vec::with_capacity(events.len());
            for event in events {
                // Stage 2.1: Reassembly & Normalization
                let (client_id, raw_data, is_message) = match event {
                    NetworkEvent::Fragment {
                        client_id,
                        fragment,
                    } => {
                        if let Some(data) = self.reassembler.ingest(client_id, fragment) {
                            (client_id, data, true)
                        } else {
                            continue;
                        }
                    }
                    NetworkEvent::UnreliableMessage { data, client_id }
                    | NetworkEvent::ReliableMessage { data, client_id } => {
                        // Try to decode as a protocol fragment first
                        if let Ok(NetworkEvent::Fragment { fragment, .. }) =
                            encoder.decode_event(&data)
                        {
                            if let Some(reassembled) = self.reassembler.ingest(client_id, fragment)
                            {
                                (client_id, reassembled, true)
                            } else {
                                continue;
                            }
                        } else {
                            (client_id, data, true)
                        }
                    }
                    NetworkEvent::ClientConnected(id) => {
                        metrics::gauge!("aetheris_connected_clients").increment(1.0);
                        tracing::info!(client_id = ?id, "Client connected (awaiting auth)");
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::ClientDisconnected(id) | NetworkEvent::Disconnected(id) => {
                        metrics::gauge!("aetheris_connected_clients").decrement(1.0);
                        if let Some((_, Some(nid))) = self.authenticated_clients.remove(&id) {
                            let _ = world.despawn_networked(nid);
                        }
                        self.auth_timestamps.remove(&id);
                        tracing::info!(client_id = ?id, "Client disconnected");
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::SessionClosed(id) => {
                        metrics::counter!("aetheris_transport_events_total", "type" => "session_closed")
                        .increment(1);
                        tracing::warn!(client_id = ?id, "WebTransport session closed");
                        if let Some((_, Some(nid))) = self.authenticated_clients.remove(&id) {
                            let _ = world.despawn_networked(nid);
                        }
                        self.auth_timestamps.remove(&id);
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::StreamReset(id) => {
                        metrics::counter!("aetheris_transport_events_total", "type" => "stream_reset")
                        .increment(1);
                        tracing::error!(client_id = ?id, "WebTransport stream reset");
                        if let Some((_, Some(nid))) = self.authenticated_clients.remove(&id) {
                            let _ = world.despawn_networked(nid);
                        }
                        self.auth_timestamps.remove(&id);
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::Ping { client_id, tick } => {
                        if self.authenticated_clients.contains_key(&client_id) {
                            pong_responses.get_or_insert_with(Vec::new).push((
                                client_id,
                                tick,
                                Instant::now(),
                            ));
                            metrics::counter!("aetheris_protocol_pings_received_total")
                                .increment(1);
                        }
                        (client_id, Vec::new(), false)
                    }
                    NetworkEvent::ClearWorld { client_id, .. }
                    | NetworkEvent::StartSession { client_id }
                    | NetworkEvent::RequestSystemManifest { client_id }
                    | NetworkEvent::GameEvent { client_id, .. }
                    | NetworkEvent::StressTest { client_id, .. }
                    | NetworkEvent::ReplicationBatch { client_id, .. }
                    | NetworkEvent::Spawn { client_id, .. } => (client_id, Vec::new(), false),
                    NetworkEvent::Pong { .. } | NetworkEvent::Auth { .. } => {
                        (aetheris_protocol::types::ClientId(0), Vec::new(), false)
                    }
                };

                if !is_message {
                    continue;
                }

                // Stage 2.2: Auth & Protocol Decode
                let jti = if let Some((jti, _)) = self.authenticated_clients.get(&client_id) {
                    // Re-validate session on every message to refresh sliding window / catch revocation
                    if !self.auth_service.is_session_authorized(jti, Some(tick)) {
                        tracing::warn!(?client_id, "Session revoked; dropping client");
                        if let Some((_, Some(nid))) = self.authenticated_clients.remove(&client_id)
                        {
                            let _ = world.despawn_networked(nid);
                        }
                        self.auth_timestamps.remove(&client_id);
                        metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
                        continue;
                    }
                    jti
                } else {
                    // Client not authenticated yet; only accept Auth message
                    match encoder.decode_event(&raw_data) {
                        Ok(NetworkEvent::Auth { session_token }) => {
                            tracing::info!(?client_id, "Auth message received");
                            match self.auth_service.verify_session(&session_token, Some(tick)) {
                                Ok(session) => {
                                    tracing::info!(?client_id, "Client authenticated successfully");

                                    self.authenticated_clients
                                        .insert(client_id, (session.jti, None));
                                    // Record when auth completed so we can measure server-side
                                    // possession latency (A-08 profiling metric).
                                    self.auth_timestamps.insert(client_id, Instant::now());

                                    tracing::info!(
                                        ?client_id,
                                        "[Auth] Client authenticated — waiting for StartSession to spawn ship"
                                    );
                                    continue;
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        ?client_id,
                                        error = ?e,
                                        "Client failed authentication"
                                    );
                                }
                            }
                        }
                        Ok(other) => {
                            tracing::warn!(
                                ?client_id,
                                variant = ?std::mem::discriminant(&other),
                                bytes = raw_data.len(),
                                "Unauthenticated client sent non-Auth event — discarding"
                            );
                            metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
                        }
                        Err(e) => {
                            tracing::warn!(
                                ?client_id,
                                error = ?e,
                                bytes = raw_data.len(),
                                "Failed to decode message from unauthenticated client"
                            );
                            metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
                        }
                    }
                    continue;
                };

                // Check if it's a protocol-level event first (Ping/Pong/etc)
                if let Ok(protocol_event) = encoder.decode_event(&raw_data) {
                    match protocol_event {
                        NetworkEvent::Ping { tick: p_tick, .. } => {
                            pong_responses.get_or_insert_with(Vec::new).push((
                                client_id,
                                p_tick,
                                Instant::now(),
                            ));
                            metrics::counter!("aetheris_protocol_pings_received_total")
                                .increment(1);
                        }
                        NetworkEvent::Auth { .. } => {
                            tracing::debug!(?client_id, "Client re-authenticating (ignored)");
                        }
                        NetworkEvent::StressTest { count, rotate, .. } => {
                            tracing::info!(
                                ?client_id,
                                count,
                                rotate,
                                "StressTest event received from authenticated client"
                            );
                            if can_run_playground_command(jti) {
                                // M10105 — Safety cap to prevent server-side resource exhaustion.
                                const MAX_STRESS: u16 = 1000;
                                let capped_count = count.min(MAX_STRESS);
                                if count > MAX_STRESS {
                                    tracing::warn!(
                                        ?client_id,
                                        count,
                                        capped_count,
                                        "Stress test count capped at limit"
                                    );
                                }

                                tracing::info!(
                                    ?client_id,
                                    count = capped_count,
                                    rotate,
                                    "Stress test command executed"
                                );
                                world.stress_test(capped_count, rotate);
                            } else {
                                tracing::warn!(?client_id, "Unauthorized StressTest attempt");
                                metrics::counter!("aetheris_unprivileged_packets_total")
                                    .increment(1);
                            }
                        }
                        NetworkEvent::Spawn {
                            entity_type,
                            x,
                            y,
                            rot,
                            ..
                        } => {
                            if can_run_playground_command(jti) {
                                let network_id =
                                    world.spawn_kind_for(entity_type, x, y, rot, client_id);

                                tracing::info!(
                                    ?client_id,
                                    entity_type,
                                    new_entity_id = network_id.0,
                                    "[Spawn] Playground entity spawned"
                                );
                            } else {
                                tracing::warn!(?client_id, "Unauthorized Spawn attempt");
                                metrics::counter!("aetheris_unprivileged_packets_total")
                                    .increment(1);
                            }
                        }
                        NetworkEvent::StartSession { .. } => {
                            if let Some((_, ship_id)) =
                                self.authenticated_clients.get_mut(&client_id)
                            {
                                let network_id = if let Some(nid) = ship_id {
                                    tracing::info!(
                                        ?client_id,
                                        ?nid,
                                        "Reusing existing session ship"
                                    );
                                    *nid
                                } else {
                                    let nid = world.spawn_session_ship(1, 0.0, 0.0, 0.0, client_id);
                                    *ship_id = Some(nid);
                                    nid
                                };

                                world.queue_reliable_event(
                                    Some(client_id),
                                    aetheris_protocol::events::GameEvent::Possession { network_id },
                                );

                                // Record server-side auth→possession latency (A-08 profiling).
                                if let Some(auth_ts) = self.auth_timestamps.remove(&client_id) {
                                    metrics::histogram!("aetheris_session_start_latency_seconds")
                                        .record(auth_ts.elapsed().as_secs_f64());
                                }

                                tracing::info!(
                                    ?client_id,
                                    network_id = network_id.0,
                                    "[StartSession] Session ship assigned — Possession sent"
                                );
                            }
                        }
                        NetworkEvent::ClearWorld { .. } => {
                            if can_run_playground_command(jti) {
                                tracing::info!(?client_id, "ClearWorld command executed");
                                world.clear_world();
                                // Reset the client's entity-ID tracking so that a subsequent
                                // StartSession can spawn a new session ship.  Without this,
                                // the "already_has_ship" guard blocks the next StartSession
                                // even though all entities were just despawned.
                                if let Some((_, ship_id)) =
                                    self.authenticated_clients.get_mut(&client_id)
                                {
                                    *ship_id = None;
                                }
                                // Queue a reliable ClearWorld ack to send after this block.
                                // EnteredSpan is !Send so we cannot .await inside this scope.
                                // The ack arrives at the client AFTER stale in-flight datagrams,
                                // guaranteeing a full entity flush (eliminates partial-clear race).
                                clear_ack_targets.push(client_id);
                            } else {
                                tracing::warn!(?client_id, "Unauthorized ClearWorld attempt");
                                metrics::counter!("aetheris_unprivileged_packets_total")
                                    .increment(1);
                            }
                        }
                        NetworkEvent::RequestSystemManifest { .. } => {
                            let jti = if let Some((jti, _)) =
                                self.authenticated_clients.get(&client_id)
                            {
                                jti
                            } else {
                                ""
                            };

                            let manifest = self.get_filtered_manifest(jti);
                            world.queue_reliable_event(
                                Some(client_id),
                                aetheris_protocol::events::GameEvent::SystemManifest { manifest },
                            );
                        }
                        NetworkEvent::ReplicationBatch { events, .. } => {
                            for event in events {
                                updates.push((
                                    client_id,
                                    aetheris_protocol::events::ComponentUpdate {
                                        network_id: event.network_id,
                                        component_kind: event.component_kind,
                                        payload: event.payload,
                                        tick: event.tick,
                                    },
                                ));
                            }
                        }
                        _ => {
                            tracing::trace!(?protocol_event, "Protocol event");
                        }
                    }
                } else {
                    // If it's not a protocol event, try to decode it as a game update
                    match encoder.decode(&raw_data) {
                        Ok(update) => updates.push((client_id, update)),
                        Err(e) => {
                            metrics::counter!("aetheris_decode_errors_total").increment(1);
                            error!(
                                error = ?e,
                                size = raw_data.len(),
                                "Failed to decode update (not a protocol event)"
                            );
                        }
                    }
                }
            }
            world.apply_updates(&updates);
            self.reassembler.prune();
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "apply")
            .record(t2.elapsed().as_secs_f64());

        // Send ClearWorld acks (reliable) for any ClearWorld commands processed this tick.
        // Sent after the EnteredSpan is dropped, since EnteredSpan is !Send.
        // The reliable delivery guarantees the client sees this AFTER any stale in-flight
        // unreliable datagrams, closing the partial-clear race condition.
        for target in clear_ack_targets {
            let ack = NetworkEvent::ClearWorld { client_id: target };
            #[allow(clippy::collapsible_if)]
            if let Ok(data) = encoder.encode_event(&ack) {
                if let Err(e) = transport.send_reliable(target, &data).await {
                    tracing::warn!(client_id = ?target, error = ?e, "Failed to send ClearWorld ack");
                }
            }
        }

        // Send Pongs for all collected Pings.
        // Use unreliable (datagram) so the reply travels the same path as the
        // incoming Ping, and clients that only read datagrams can receive it.
        if let Some(pongs) = pong_responses {
            for (client_id, p_tick, received_at) in pongs {
                let pong_event = NetworkEvent::Pong { tick: p_tick };
                if let Ok(data) = encoder.encode_event(&pong_event) {
                    // Measure server-side Pong dispatch time (encode + send).
                    // This is NOT the full network RTT, but it captures the
                    // server processing overhead between Ping receipt and Pong send.
                    let dispatch_start = Instant::now();
                    match transport.send_unreliable(client_id, &data).await {
                        Ok(()) => {
                            let dispatch_ms = dispatch_start.elapsed().as_secs_f64() * 1000.0;
                            let server_hold_ms = received_at.elapsed().as_secs_f64() * 1000.0;
                            metrics::histogram!("aetheris_server_pong_dispatch_ms")
                                .record(dispatch_ms);
                            metrics::histogram!("aetheris_server_ping_hold_ms")
                                .record(server_hold_ms);
                        }
                        Err(e) => {
                            error!(error = ?e, client_id = ?client_id, "Failed to send Pong");
                        }
                    }
                }
            }
        }

        // Stage 3: Simulate
        let t3 = Instant::now();
        {
            let _span = debug_span!("stage3_simulate").entered();
            // Simulation logic (physics, AI, game rules) happens here.
            world.simulate();
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "simulate")
            .record(t3.elapsed().as_secs_f64());

        // Stage 4: Extract
        let t4 = Instant::now();
        let (deltas, reliable_events) = {
            let ds = world.extract_deltas();
            let rs = world.extract_reliable_events();
            (ds, rs)
        };
        // Reset ECS change-detection *after* extraction so simulate()'s mutations are visible.
        world.post_extract();
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "extract")
            .record(t4.elapsed().as_secs_f64());

        // Stage 5: Encode & Send
        let t5 = Instant::now();

        // Stage 5.1: Send Reliable Events
        for (target, wire_event) in reliable_events {
            // Broadcast reliably to all authenticated clients if target is None
            let targets: Vec<_> = if let Some(id) = target {
                vec![id]
            } else {
                self.authenticated_clients.keys().copied().collect()
            };

            for id in targets {
                let network_event = wire_event.clone().into_network_event(id);
                match encoder.encode_event(&network_event) {
                    Ok(data) => {
                        if let Some(tx) = &self.outbound_tx {
                            let _ = tx
                                .send(OutboundMessage::Reliable {
                                    client_id: id,
                                    data,
                                })
                                .await;
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, client_id = ?id, "Failed to encode reliable event");
                    }
                }
            }
        }

        if !deltas.is_empty() {
            let mut broadcast_count: u64 = 0;

            let stage_span = debug_span!("stage5_send", count = deltas.len());
            let _guard = stage_span.enter();

            // A-01: Packet Batching (Phase 1 Optimization)
            // Group all deltas by their target clients to avoid N*M packet explosion.
            let mut client_batches: HashMap<
                aetheris_protocol::types::ClientId,
                Vec<aetheris_protocol::events::ReplicationEvent>,
            > = HashMap::with_capacity(self.authenticated_clients.len());

            for delta in deltas {
                let targets =
                    Self::get_delta_targets(world, &self.authenticated_clients, delta.network_id);

                match targets {
                    DeltaTargets::Broadcast => {
                        // Global broadcast (to all authenticated clients)
                        for &client_id in self.authenticated_clients.keys() {
                            client_batches
                                .entry(client_id)
                                .or_default()
                                .push(delta.clone());
                        }
                    }
                    DeltaTargets::Recipients(recipients) => {
                        // Targeted multicast (AoI / Room filtered)
                        for target in recipients {
                            client_batches
                                .entry(target)
                                .or_default()
                                .push(delta.clone());
                        }
                    }
                    DeltaTargets::NoRecipients => {}
                }
            }

            let max_size = encoder.max_encoded_size();
            thread_local! {
                static SCRATCH_BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
            }

            // A-04: Parallel Stage 5 Encode
            // CPU-intensive serialization is offloaded to a dedicated Rayon pool.
            // We use block_in_place to inform Tokio that the current thread is performing CPU-heavy work.
            use rayon::prelude::{IntoParallelIterator, ParallelIterator};

            let batches_to_encode: Vec<_> = client_batches.into_iter().collect();

            let encoded_results = tokio::task::block_in_place(|| {
                self.encode_pool.install(|| {
                    batches_to_encode
                        .into_par_iter()
                        .map(|(client_id, events)| {
                            let batch_event =
                                aetheris_protocol::events::NetworkEvent::ReplicationBatch {
                                    client_id,
                                    events,
                                };
                            // SCRATCH_BUFFER optimization (M10105):
                            // Uses worker-local memory to avoid allocations during serialization.
                            // Only a single final allocation (to_vec) is performed to return data to main thread.
                            SCRATCH_BUFFER.with(|buf| {
                                let mut b = buf.borrow_mut();
                                if b.len() < max_size {
                                    b.resize(max_size, 0);
                                }
                                match encoder.encode_event_into(&batch_event, &mut b) {
                                    Ok(size) => (client_id, Ok(b[..size].to_vec())),
                                    Err(
                                        aetheris_protocol::error::EncodeError::BufferOverflow {
                                            ..
                                        },
                                    ) => {
                                        // M10105 — Reliable fallback for large batches that exceed scratch buffer.
                                        // We use the allocating encode_event() here as a safety valve.
                                        match encoder.encode_event(&batch_event) {
                                            Ok(data) => (client_id, Ok(data)),
                                            Err(e) => (client_id, Err(e)),
                                        }
                                    }
                                    Err(e) => (client_id, Err(e)),
                                }
                            })
                        })
                        .collect::<Vec<_>>()
                })
            });

            for (client_id, result) in encoded_results {
                match result {
                    Ok(data) => {
                        let targets = DeltaTargets::Recipients(vec![client_id]);
                        if data.len() > aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE {
                            match self
                                .fragment_and_send(&data, data.len(), &targets, encoder)
                                .await
                            {
                                Ok(count) => broadcast_count += count,
                                Err(e) => {
                                    error!(error = ?e, ?client_id, "Failed to fragment large batch");
                                }
                            }
                        } else if let Some(tx) = &self.outbound_tx {
                            let _ = tx
                                .send(OutboundMessage::Unreliable { client_id, data })
                                .await;
                            broadcast_count += 1;
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, ?client_id, "Failed to encode batch");
                    }
                }
            }

            metrics::counter!("aetheris_packets_outbound_total").increment(broadcast_count);
            metrics::counter!("aetheris_packets_broadcast_total").increment(broadcast_count);
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "send")
            .record(t5.elapsed().as_secs_f64());

        metrics::histogram!("aetheris_tick_duration_seconds")
            .record(tick_start.elapsed().as_secs_f64());
    }

    fn get_delta_targets(
        world: &dyn WorldState,
        clients: &HashMap<ClientId, (String, Option<NetworkId>)>,
        entity_id: NetworkId,
    ) -> DeltaTargets {
        if let Some(room_id) = world.get_entity_room(entity_id) {
            let mut recipients = Vec::new();
            for &client_id in clients.keys() {
                if world.get_client_room(client_id) == Some(room_id) {
                    recipients.push(client_id);
                }
            }
            if recipients.is_empty() {
                DeltaTargets::NoRecipients
            } else {
                DeltaTargets::Recipients(recipients)
            }
        } else {
            DeltaTargets::Broadcast
        }
    }

    async fn fragment_and_send(
        &mut self,
        data: &[u8],
        len: usize,
        targets: &DeltaTargets,
        encoder: &dyn Encoder,
    ) -> Result<u64, EncodeError> {
        let Some(tx) = &self.outbound_tx else {
            return Ok(0);
        };
        let message_id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);

        let chunk_size = aetheris_protocol::MAX_FRAGMENT_PAYLOAD_SIZE;
        let chunks: Vec<_> = data[..len].chunks(chunk_size).collect();

        let Ok(total_fragments) = u16::try_from(chunks.len()) else {
            error!(
                message_id,
                chunks = chunks.len(),
                "Too many fragments required for message; dropping payload"
            );
            return Err(EncodeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Too many fragments",
            )));
        };

        let mut sent_count = 0;
        for (i, chunk) in chunks.into_iter().enumerate() {
            let Ok(fragment_index) = u16::try_from(i) else {
                error!(message_id, index = i, "Fragment index overflow; stopping");
                break;
            };

            let fragment = FragmentedEvent {
                message_id,
                fragment_index,
                total_fragments,
                payload: chunk.to_vec(),
            };
            let fragment_event = NetworkEvent::Fragment {
                client_id: ClientId(0),
                fragment,
            };

            match encoder.encode_event(&fragment_event) {
                Ok(encoded_fragment) => match targets {
                    DeltaTargets::Broadcast => {
                        let _ = tx
                            .send(OutboundMessage::BroadcastUnreliable {
                                data: encoded_fragment,
                            })
                            .await;
                        sent_count += 1;
                    }
                    DeltaTargets::Recipients(recipients) => {
                        for &target in recipients {
                            let _ = tx
                                .send(OutboundMessage::Unreliable {
                                    client_id: target,
                                    data: encoded_fragment.clone(),
                                })
                                .await;
                            sent_count += 1;
                        }
                    }
                    DeltaTargets::NoRecipients => {}
                },
                Err(e) => {
                    error!(error = ?e, "Failed to encode fragment event");
                }
            }
        }

        Ok(sent_count)
    }

    fn get_filtered_manifest(&self, jti: &str) -> BTreeMap<String, String> {
        let mut manifest = BTreeMap::new();
        manifest.insert(
            "version_server".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
        manifest.insert(
            "version_protocol".to_string(),
            aetheris_protocol::VERSION.to_string(),
        );

        if can_run_playground_command(jti) {
            manifest.insert("tick_rate".to_string(), self.tick_rate.to_string());
            manifest.insert(
                "clients_active".to_string(),
                self.authenticated_clients.len().to_string(),
            );
        }
        manifest
    }
}

/// Validates if a session (identified by its JTI) is authorized to run destructive playground commands.
///
/// In Phase 1, this uses a simplified check against the 'admin' JTI used in development.
/// In Phase 3, this will be tied to the account's permission level.
fn can_run_playground_command(jti: &str) -> bool {
    // Current dev credential in Aetheris Playground always generates jti="admin"
    // Fail closed: AETHERIS_ENV must be explicitly set to "dev"; absence is not treated as dev.
    jti == "admin" || std::env::var("AETHERIS_ENV").ok().as_deref() == Some("dev")
}

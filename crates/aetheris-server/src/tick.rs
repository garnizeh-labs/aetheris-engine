use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{Instrument, debug_span, error, info_span};

use crate::auth::AuthServiceImpl;
use aetheris_protocol::error::EncodeError;
use aetheris_protocol::events::{FragmentedEvent, NetworkEvent};
use aetheris_protocol::reassembler::Reassembler;
use aetheris_protocol::traits::{Encoder, GameTransport, WorldState};

/// Manages the fixed-timestep execution of the game loop.
#[derive(Debug)]
pub struct TickScheduler {
    tick_rate: u64,
    current_tick: u64,
    auth_service: AuthServiceImpl,

    /// Maps `ClientId` -> (Session JTI, all owned `NetworkId`s)
    /// Index 0 (if present) is always the session ship.
    authenticated_clients: HashMap<
        aetheris_protocol::types::ClientId,
        (String, Vec<aetheris_protocol::types::NetworkId>),
    >,
    /// Tracks when each client was successfully authenticated.
    /// Used to record `aetheris_session_start_latency_seconds` — the server-side
    /// time from auth validation to Possession dispatch. See A-08 in
    /// `performance/runs/20260422_101553/ACTIONS.md`.
    auth_timestamps: HashMap<aetheris_protocol::types::ClientId, Instant>,
    reassembler: Reassembler,
    next_message_id: u32,
}

impl TickScheduler {
    /// Creates a new scheduler with the specified tick rate.
    #[must_use]
    pub fn new(tick_rate: u64, auth_service: AuthServiceImpl) -> Self {
        Self {
            tick_rate,
            current_tick: 0,
            auth_service,
            authenticated_clients: HashMap::new(),
            auth_timestamps: HashMap::new(),
            reassembler: Reassembler::new(),
            next_message_id: 0,
        }
    }

    /// Runs the infinite game loop until the shutdown token is cancelled.
    pub async fn run(
        &mut self,
        mut transport: Box<dyn GameTransport>,
        mut world: Box<dyn WorldState>,
        encoder: Box<dyn Encoder>,
        mut shutdown: broadcast::Receiver<()>,
    ) {
        #[allow(clippy::cast_precision_loss)]
        let tick_duration = Duration::from_secs_f64(1.0 / self.tick_rate as f64);
        let mut interval = interval(tick_duration);
        // Use Delay so that a slow tick shifts the next deadline rather than
        // firing immediately (Burst) or silently skipping it (Skip). This keeps
        // the effective tick rate at the configured target instead of running at
        // whatever rate the hardware allows. See A-07 in performance/runs/20260422_092931/.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // Pre-allocate buffer for Stage 5 to avoid per-tick allocations.
        // Encoder's max_encoded_size is used as a safe upper bound.
        let mut encode_buffer = vec![0u8; encoder.max_encoded_size()];

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
                        transport.as_mut(),
                        world.as_mut(),
                        encoder.as_ref(),
                        &mut encode_buffer,
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
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    pub async fn tick_step(
        &mut self,
        transport: &mut dyn GameTransport,
        world: &mut dyn WorldState,
        encoder: &dyn Encoder,
        encode_buffer: &mut [u8],
    ) {
        let tick = self.current_tick;
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
                if let Some((_, network_ids)) = self.authenticated_clients.remove(&client_id) {
                    for network_id in network_ids {
                        let _ = world.despawn_networked(network_id);
                    }
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
                        if let Some((_, network_ids)) = self.authenticated_clients.remove(&id) {
                            for network_id in network_ids {
                                let _ = world.despawn_networked(network_id);
                            }
                        }
                        self.auth_timestamps.remove(&id);
                        tracing::info!(client_id = ?id, "Client disconnected");
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::SessionClosed(id) => {
                        metrics::counter!("aetheris_transport_events_total", "type" => "session_closed")
                        .increment(1);
                        tracing::warn!(client_id = ?id, "WebTransport session closed");
                        if let Some((_, network_ids)) = self.authenticated_clients.remove(&id) {
                            for network_id in network_ids {
                                let _ = world.despawn_networked(network_id);
                            }
                        }
                        self.auth_timestamps.remove(&id);
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::StreamReset(id) => {
                        metrics::counter!("aetheris_transport_events_total", "type" => "stream_reset")
                        .increment(1);
                        tracing::error!(client_id = ?id, "WebTransport stream reset");
                        if let Some((_, network_ids)) = self.authenticated_clients.remove(&id) {
                            for network_id in network_ids {
                                let _ = world.despawn_networked(network_id);
                            }
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
                        if let Some((_, network_ids)) =
                            self.authenticated_clients.remove(&client_id)
                        {
                            for network_id in network_ids {
                                let _ = world.despawn_networked(network_id);
                            }
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
                            if let Some(jti) = self
                                .auth_service
                                .validate_and_get_jti(&session_token, Some(tick))
                            {
                                tracing::info!(?client_id, "Client authenticated successfully");

                                self.authenticated_clients
                                    .insert(client_id, (jti, Vec::new()));
                                // Record when auth completed so we can measure server-side
                                // possession latency (A-08 profiling metric).
                                self.auth_timestamps.insert(client_id, Instant::now());

                                tracing::info!(
                                    ?client_id,
                                    "[Auth] Client authenticated — waiting for StartSession to spawn ship"
                                );
                                continue;
                            }
                            tracing::warn!(
                                ?client_id,
                                "Client failed authentication (token rejected)"
                            );
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
                                if let Some((_, network_ids)) =
                                    self.authenticated_clients.get_mut(&client_id)
                                {
                                    network_ids.push(network_id);
                                }

                                tracing::info!(
                                    ?client_id,
                                    entity_type,
                                    new_entity_id = network_id.0,
                                    "[Spawn] Playground entity spawned — tracked for cleanup on disconnect"
                                );
                            } else {
                                tracing::warn!(?client_id, "Unauthorized Spawn attempt");
                                metrics::counter!("aetheris_unprivileged_packets_total")
                                    .increment(1);
                            }
                        }
                        NetworkEvent::StartSession { .. } => {
                            // Only allow one session ship per client.
                            let already_has_ship = self
                                .authenticated_clients
                                .get(&client_id)
                                .is_some_and(|(_, ids)| !ids.is_empty());

                            if already_has_ship {
                                tracing::warn!(
                                    ?client_id,
                                    "StartSession ignored — client already has a session ship"
                                );
                            } else {
                                let network_id =
                                    world.spawn_session_ship(1, 0.0, 0.0, 0.0, client_id);
                                if let Some((_, network_ids)) =
                                    self.authenticated_clients.get_mut(&client_id)
                                {
                                    network_ids.push(network_id); // index 0 = session ship
                                }

                                world.queue_reliable_event(
                                    Some(client_id),
                                    aetheris_protocol::events::GameEvent::Possession { network_id },
                                );

                                // Record server-side auth→possession latency (A-08 profiling).
                                // This measures only the server cost (spawn + event queue) after
                                // auth validation — not the client-observed round-trip time.
                                // If this histogram shows values near zero the 6 ms stretch miss
                                // in Time-to-Possess P99 is attributable to protocol handshake
                                // and tick-scheduling jitter, not server processing.
                                if let Some(auth_ts) = self.auth_timestamps.remove(&client_id) {
                                    metrics::histogram!("aetheris_session_start_latency_seconds")
                                        .record(auth_ts.elapsed().as_secs_f64());
                                }

                                tracing::info!(
                                    ?client_id,
                                    network_id = network_id.0,
                                    "[StartSession] Session ship spawned — Possession sent"
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
                                if let Some((_, ids)) =
                                    self.authenticated_clients.get_mut(&client_id)
                                {
                                    ids.clear();
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
            let _span = debug_span!("stage4_extract").entered();
            (world.extract_deltas(), world.extract_reliable_events())
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
                        if let Err(e) = transport.send_reliable(id, &data).await {
                            error!(error = ?e, client_id = ?id, "Failed to send reliable event");
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

            for delta in deltas {
                let encode_result = encoder.encode(&delta, encode_buffer);
                match encode_result {
                    Ok(len) if len > aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE => {
                        let targets = Self::get_delta_targets(
                            world,
                            &self.authenticated_clients,
                            delta.network_id,
                        );
                        match self
                            .fragment_and_send(encode_buffer, len, &targets, encoder, transport)
                            .await
                        {
                            Ok(count) => broadcast_count += count,
                            Err(e) => error!(error = ?e, "Failed to fragment and broadcast delta"),
                        }
                    }
                    Ok(len) => {
                        let targets = Self::get_delta_targets(
                            world,
                            &self.authenticated_clients,
                            delta.network_id,
                        );
                        if targets.is_empty() {
                            if let Err(e) =
                                transport.broadcast_unreliable(&encode_buffer[..len]).await
                            {
                                error!(error = ?e, "Failed to broadcast delta");
                            } else {
                                broadcast_count += 1;
                            }
                        } else if targets.len() == self.authenticated_clients.len() {
                            // A-05: Phase 1 single-room broadcast short-circuit.
                            //
                            // SEMANTICS NOTE — broadcast_count:
                            //   When sending individually, broadcast_count is incremented once
                            //   per successful per-client send (so +N for N clients).
                            //   When using broadcast_unreliable(), it is incremented only +1
                            //   regardless of how many clients receive the datagram.
                            //   This asymmetry is intentional and matches the existing pattern
                            //   for the `targets.is_empty()` broadcast path above.
                            //   The metric therefore counts *dispatch calls*, not *recipients*.
                            //
                            // CORRECTNESS CONSTRAINT — Phase 1 only:
                            //   `broadcast_unreliable()` sends to ALL connected clients, not
                            //   just the `targets` slice. This is only safe when `targets` ==
                            //   the full `authenticated_clients` set, i.e. when there is exactly
                            //   one room and every authenticated client is in it.
                            //
                            //   Phase 1 satisfies this: `get_delta_targets` returns all
                            //   authenticated clients when the entity has no room override.
                            //
                            //   ⚠ MUST REVERT before AoI / multi-room lands (Phase 2+).
                            //   When AoI introduces per-room filtering, `targets` will be a
                            //   strict subset of `authenticated_clients` and broadcasting to
                            //   everyone would send data to clients outside the entity's AoI,
                            //   breaking both correctness and the interest-management guarantee.
                            if let Err(e) =
                                transport.broadcast_unreliable(&encode_buffer[..len]).await
                            {
                                error!(error = ?e, "Failed to broadcast delta");
                            } else {
                                broadcast_count += 1;
                            }
                        } else {
                            for target in targets {
                                if let Err(e) = transport
                                    .send_unreliable(target, &encode_buffer[..len])
                                    .await
                                {
                                    error!(error = ?e, "Failed to send delta");
                                } else {
                                    broadcast_count += 1;
                                }
                            }
                        }
                    }
                    Err(EncodeError::BufferOverflow {
                        needed,
                        available: _,
                    }) => {
                        let mut large_buffer = vec![0u8; needed];
                        if let Ok(len) = encoder.encode(&delta, &mut large_buffer) {
                            let targets = Self::get_delta_targets(
                                world,
                                &self.authenticated_clients,
                                delta.network_id,
                            );
                            match self
                                .fragment_and_send(&large_buffer, len, &targets, encoder, transport)
                                .await
                            {
                                Ok(count) => broadcast_count += count,
                                Err(e) => {
                                    error!(error = ?e, "Failed to fragment and broadcast large delta");
                                }
                            }
                        } else {
                            error!("Failed to encode into large scratch buffer");
                        }
                    }
                    Err(e) => {
                        metrics::counter!("aetheris_encode_errors_total").increment(1);
                        error!(
                            network_id = ?delta.network_id,
                            error = ?e,
                            "Failed to encode delta"
                        );
                    }
                }
            }
            metrics::counter!("aetheris_packets_outbound_total").increment(broadcast_count);
            metrics::counter!("aetheris_packets_broadcast_total").increment(broadcast_count);
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "send")
            .record(t5.elapsed().as_secs_f64());

        // Stage 6: Finalize
        self.current_tick += 1;
    }

    fn get_delta_targets(
        world: &mut dyn WorldState,
        clients: &HashMap<
            aetheris_protocol::types::ClientId,
            (String, Vec<aetheris_protocol::types::NetworkId>),
        >,
        entity_id: aetheris_protocol::types::NetworkId,
    ) -> Vec<aetheris_protocol::types::ClientId> {
        if let Some(room_id) = world.get_entity_room(entity_id) {
            let mut targets = Vec::new();
            for &client_id in clients.keys() {
                if world.get_client_room(client_id) == Some(room_id) {
                    targets.push(client_id);
                }
            }
            targets
        } else {
            Vec::new() // Empty means broadcast
        }
    }

    async fn fragment_and_send(
        &mut self,
        data: &[u8],
        len: usize,
        targets: &[aetheris_protocol::types::ClientId],
        encoder: &dyn Encoder,
        transport: &dyn GameTransport,
    ) -> Result<u64, EncodeError> {
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
                client_id: aetheris_protocol::types::ClientId(0),
                fragment,
            };

            match encoder.encode_event(&fragment_event) {
                Ok(encoded_fragment) => {
                    if targets.is_empty() {
                        if let Err(e) = transport.broadcast_unreliable(&encoded_fragment).await {
                            error!(error = ?e, "Failed to broadcast fragment");
                        } else {
                            sent_count += 1;
                        }
                    } else {
                        for &target in targets {
                            if let Err(e) =
                                transport.send_unreliable(target, &encoded_fragment).await
                            {
                                error!(error = ?e, "Failed to send fragment");
                            } else {
                                sent_count += 1;
                            }
                        }
                    }
                }
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

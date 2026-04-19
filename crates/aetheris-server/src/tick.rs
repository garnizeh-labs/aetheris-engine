use std::collections::HashMap;
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

    /// Maps `ClientId` -> (Session JTI, spawned `NetworkId`)
    authenticated_clients:
        HashMap<aetheris_protocol::types::ClientId, (String, aetheris_protocol::types::NetworkId)>,
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
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Pre-allocate buffer for Stage 5 to avoid per-tick allocations.
        // Encoder's max_encoded_size is used as a safe upper bound.
        let mut encode_buffer = vec![0u8; encoder.max_encoded_size()];

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.current_tick += 1;

                    let start = Instant::now();
                    Self::tick_step(
                        transport.as_mut(),
                        world.as_mut(),
                        encoder.as_ref(),
                        &self.auth_service,
                        &mut self.authenticated_clients,
                        &mut self.reassembler,
                        &mut self.next_message_id,
                        &mut encode_buffer,
                        self.current_tick,
                    )
                    .instrument(info_span!("tick", tick = self.current_tick))
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
        transport: &mut dyn GameTransport,
        world: &mut dyn WorldState,
        encoder: &dyn Encoder,
        auth_service: &AuthServiceImpl,
        authenticated_clients: &mut HashMap<
            aetheris_protocol::types::ClientId,
            (String, aetheris_protocol::types::NetworkId),
        >,
        reassembler: &mut Reassembler,
        next_message_id: &mut u32,
        encode_buffer: &mut [u8],
        tick: u64,
    ) {
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
            for (&client_id, (jti, _)) in authenticated_clients.iter() {
                if !auth_service.is_session_authorized(jti, Some(tick)) {
                    tracing::warn!(?client_id, "Session invalidated during periodic check");
                    to_remove.push(client_id);
                }
            }
            for client_id in to_remove {
                if let Some((_, network_id)) = authenticated_clients.remove(&client_id) {
                    let _ = world.despawn_networked(network_id);
                }
                metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
            }
        }

        // Stage 2: Apply
        let t2 = Instant::now();
        let mut pong_responses = None;
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
                        if let Some(data) = reassembler.ingest(client_id, fragment) {
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
                            if let Some(reassembled) = reassembler.ingest(client_id, fragment) {
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
                        if let Some((_, network_id)) = authenticated_clients.remove(&id) {
                            let _ = world.despawn_networked(network_id);
                        }
                        tracing::info!(client_id = ?id, "Client disconnected");
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::Ping { client_id, tick } => {
                        if authenticated_clients.contains_key(&client_id) {
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
                    NetworkEvent::SessionClosed(id) => {
                        metrics::counter!("aetheris_transport_events_total", "type" => "session_closed")
                        .increment(1);
                        tracing::warn!(client_id = ?id, "WebTransport session closed");
                        if let Some((_, network_id)) = authenticated_clients.remove(&id) {
                            let _ = world.despawn_networked(network_id);
                        }
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::StreamReset(id) => {
                        metrics::counter!("aetheris_transport_events_total", "type" => "stream_reset")
                        .increment(1);
                        tracing::error!(client_id = ?id, "WebTransport stream reset");
                        if let Some((_, network_id)) = authenticated_clients.remove(&id) {
                            let _ = world.despawn_networked(network_id);
                        }
                        (id, Vec::new(), false)
                    }
                    NetworkEvent::Auth { .. }
                    | NetworkEvent::Pong { .. }
                    | NetworkEvent::StressTest { .. }
                    | NetworkEvent::Spawn { .. }
                    | NetworkEvent::ClearWorld { .. } => {
                        // All other events are handled later after decoding ReliableMessage
                        // or are not expected directly from the transport layer.
                        continue;
                    }
                };

                if !is_message {
                    continue;
                }

                // Stage 2.2: Auth & Protocol Decode
                let jti = if let Some((jti, _)) = authenticated_clients.get(&client_id) {
                    // Re-validate session on every message to refresh sliding window / catch revocation
                    if !auth_service.is_session_authorized(jti, Some(tick)) {
                        tracing::warn!(?client_id, "Session revoked; dropping client");
                        if let Some((_, network_id)) = authenticated_clients.remove(&client_id) {
                            let _ = world.despawn_networked(network_id);
                        }
                        metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
                        continue;
                    }
                    jti
                } else {
                    // Client not authenticated yet; only accept Auth message
                    if let Ok(NetworkEvent::Auth { session_token }) =
                        encoder.decode_event(&raw_data)
                    {
                        tracing::info!(?client_id, "Auth message received");
                        if let Some(jti) =
                            auth_service.validate_and_get_jti(&session_token, Some(tick))
                        {
                            tracing::info!(?client_id, "Client authenticated successfully");
                            let network_id = world.spawn_networked_for(client_id);
                            authenticated_clients.insert(client_id, (jti, network_id));
                            continue;
                        }
                        tracing::warn!(?client_id, "Client failed authentication");
                    } else {
                        tracing::debug!(
                            ?client_id,
                            "Discarding message from unauthenticated client"
                        );
                        metrics::counter!("aetheris_unprivileged_packets_total").increment(1);
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
                                tracing::info!(
                                    ?client_id,
                                    entity_type,
                                    x,
                                    y,
                                    "Spawn command executed"
                                );
                                world.spawn_kind(entity_type, x, y, rot);
                            } else {
                                tracing::warn!(?client_id, "Unauthorized Spawn attempt");
                                metrics::counter!("aetheris_unprivileged_packets_total")
                                    .increment(1);
                            }
                        }
                        NetworkEvent::ClearWorld { .. } => {
                            if can_run_playground_command(jti) {
                                tracing::info!(?client_id, "ClearWorld command executed");
                                world.clear_world();
                            } else {
                                tracing::warn!(?client_id, "Unauthorized ClearWorld attempt");
                                metrics::counter!("aetheris_unprivileged_packets_total")
                                    .increment(1);
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
            reassembler.prune();
        }
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "apply")
            .record(t2.elapsed().as_secs_f64());

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
        let deltas = {
            let _span = debug_span!("stage4_extract").entered();
            world.extract_deltas()
        };
        metrics::histogram!("aetheris_stage_duration_seconds", "stage" => "extract")
            .record(t4.elapsed().as_secs_f64());

        // Stage 5: Encode & Send
        let t5 = Instant::now();
        if !deltas.is_empty() {
            let mut broadcast_count: u64 = 0;

            let stage_span = debug_span!("stage5_send", count = deltas.len());
            let _guard = stage_span.enter();

            for delta in deltas {
                let encode_result = encoder.encode(&delta, encode_buffer);
                match encode_result {
                    Ok(len) if len > aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE => {
                        match Self::fragment_and_broadcast(
                            encode_buffer,
                            len,
                            next_message_id,
                            encoder,
                            transport,
                        )
                        .await
                        {
                            Ok(count) => broadcast_count += count,
                            Err(e) => error!(error = ?e, "Failed to fragment and broadcast delta"),
                        }
                    }
                    Ok(len) => {
                        if let Err(e) = transport.broadcast_unreliable(&encode_buffer[..len]).await
                        {
                            error!(error = ?e, "Failed to broadcast delta");
                        } else {
                            broadcast_count += 1;
                        }
                    }
                    Err(EncodeError::BufferOverflow {
                        needed,
                        available: _,
                    }) => {
                        let mut large_buffer = vec![0u8; needed];
                        if let Ok(len) = encoder.encode(&delta, &mut large_buffer) {
                            match Self::fragment_and_broadcast(
                                &large_buffer,
                                len,
                                next_message_id,
                                encoder,
                                transport,
                            )
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
    }

    async fn fragment_and_broadcast(
        data: &[u8],
        len: usize,
        next_message_id: &mut u32,
        encoder: &dyn Encoder,
        transport: &dyn GameTransport,
    ) -> Result<u64, EncodeError> {
        let message_id = *next_message_id;
        *next_message_id = next_message_id.wrapping_add(1);

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
                    if let Err(e) = transport.broadcast_unreliable(&encoded_fragment).await {
                        error!(error = ?e, "Failed to broadcast fragment");
                    } else {
                        sent_count += 1;
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Failed to encode fragment event");
                }
            }
        }

        Ok(sent_count)
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

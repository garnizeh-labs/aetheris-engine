//! Aetheris WebTransport-based transport logic.
#![cfg(not(target_arch = "wasm32"))]
//!
//! This crate implements the `GameTransport` trait using `wtransport` (QUIC/HTTP3).
//! It handles in-memory self-signed certificate generation and logs the SHA-256
//! hash required for browser-based client connections.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use rcgen::{CertificateParams, KeyPair};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::endpoint::endpoint_side::Server as ServerSide;
use wtransport::{Connection, Endpoint, Identity, ServerConfig};

use aetheris_protocol::events::NetworkEvent;
use aetheris_protocol::traits::{ClientId, GameTransport, TransportError};

type ConnectionMap = HashMap<ClientId, Connection>;

/// A WebTransport bridge that implements `GameTransport`.
pub struct WebTransportBridge {
    _endpoint: Arc<Endpoint<ServerSide>>,
    events: Arc<Mutex<VecDeque<NetworkEvent>>>,
    // Map of client IDs to their connections (simplified for Phase 1)
    connections: Arc<Mutex<ConnectionMap>>,
    connected_client_count: Arc<std::sync::atomic::AtomicUsize>,
    cert_hash: String,
}

impl WebTransportBridge {
    /// Creates a new WebTransport bridge bound to the specified address.
    ///
    /// Generates a fresh self-signed certificate in memory and logs its SHA-256 hash.
    ///
    /// # Panics
    /// Panics if the endpoint creation fails.
    pub async fn new(addr: SocketAddr) -> Self {
        let (identity, cert_hash) = generate_self_signed_identity().await;

        let config = ServerConfig::builder()
            .with_bind_address(addr)
            .with_identity(identity)
            .max_idle_timeout(Some(std::time::Duration::from_secs(30)))
            .expect("Invalid idle timeout")
            .keep_alive_interval(Some(std::time::Duration::from_secs(10)))
            .build();

        let endpoint = Endpoint::server(config).expect("Failed to create WebTransport endpoint");
        let endpoint = Arc::new(endpoint);
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let connections = Arc::new(Mutex::new(HashMap::new()));
        let connected_client_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let server = Self {
            _endpoint: Arc::clone(&endpoint),
            events,
            connections,
            connected_client_count,
            cert_hash,
        };

        server.spawn_listener(endpoint);

        server
    }

    fn spawn_listener(&self, endpoint: Arc<Endpoint<ServerSide>>) {
        let events = Arc::clone(&self.events);
        let connections = Arc::clone(&self.connections);
        let client_count = Arc::clone(&self.connected_client_count);

        tokio::spawn(async move {
            if let Ok(local_addr) = endpoint.local_addr() {
                info!(
                    "WebTransport listener task started (address: {:?})",
                    local_addr
                );
            }
            loop {
                info!("WebTransport waiting for next incoming session...");
                let incoming = endpoint.accept().await;
                info!("WebTransport received an incoming session attempt");
                let events_inner = Arc::clone(&events);
                let connections_inner = Arc::clone(&connections);
                let count_inner = Arc::clone(&client_count);

                tokio::spawn(async move {
                    handle_incoming_connection(
                        incoming,
                        events_inner,
                        connections_inner,
                        count_inner,
                    )
                    .await;
                });
            }
        });
    }
}

#[async_trait]
impl GameTransport for WebTransportBridge {
    #[tracing::instrument(skip(self, data), fields(client_id = %client_id.0, size = data.len()))]
    async fn send_unreliable(
        &self,
        client_id: ClientId,
        data: &[u8],
    ) -> Result<(), TransportError> {
        let mut conn_guard = self.connections.lock().await;
        let connection_map: &mut ConnectionMap = &mut conn_guard;
        if let Some(conn) = connection_map.get_mut(&client_id) {
            let conn: &mut Connection = conn;
            if let Err(e) = conn.send_datagram(data) {
                metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "datagram_send_fail").increment(1);
                return Err(TransportError::Io(std::io::Error::other(format!(
                    "{:?}",
                    e
                ))));
            }
            metrics::counter!("aetheris_transport_packets_total", "transport" => "webtransport", "direction" => "outbound", "channel" => "unreliable").increment(1);
            metrics::counter!("aetheris_transport_bytes_total", "transport" => "webtransport", "direction" => "outbound", "channel" => "unreliable").increment(data.len() as u64);
            Ok(())
        } else {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "client_not_connected").increment(1);
            Err(TransportError::ClientNotConnected(client_id))
        }
    }

    #[tracing::instrument(skip(self, data), fields(client_id = %client_id.0, size = data.len()))]
    async fn send_reliable(&self, client_id: ClientId, data: &[u8]) -> Result<(), TransportError> {
        let conn = {
            let conn_guard = self.connections.lock().await;
            conn_guard.get(&client_id).cloned()
        };

        if let Some(conn) = conn {
            // wtransport 0.7.0 uses double await pattern for open_bi()
            match conn.open_bi().await {
                Ok(opening) => match opening.await {
                    Ok((mut send_stream, _recv_stream)) => {
                        send_stream.write_all(data).await.map_err(|e| {
                            metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "stream_write_fail").increment(1);
                            TransportError::Io(std::io::Error::other(format!(
                                "Failed to send reliable data: {}",
                                e
                            )))
                        })?;
                        send_stream.finish().await.map_err(|e| {
                            metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "stream_finish_fail").increment(1);
                            TransportError::Io(std::io::Error::other(format!(
                                "Failed to finish reliable stream: {}",
                                e
                            )))
                        })?;
                        metrics::counter!("aetheris_transport_packets_total", "transport" => "webtransport", "direction" => "outbound", "channel" => "reliable").increment(1);
                        metrics::counter!("aetheris_transport_bytes_total", "transport" => "webtransport", "direction" => "outbound", "channel" => "reliable").increment(data.len() as u64);
                        Ok(())
                    }
                    Err(e) => {
                        metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "stream_open_fail").increment(1);
                        Err(TransportError::Io(std::io::Error::other(format!(
                            "Failed to establish bidirectional stream: {}",
                            e
                        ))))
                    }
                },
                Err(e) => {
                    metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "stream_init_fail").increment(1);
                    Err(TransportError::Io(std::io::Error::other(format!(
                        "Failed to initiate bidirectional stream: {}",
                        e
                    ))))
                }
            }
        } else {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "client_not_connected").increment(1);
            Err(TransportError::ClientNotConnected(client_id))
        }
    }

    #[tracing::instrument(skip(self, data), fields(size = data.len()))]
    async fn broadcast_unreliable(&self, data: &[u8]) -> Result<(), TransportError> {
        let mut conn_guard = self.connections.lock().await;
        let connection_map: &mut ConnectionMap = &mut conn_guard;
        for (client_id, conn) in connection_map.iter_mut() {
            if let Err(e) = conn.send_datagram(data) {
                metrics::counter!("aetheris_transport_errors_total", "transport" => "webtransport", "type" => "broadcast_fail").increment(1);
                warn!(
                    "Failed to broadcast unreliable datagram to client {:?}: {:?}",
                    client_id, e
                );
            } else {
                metrics::counter!("aetheris_transport_packets_total", "transport" => "webtransport", "direction" => "outbound", "channel" => "broadcast_unreliable").increment(1);
                metrics::counter!("aetheris_transport_bytes_total", "transport" => "webtransport", "direction" => "outbound", "channel" => "broadcast_unreliable").increment(data.len() as u64);
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    async fn poll_events(&mut self) -> Result<Vec<NetworkEvent>, TransportError> {
        let mut events = self.events.lock().await;
        Ok(events.drain(..).collect())
    }

    async fn connected_client_count(&self) -> usize {
        self.connected_client_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl WebTransportBridge {
    /// Returns the SHA-256 hash of the server's self-signed certificate (Base64).
    #[must_use]
    pub fn cert_hash(&self) -> &str {
        &self.cert_hash
    }
}

async fn handle_incoming_connection(
    incoming: IncomingSession,
    events: Arc<Mutex<VecDeque<NetworkEvent>>>,
    connections: Arc<Mutex<ConnectionMap>>,
    connected_client_count: Arc<std::sync::atomic::AtomicUsize>,
) {
    info!("Handling incoming WebTransport connection...");
    let session_request = match incoming.await {
        Ok(r) => {
            info!(
                "WebTransport session request received from {:?}",
                r.remote_address()
            );
            r
        }
        Err(e) => {
            warn!(
                "Failed to accept incoming WebTransport session request: {}",
                e
            );
            return;
        }
    };

    let connection = match session_request.accept().await {
        Ok(c) => {
            info!(
                "WebTransport connection accepted for {:?}",
                c.remote_address()
            );
            c
        }
        Err(e) => {
            warn!("Failed to accept WebTransport connection: {}", e);
            return;
        }
    };

    let client_id = ClientId(rand::random());
    {
        let mut conn_guard = connections.lock().await;
        let connection_map: &mut ConnectionMap = &mut conn_guard;
        connection_map.insert(client_id, connection.clone());
    }

    connected_client_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    {
        let mut events_guard = events.lock().await;
        events_guard.push_back(NetworkEvent::ClientConnected(client_id));
    }

    info!("Client connected via WebTransport: {:?}", client_id);

    // Spawn task to read datagrams and streams for this connection
    let conn_clone = connection.clone();
    let events_clone = Arc::clone(&events);
    let connections_clone = Arc::clone(&connections);
    let count_clone = Arc::clone(&connected_client_count);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                datagram = conn_clone.receive_datagram() => {
                    match datagram {
                        Ok(data) => {
                            let mut events_guard = events_clone.lock().await;
                            events_guard.push_back(NetworkEvent::UnreliableMessage {
                                client_id,
                                data: data.to_vec(),
                            });
                            metrics::counter!("aetheris_transport_packets_total", "transport" => "webtransport", "direction" => "inbound", "channel" => "unreliable").increment(1);
                            metrics::counter!("aetheris_transport_bytes_total", "transport" => "webtransport", "direction" => "inbound", "channel" => "unreliable").increment(data.len() as u64);
                        }
                        Err(e) => {
                            warn!("WebTransport receive_datagram failed for client {:?}: {:?}", client_id, e);
                            let mut events_guard = events_clone.lock().await;
                            events_guard.push_back(NetworkEvent::SessionClosed(client_id));
                            break;
                        }
                    }
                }
                stream_res = conn_clone.accept_bi() => {
                    match stream_res {
                        Ok(bi) => {
                            let events_inner = Arc::clone(&events_clone);
                            tokio::spawn(async move {
                                use tokio::io::AsyncReadExt;
                                const MAX_RELIABLE_PAYLOAD_SIZE: usize = 1024 * 1024; // 1MB limit for reliable messages

                                let mut buffer = Vec::new();
                                // bi is (SendStream, RecvStream)
                                // Use take() to limit the number of bytes read (plus one to detect overflow)
                                let mut limited_reader = bi.1.take(MAX_RELIABLE_PAYLOAD_SIZE as u64 + 1);

                                if let Err(e) = limited_reader.read_to_end(&mut buffer).await {
                                    error!("Failed to read reliable stream for client {:?}: {}", client_id, e);
                                    let mut events_guard = events_inner.lock().await;
                                    events_guard.push_back(NetworkEvent::StreamReset(client_id));
                                    return;
                                }

                                // If we read more than the limit, it's a violation
                                if buffer.len() > MAX_RELIABLE_PAYLOAD_SIZE {
                                    error!("Reliable message exceeded maximum size ({}) from client {:?}", MAX_RELIABLE_PAYLOAD_SIZE, client_id);
                                    return;
                                }

                                {
                                    let mut events_guard = events_inner.lock().await;
                                    let buffer_len = buffer.len() as u64;
                                    events_guard.push_back(NetworkEvent::ReliableMessage {
                                        client_id,
                                        data: buffer,
                                    });
                                    metrics::counter!("aetheris_transport_packets_total", "transport" => "webtransport", "direction" => "inbound", "channel" => "reliable").increment(1);
                                    metrics::counter!("aetheris_transport_bytes_total", "transport" => "webtransport", "direction" => "inbound", "channel" => "reliable").increment(buffer_len);
                                }
                            });
                        }
                        Err(e) => {
                            warn!("WebTransport accept_bi failed for client {:?}: {:?}", client_id, e);
                            break;
                        }
                    }
                }
            }
        }

        warn!("Client disconnected via WebTransport: {:?}", client_id);
        count_clone.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut conn_guard = connections_clone.lock().await;
            conn_guard.remove(&client_id);
        }

        let mut events_guard = events_clone.lock().await;
        events_guard.push_back(NetworkEvent::ClientDisconnected(client_id));
    });
}

async fn generate_self_signed_identity() -> (Identity, String) {
    let cert_dir = std::path::PathBuf::from("target/dev-certs");
    let cert_path = cert_dir.join("cert.pem");
    let key_path = cert_dir.join("key.pem");
    let hash_path = cert_dir.join("cert.sha256");

    if cert_path.exists() && key_path.exists() && hash_path.exists() {
        match (
            tokio::fs::read_to_string(&hash_path).await,
            Identity::load_pemfiles(&cert_path, &key_path).await,
        ) {
            (Ok(hash_b64), Ok(identity)) => {
                info!("--------------------------------------------------");
                info!("WEBTRANSPORT SELF-SIGNED CERTIFICATE LOADED");
                info!("SHA-256 Hash (Base64): {}", hash_b64.trim());
                info!("(Delete target/dev-certs/ to force regeneration)");
                info!("--------------------------------------------------");
                return (identity, hash_b64.trim().to_string());
            }
            (hash_err, identity_err) => {
                warn!(
                    "Failed to load persistent certificate (hash_err: {:?}, id_err: {:?}). Regenerating...",
                    hash_err.is_err(),
                    identity_err.is_err()
                );
                // Fall through to regeneration
            }
        }
    }

    // First run: generate a new certificate and persist it.
    // CRITICAL: Chrome's serverCertificateHashes requires validity <= 14 days.
    // rcgen::generate_simple_self_signed defaults to 100 years — so we must
    // use CertificateParams and set not_after explicitly.
    let mut params = CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("Failed to create cert params");
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = time::OffsetDateTime::now_utc()
        .checked_add(time::Duration::days(13))
        .expect("Date overflow");

    let key_pair = KeyPair::generate().expect("Failed to generate key pair");
    let cert = params
        .self_signed(&key_pair)
        .expect("Failed to self-sign cert");

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let cert_der = cert.der();
    let mut hasher = Sha256::new();
    hasher.update(cert_der.as_ref());
    let hash = hasher.finalize();
    let hash_b64 = base64::Engine::encode(&base64::prelude::BASE64_STANDARD, hash);

    tokio::fs::create_dir_all(&cert_dir)
        .await
        .expect("Failed to create cert directory");

    tokio::fs::write(&cert_path, &cert_pem)
        .await
        .expect("Failed to write cert");
    tokio::fs::write(&key_path, &key_pem)
        .await
        .expect("Failed to write key");
    tokio::fs::write(&hash_path, &hash_b64)
        .await
        .expect("Failed to write cert hash");

    info!("--------------------------------------------------");
    info!("WEBTRANSPORT SELF-SIGNED CERTIFICATE GENERATED");
    info!("SHA-256 Hash (Base64): {}", hash_b64);
    info!("Valid for: 13 days (Chrome serverCertificateHashes constraint: <= 14 days)");
    info!("Saved to: {}", cert_dir.display());
    info!("--------------------------------------------------");

    Identity::load_pemfiles(&cert_path, &key_path)
        .await
        .map(|id| (id, hash_b64))
        .expect("Failed to load identity from persistent files")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_concurrent_send_unreliable_load() {
        // Start server
        let addr = "127.0.0.1:0".parse().unwrap();
        let mut server = WebTransportBridge::new(addr).await;
        let server_addr = server._endpoint.local_addr().unwrap();

        let num_clients = 100;
        let mut client_tasks = Vec::new();

        // Spawn 100 clients
        let server_hash = server.cert_hash().to_string();
        for i in 0..num_clients {
            let hash_str = server_hash.clone();
            client_tasks.push(tokio::spawn(async move {
                let hash_bytes =
                    base64::Engine::decode(&base64::prelude::BASE64_STANDARD, hash_str.trim())
                        .expect("Failed to decode base64 hash");

                let hash = wtransport::tls::Sha256Digest::new(
                    hash_bytes.try_into().expect("Invalid hash length"),
                );

                let config = wtransport::ClientConfig::builder()
                    .with_bind_address("127.0.0.1:0".parse().unwrap())
                    .with_server_certificate_hashes(vec![hash])
                    .build();

                let endpoint = Endpoint::client(config).expect("Failed to create client endpoint");

                let url = format!("https://{}/", server_addr);

                let connection = match timeout(Duration::from_secs(5), endpoint.connect(&url)).await
                {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(e)) => panic!("Client {} failed to connect: {:?}", i, e),
                    Err(_) => panic!("Client {} connection timed out", i),
                };

                // Stagger sends to avoid overwhelming socket buffers
                tokio::time::sleep(Duration::from_millis(i as u64 * 10)).await;
                let msg = format!("message from client {}", i);
                connection
                    .send_datagram(msg.as_bytes())
                    .expect("Failed to send datagram");

                // Keep connection alive longer to ensure we can count them
                tokio::time::sleep(Duration::from_secs(2)).await;
            }));
        }

        // Poll server events
        let mut connected_count = 0;
        let mut message_count = 0;
        let mut peak_client_count = 0;
        let start = std::time::Instant::now();

        while (connected_count < num_clients || message_count < num_clients)
            && start.elapsed() < Duration::from_secs(20)
        {
            let events = server.poll_events().await.unwrap();
            for event in events {
                match event {
                    NetworkEvent::ClientConnected(_) => connected_count += 1,
                    NetworkEvent::UnreliableMessage { .. } => message_count += 1,
                    _ => {}
                }
            }

            let current_count = server.connected_client_count().await;
            if current_count > peak_client_count {
                peak_client_count = current_count;
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 5. Final validation of counts and messages
        for task in client_tasks {
            task.await.expect("Client task panicked or failed");
        }

        assert!(
            connected_count >= num_clients,
            "Only {}/{} clients connected at some point",
            connected_count,
            num_clients
        );
        assert!(
            peak_client_count > 0,
            "No clients were ever recorded as connected in the atomic counter"
        );
        // We allow for a very small amount of packet loss even on loopback if buffers overflow,
        // but it should ideally be 100%. We'll settle for 95% to avoid flaky tests on busy CI.
        assert!(
            message_count >= 95,
            "Only {}/{} messages received",
            message_count,
            num_clients
        );
    }
}

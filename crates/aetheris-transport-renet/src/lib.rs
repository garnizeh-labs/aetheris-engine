//! Aetheris Renet-based transport logic.
//!
//! **Phase:** P1 - MVP Implementation
//! **Constraint:** Leverages UDP with renet-specific reliability channels.
//! **Purpose:** Provides a rapid-iteration transport layer for the Data Plane using
//! established UDP abstraction libraries.

#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::too_many_lines)]
#![cfg(not(target_arch = "wasm32"))]

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use renet::{ChannelConfig, ConnectionConfig, RenetServer, SendType, ServerEvent};
use renet_netcode::{NetcodeServerTransport, ServerConfig};

use aetheris_protocol::MAX_SAFE_PAYLOAD_SIZE;
use aetheris_protocol::error::TransportError;
use aetheris_protocol::events::NetworkEvent;
use aetheris_protocol::traits::GameTransport;
use aetheris_protocol::types::ClientId;

/// Renet-based implementation of the [`GameTransport`] trait.
pub struct RenetTransport {
    server: Mutex<RenetServer>,
    transport: Mutex<NetcodeServerTransport>,
    last_update: Mutex<Instant>,
    local_addr: SocketAddr,
    rate_limiter: Mutex<IpRateLimiter>,
    max_payload_size: usize,
    last_prune: Mutex<Instant>,
    suppressed_disconnects: Mutex<std::collections::HashSet<u64>>,
}

/// Configuration for the Renet server.
pub struct RenetServerConfig {
    /// Unique protocol identifier.
    pub protocol_id: u64,
    /// Maximum number of allowed clients.
    pub max_clients: usize,
    /// Authentication method (e.g., Unsecure, Secure).
    pub authentication: renet_netcode::ServerAuthentication,
    /// Maximum number of new connections allowed per second from a single IP.
    pub max_new_connections_per_second: u32,
    /// Maximum inbound payload size (MTU).
    pub max_payload_size: usize,
}

impl Default for RenetServerConfig {
    fn default() -> Self {
        Self {
            protocol_id: 0,
            max_clients: 1000,
            authentication: renet_netcode::ServerAuthentication::Unsecure,
            max_new_connections_per_second: 5,
            max_payload_size: MAX_SAFE_PAYLOAD_SIZE,
        }
    }
}

/// Simple token-bucket rate limiter for IPs.
struct IpRateLimiter {
    limits: std::collections::HashMap<std::net::IpAddr, TokenBucket>,
    max_rate: f64,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl IpRateLimiter {
    fn new(max_rate: f64) -> Self {
        Self {
            limits: std::collections::HashMap::new(),
            max_rate,
        }
    }

    fn check(&mut self, ip: std::net::IpAddr) -> bool {
        let now = Instant::now();
        let bucket = self.limits.entry(ip).or_insert_with(|| TokenBucket {
            tokens: self.max_rate,
            last_refill: now,
        });

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.max_rate).min(self.max_rate);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Prunes old entries from the rate limiter to prevent memory leaks.
    fn prune(&mut self) {
        let now = Instant::now();
        // Remove entries that haven't been seen in 10 minutes and are full
        self.limits.retain(|_ip, bucket| {
            let full = bucket.tokens >= self.max_rate - 0.1;
            let idle = now.duration_since(bucket.last_refill) > Duration::from_mins(10);
            !(full && idle)
        });
    }
}

/// Channel 0: Unreliable messaging.
pub const CHANNEL_UNRELIABLE: u8 = 0;
/// Channel 1: Reliable, ordered messaging.
pub const CHANNEL_RELIABLE: u8 = 1;

impl RenetTransport {
    /// Creates a new Renet transport bound to the specified address.
    ///
    /// If `config` is `None`, default settings (Protocol 0, 1000 clients, Unsecure) are used.
    ///
    /// # Errors
    /// Returns a [`TransportError::Io`] if the socket fails to bind.
    pub fn new_server(
        addr: SocketAddr,
        config: Option<RenetServerConfig>,
    ) -> Result<Self, TransportError> {
        let config = config.unwrap_or_default();
        let connection_config = ConnectionConfig {
            server_channels_config: vec![
                ChannelConfig {
                    channel_id: CHANNEL_UNRELIABLE,
                    max_memory_usage_bytes: 1024 * 1024,
                    send_type: SendType::Unreliable,
                },
                ChannelConfig {
                    channel_id: CHANNEL_RELIABLE,
                    max_memory_usage_bytes: 1024 * 1024,
                    send_type: SendType::ReliableOrdered {
                        resend_time: Duration::from_millis(300),
                    },
                },
            ],
            ..Default::default()
        };

        let server = RenetServer::new(connection_config);

        let server_config = ServerConfig {
            current_time: Duration::ZERO,
            max_clients: config.max_clients,
            protocol_id: config.protocol_id,
            public_addresses: vec![addr],
            authentication: config.authentication,
        };

        let socket = std::net::UdpSocket::bind(addr).map_err(TransportError::Io)?;
        let local_addr = socket.local_addr().map_err(TransportError::Io)?;

        let transport = NetcodeServerTransport::new(server_config, socket)
            .map_err(|e| TransportError::Io(std::io::Error::other(e)))?;

        Ok(Self {
            server: Mutex::new(server),
            transport: Mutex::new(transport),
            last_update: Mutex::new(Instant::now()),
            local_addr,
            rate_limiter: Mutex::new(IpRateLimiter::new(f64::from(
                config.max_new_connections_per_second,
            ))),
            max_payload_size: config.max_payload_size,
            last_prune: Mutex::new(Instant::now()),
            suppressed_disconnects: Mutex::new(std::collections::HashSet::new()),
        })
    }

    /// Returns the local address the transport is bound to.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.local_addr
    }
}

#[async_trait]
impl GameTransport for RenetTransport {
    #[tracing::instrument(skip(self, data), fields(client_id = %client_id.0, size = data.len()))]
    async fn send_unreliable(
        &self,
        client_id: ClientId,
        data: &[u8],
    ) -> Result<(), TransportError> {
        if data.len() > MAX_SAFE_PAYLOAD_SIZE {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "payload_too_large").increment(1);
            return Err(TransportError::PayloadTooLarge {
                size: data.len(),
                max: MAX_SAFE_PAYLOAD_SIZE,
            });
        }

        let mut server = self
            .server
            .lock()
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;

        if !server.is_connected(client_id.0) {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "client_not_connected").increment(1);
            return Err(TransportError::ClientNotConnected(client_id));
        }

        server.send_message(client_id.0, CHANNEL_UNRELIABLE, data.to_vec());
        metrics::counter!("aetheris_transport_packets_total", "transport" => "renet", "direction" => "outbound", "channel" => "unreliable").increment(1);
        metrics::counter!("aetheris_transport_bytes_total", "transport" => "renet", "direction" => "outbound", "channel" => "unreliable").increment(data.len() as u64);
        Ok(())
    }

    #[tracing::instrument(skip(self, data), fields(client_id = %client_id.0, size = data.len()))]
    async fn send_reliable(&self, client_id: ClientId, data: &[u8]) -> Result<(), TransportError> {
        if data.len() > MAX_SAFE_PAYLOAD_SIZE {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "payload_too_large").increment(1);
            return Err(TransportError::PayloadTooLarge {
                size: data.len(),
                max: MAX_SAFE_PAYLOAD_SIZE,
            });
        }

        let mut server = self
            .server
            .lock()
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;

        if !server.is_connected(client_id.0) {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "client_not_connected").increment(1);
            return Err(TransportError::ClientNotConnected(client_id));
        }

        server.send_message(client_id.0, CHANNEL_RELIABLE, data.to_vec());
        metrics::counter!("aetheris_transport_packets_total", "transport" => "renet", "direction" => "outbound", "channel" => "reliable").increment(1);
        metrics::counter!("aetheris_transport_bytes_total", "transport" => "renet", "direction" => "outbound", "channel" => "reliable").increment(data.len() as u64);
        Ok(())
    }

    #[tracing::instrument(skip(self, data), fields(size = data.len()))]
    async fn broadcast_unreliable(&self, data: &[u8]) -> Result<(), TransportError> {
        if data.len() > MAX_SAFE_PAYLOAD_SIZE {
            metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "payload_too_large").increment(1);
            return Err(TransportError::PayloadTooLarge {
                size: data.len(),
                max: MAX_SAFE_PAYLOAD_SIZE,
            });
        }

        let mut server = self
            .server
            .lock()
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
        server.broadcast_message(CHANNEL_UNRELIABLE, data.to_vec());
        metrics::counter!("aetheris_transport_packets_total", "transport" => "renet", "direction" => "outbound", "channel" => "broadcast_unreliable").increment(1);
        metrics::counter!("aetheris_transport_bytes_total", "transport" => "renet", "direction" => "outbound", "channel" => "broadcast_unreliable").increment(data.len() as u64);
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    async fn poll_events(&mut self) -> Result<Vec<NetworkEvent>, TransportError> {
        let now = Instant::now();

        let duration = {
            let mut last_update = self
                .last_update
                .lock()
                .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
            let d = now.duration_since(*last_update);
            *last_update = now;
            d
        };

        {
            let mut last_prune = self
                .last_prune
                .lock()
                .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
            if now.duration_since(*last_prune) > Duration::from_mins(1) {
                let mut rate_limiter = self
                    .rate_limiter
                    .lock()
                    .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
                rate_limiter.prune();
                *last_prune = now;
            }
        }

        let mut events = Vec::new();
        let mut server = self
            .server
            .lock()
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
        let mut transport = self
            .transport
            .lock()
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;

        if let Err(e) = transport.update(duration, &mut server) {
            tracing::error!(error = ?e, "Netcode transport update failure");
        }
        server.update(duration);
        transport.send_packets(&mut server);

        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    let addr = transport.client_addr(client_id);
                    let allowed = if let Some(addr) = addr {
                        let mut rate_limiter = self
                            .rate_limiter
                            .lock()
                            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
                        rate_limiter.check(addr.ip())
                    } else {
                        true
                    };

                    if allowed {
                        events.push(NetworkEvent::ClientConnected(ClientId(client_id)));
                    } else {
                        tracing::warn!(
                            client_id,
                            ?addr,
                            "Connection rate limit exceeded, disconnecting"
                        );
                        // Suppress both Connected and future Disconnected events for this client
                        // to satisfy tests while keeping metrics balanced (delta = 0).
                        let mut suppressed = self
                            .suppressed_disconnects
                            .lock()
                            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
                        suppressed.insert(client_id);
                        server.disconnect(client_id);
                    }
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    let mut suppressed = self
                        .suppressed_disconnects
                        .lock()
                        .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;

                    if suppressed.remove(&client_id) {
                        tracing::debug!(client_id, "Suppressed rate-limited disconnect event");
                    } else {
                        tracing::debug!(client_id, ?reason, "Client disconnected");
                        events.push(NetworkEvent::ClientDisconnected(ClientId(client_id)));
                    }
                }
            }
        }

        // Drop transport lock before processing messages to minimize contention
        drop(transport);

        let max_payload = self.max_payload_size;
        let client_ids: Vec<u64> = server.clients_id();
        for client_id in &client_ids {
            while let Some(message) = server.receive_message(*client_id, CHANNEL_UNRELIABLE) {
                if message.len() > max_payload {
                    tracing::warn!(
                        client_id,
                        size = message.len(),
                        limit = max_payload,
                        "Discarding oversized unreliable message"
                    );
                    metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "oversized_unreliable_msg").increment(1);
                    continue;
                }
                events.push(NetworkEvent::UnreliableMessage {
                    client_id: ClientId(*client_id),
                    data: message.to_vec(),
                });
                metrics::counter!("aetheris_transport_packets_total", "transport" => "renet", "direction" => "inbound", "channel" => "unreliable").increment(1);
                metrics::counter!("aetheris_transport_bytes_total", "transport" => "renet", "direction" => "inbound", "channel" => "unreliable").increment(message.len() as u64);
            }
            while let Some(message) = server.receive_message(*client_id, CHANNEL_RELIABLE) {
                if message.len() > max_payload {
                    tracing::warn!(
                        client_id,
                        size = message.len(),
                        limit = max_payload,
                        "Discarding oversized reliable message"
                    );
                    metrics::counter!("aetheris_transport_errors_total", "transport" => "renet", "type" => "oversized_reliable_msg").increment(1);
                    continue;
                }
                events.push(NetworkEvent::ReliableMessage {
                    client_id: ClientId(*client_id),
                    data: message.to_vec(),
                });
                metrics::counter!("aetheris_transport_packets_total", "transport" => "renet", "direction" => "inbound", "channel" => "reliable").increment(1);
                metrics::counter!("aetheris_transport_bytes_total", "transport" => "renet", "direction" => "inbound", "channel" => "reliable").increment(message.len() as u64);
            }
        }

        // Report aggregate packet loss as datagram drop rate
        let mut total_loss = 0.0;
        let mut connected_count = 0;
        for client_id in &client_ids {
            if let Ok(info) = server.network_info(*client_id) {
                total_loss += info.packet_loss;
                connected_count += 1;
            }
        }
        if connected_count > 0 {
            metrics::gauge!("aetheris_datagram_drop_rate")
                .set(total_loss / f64::from(connected_count));
        }

        Ok(events)
    }

    async fn connected_client_count(&self) -> usize {
        let Ok(server) = self.server.lock() else {
            return 0; // Or panic? Given the poll_events change, we might want this to return Result too, but for now we follow the pattern.
        };
        server.connected_clients()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renet::RenetClient;
    use renet_netcode::NetcodeClientTransport;

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_renet_loopback_connectivity() {
        let addr = "127.0.0.1:0".parse().unwrap();
        let mut server_transport = RenetTransport::new_server(addr, None).unwrap();
        let server_addr = server_transport.addr();

        // Setup client
        let connection_config = ConnectionConfig::default();
        let mut client = RenetClient::new(connection_config);

        let client_id = 42;
        let auth = renet_netcode::ClientAuthentication::Unsecure {
            protocol_id: 0,
            client_id,
            server_addr,
            user_data: None,
        };

        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut client_transport =
            NetcodeClientTransport::new(Duration::ZERO, auth, socket).unwrap();

        // Connect and poll until connected
        let mut connected = false;
        let duration = Duration::from_millis(10);
        for _ in 0..100 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            client_transport.send_packets(&mut client).unwrap();

            let events = server_transport.poll_events().await.unwrap();
            for event in events {
                if let NetworkEvent::ClientConnected(id) = event
                    && id.0 == client_id
                {
                    connected = true;
                }
            }

            if connected {
                break;
            }
            tokio::time::sleep(duration).await;
        }

        assert!(connected, "Client failed to connect to server");

        // Send message from client to server
        let msg = b"hello aetheris";
        client.send_message(CHANNEL_UNRELIABLE, msg.to_vec());

        // Poll to receive
        let mut received = false;
        for _ in 0..100 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            client_transport.send_packets(&mut client).unwrap();

            let events = server_transport.poll_events().await.unwrap();
            for event in events {
                if let NetworkEvent::UnreliableMessage {
                    client_id: id,
                    data,
                } = event
                    && id.0 == client_id
                    && data == msg
                {
                    received = true;
                }
            }
            if received {
                break;
            }
            tokio::time::sleep(duration).await;
        }

        assert!(received, "Server failed to receive message from client");

        // Send message from server to client
        let server_msg = b"welcome to aetheris";
        server_transport
            .send_reliable(ClientId(client_id), server_msg)
            .await
            .unwrap();

        // Poll client to receive
        let mut client_received = false;
        for _ in 0..100 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            client_transport.send_packets(&mut client).unwrap();

            while let Some(data) = client.receive_message(CHANNEL_RELIABLE) {
                if &data[..] == server_msg {
                    client_received = true;
                }
            }

            server_transport.poll_events().await.unwrap(); // Keep server alive

            if client_received {
                break;
            }
            tokio::time::sleep(duration).await;
        }

        assert!(
            client_received,
            "Client failed to receive message from server"
        );

        // 1) Test broadcast_unreliable
        let broadcast_msg = b"broadcast message";
        server_transport
            .broadcast_unreliable(broadcast_msg)
            .await
            .unwrap();

        let mut broadcast_received = false;
        for _ in 0..100 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            client_transport.send_packets(&mut client).unwrap();

            while let Some(data) = client.receive_message(CHANNEL_UNRELIABLE) {
                if &data[..] == broadcast_msg {
                    broadcast_received = true;
                }
            }
            server_transport.poll_events().await.unwrap();
            if broadcast_received {
                break;
            }
            tokio::time::sleep(duration).await;
        }
        assert!(
            broadcast_received,
            "Client failed to receive broadcast message from server"
        );

        // 2) Verify connected_client_count
        assert_eq!(server_transport.connected_client_count().await, 1);

        // 3) Disconnect client and verify count drops to 0
        client_transport.disconnect();
        for _ in 0..10 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            let _ = client_transport.send_packets(&mut client);
            tokio::time::sleep(duration).await;
        }

        // 4) Poll server until ClientDisconnected is observed
        let mut disconnected = false;
        for _ in 0..100 {
            let events = server_transport.poll_events().await.unwrap();
            for event in events {
                if let NetworkEvent::ClientDisconnected(id) = event
                    && id.0 == client_id
                {
                    disconnected = true;
                }
            }
            if disconnected {
                break;
            }
            tokio::time::sleep(duration).await;
        }
        assert!(
            disconnected,
            "Server failed to observe client disconnection"
        );
        assert_eq!(server_transport.connected_client_count().await, 0);
    }

    #[tokio::test]
    async fn test_inbound_payload_size_limit() {
        let addr = "127.0.0.1:0".parse().unwrap();
        let mut server_transport = RenetTransport::new_server(
            addr,
            Some(RenetServerConfig {
                max_payload_size: 10,
                ..Default::default()
            }),
        )
        .unwrap();
        let server_addr = server_transport.addr();

        // Setup client with larger allowed MTU to allow sending 11 bytes
        let connection_config = ConnectionConfig::default();
        let mut client = RenetClient::new(connection_config);

        let client_id = 99;
        let auth = renet_netcode::ClientAuthentication::Unsecure {
            protocol_id: 0,
            client_id,
            server_addr,
            user_data: None,
        };

        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut client_transport =
            NetcodeClientTransport::new(Duration::ZERO, auth, socket).unwrap();

        let duration = Duration::from_millis(10);
        // Connect
        for _ in 0..50 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            let _ = client_transport.send_packets(&mut client);
            server_transport.poll_events().await.unwrap();
            tokio::time::sleep(duration).await;
        }

        // Send 11 bytes (limit is 10)
        let too_large_msg = vec![0u8; 11];
        client.send_message(CHANNEL_UNRELIABLE, too_large_msg);

        // Poll server and verify message is NOT received
        let mut received = false;
        for _ in 0..50 {
            let _ = client_transport.update(duration, &mut client);
            client.update(duration);
            let _ = client_transport.send_packets(&mut client);
            let events = server_transport.poll_events().await.unwrap();
            for event in events {
                if let NetworkEvent::UnreliableMessage { .. } = event {
                    received = true;
                }
            }
            if received {
                break;
            }
            tokio::time::sleep(duration).await;
        }

        assert!(
            !received,
            "Server should have discarded the oversized message"
        );
    }

    #[tokio::test]
    async fn test_connection_rate_limit() {
        let addr = "127.0.0.1:0".parse().unwrap();
        let mut server_transport = RenetTransport::new_server(
            addr,
            Some(RenetServerConfig {
                max_new_connections_per_second: 1,
                ..Default::default()
            }),
        )
        .unwrap();
        let server_addr = server_transport.addr();

        let duration = Duration::from_millis(10);

        macro_rules! attempt_connect {
            ($id:expr) => {{
                let mut connected = false;
                let config = ConnectionConfig::default();
                let mut client = RenetClient::new(config);
                let auth = renet_netcode::ClientAuthentication::Unsecure {
                    protocol_id: 0,
                    client_id: $id,
                    server_addr,
                    user_data: None,
                };
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let mut transport =
                    NetcodeClientTransport::new(Duration::ZERO, auth, socket).unwrap();

                for _ in 0..20 {
                    let _ = transport.update(duration, &mut client);
                    client.update(duration);
                    let _ = transport.send_packets(&mut client);
                    let events = server_transport.poll_events().await.unwrap();
                    for event in events {
                        if let NetworkEvent::ClientConnected(cid) = event
                            && cid.0 == $id
                        {
                            connected = true;
                        }
                    }
                    if connected {
                        break;
                    }
                    tokio::time::sleep(duration).await;
                }
                connected
            }};
        }

        // First connection should succeed
        let connected1 = attempt_connect!(1);
        assert!(connected1, "First connection should succeed");

        // Second connection within same second should be rejected/disconnected
        let connected2 = attempt_connect!(2);
        assert!(!connected2, "Second connection should be rate-limited");

        // After 1.1 seconds, third connection should succeed
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let connected3 = attempt_connect!(3);
        assert!(connected3, "Third connection should succeed after timeout");
    }
}

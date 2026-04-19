//! Aggregates multiple `GameTransport` implementations into one.

use aetheris_protocol::events::NetworkEvent;
use aetheris_protocol::traits::{ClientId, GameTransport, TransportError};
use async_trait::async_trait;

/// Combines multiple `GameTransport` implementations.
///
/// This allows the server to simultaneously support native clients (via Renet)
/// and web clients (via WebTransport).
pub struct MultiTransport {
    transports: Vec<Box<dyn GameTransport>>,
}

impl MultiTransport {
    /// Creates a new empty `MultiTransport`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transports: Vec::new(),
        }
    }

    /// Adds a transport to the aggregator.
    pub fn add_transport(&mut self, transport: Box<dyn GameTransport>) {
        self.transports.push(transport);
    }
}

impl Default for MultiTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl GameTransport for MultiTransport {
    async fn send_unreliable(
        &self,
        client_id: ClientId,
        data: &[u8],
    ) -> Result<(), TransportError> {
        for transport in &self.transports {
            // We try to send to all, but only one will likely succeed if IDs are properly partitioned
            // or if the transport returns ClientNotConnected for unknown IDs.
            match transport.send_unreliable(client_id, data).await {
                Ok(()) => return Ok(()),
                Err(TransportError::ClientNotConnected(_)) => {}
                Err(e) => {
                    tracing::error!("MultiTransport: individual transport send error: {:?}", e);
                    return Err(e);
                }
            }
        }
        Err(TransportError::ClientNotConnected(client_id))
    }

    async fn send_reliable(&self, client_id: ClientId, data: &[u8]) -> Result<(), TransportError> {
        for transport in &self.transports {
            match transport.send_reliable(client_id, data).await {
                Ok(()) => return Ok(()),
                Err(TransportError::ClientNotConnected(_)) => {}
                Err(e) => {
                    tracing::error!("MultiTransport: individual transport send error: {:?}", e);
                    return Err(e);
                }
            }
        }
        Err(TransportError::ClientNotConnected(client_id))
    }

    async fn broadcast_unreliable(&self, data: &[u8]) -> Result<(), TransportError> {
        let mut first_error = None;
        for transport in &self.transports {
            if let Err(e) = transport.broadcast_unreliable(data).await
                && first_error.is_none()
            {
                first_error = Some(e);
            }
        }

        if let Some(e) = first_error {
            Err(e)
        } else {
            Ok(())
        }
    }

    async fn poll_events(&mut self) -> Result<Vec<NetworkEvent>, TransportError> {
        let mut all_events = Vec::new();
        for transport in &mut self.transports {
            all_events.extend(transport.poll_events().await?);
        }
        Ok(all_events)
    }

    async fn connected_client_count(&self) -> usize {
        let mut total = 0;
        for transport in &self.transports {
            total += transport.connected_client_count().await;
        }
        total
    }
}

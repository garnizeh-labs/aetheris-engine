use crate::auth::AuthServiceImpl;
use aetheris_protocol::auth::v1::auth_service_server::AuthService;
use aetheris_protocol::matchmaking::v1::{
    CancelQueueRequest, CancelQueueResponse, HeartbeatRequest, HeartbeatResponse,
    ListServersRequest, ListServersResponse, MatchFoundStatus, QueueRequest, QueueUpdate,
    QueuedStatus, RegisterInstanceRequest, RegisterInstanceResponse, ServerInstance,
    matchmaking_service_server::MatchmakingService, queue_update::Status as UpdateStatus,
};
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

#[derive(Debug, Clone)]
pub struct RegisteredServer {
    pub info: ServerInstance,
    pub last_heartbeat: std::time::Instant,
}

#[derive(Clone)]
pub struct MatchmakingServiceImpl {
    servers: Arc<DashMap<String, RegisteredServer>>,
    authorizer: Arc<AuthServiceImpl>,
}

// Default removed as we need an authorizer now.

impl MatchmakingServiceImpl {
    #[must_use]
    pub fn new(authorizer: Arc<AuthServiceImpl>) -> Self {
        Self {
            servers: Arc::new(DashMap::new()),
            authorizer,
        }
    }
}

#[async_trait]
impl MatchmakingService for MatchmakingServiceImpl {
    type JoinQueueStream = ReceiverStream<Result<QueueUpdate, Status>>;

    async fn join_queue(
        &self,
        request: Request<QueueRequest>,
    ) -> Result<Response<Self::JoinQueueStream>, Status> {
        let req = request.into_inner();

        if !self.authorizer.is_authorized(&req.session_token) {
            return Err(Status::unauthenticated("Invalid session"));
        }

        let session_token = req.session_token.clone();
        let (tx, rx) = mpsc::channel(10);

        // Immediate Queued status
        let tx_queued = tx.clone();
        tokio::spawn(async move {
            let _ = tx_queued
                .send(Ok(QueueUpdate {
                    status: Some(UpdateStatus::Queued(QueuedStatus {
                        position: 1,
                        estimated_wait_seconds: 1,
                    })),
                }))
                .await;
        });

        let servers = self.servers.clone();
        let auth_clone = self.authorizer.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            let optimal = servers
                .iter()
                .filter(|s| s.last_heartbeat.elapsed().as_secs() < 30)
                .filter(|s| s.info.players + s.info.reserved < s.info.max_players)
                .max_by_key(|s| s.info.max_players - (s.info.players + s.info.reserved))
                .map(|s| s.info.clone());

            if let Some(server) = optimal {
                use aetheris_protocol::auth::v1::ConnectTokenRequest;
                let connect_req = Request::new(ConnectTokenRequest {
                    session_token,
                    server_address: server.addr.clone(),
                });

                let auth = auth_clone.clone();
                match auth.issue_connect_token(connect_req).await {
                    Ok(resp) => {
                        let inner_resp = resp.into_inner();
                        let _ = tx
                            .send(Ok(QueueUpdate {
                                status: Some(UpdateStatus::Matched(MatchFoundStatus {
                                    quic_token: inner_resp.token,
                                    server_address: server.addr,
                                    world_instance_id: server.instance_id,
                                })),
                            }))
                            .await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(Status::internal(format!(
                                "Failed to issue connect token: {e}"
                            ))))
                            .await;
                    }
                }
            } else {
                let _ = tx
                    .send(Err(Status::resource_exhausted("No servers available")))
                    .await;
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn cancel_queue(
        &self,
        _request: Request<CancelQueueRequest>,
    ) -> Result<Response<CancelQueueResponse>, Status> {
        Ok(Response::new(CancelQueueResponse { success: true }))
    }

    async fn list_servers(
        &self,
        _request: Request<ListServersRequest>,
    ) -> Result<Response<ListServersResponse>, Status> {
        let instances = self.servers.iter().map(|s| s.info.clone()).collect();
        Ok(Response::new(ListServersResponse { instances }))
    }

    async fn register_instance(
        &self,
        request: Request<RegisterInstanceRequest>,
    ) -> Result<Response<RegisterInstanceResponse>, Status> {
        let req = request.into_inner();
        let Some(instance) = req.instance else {
            return Err(Status::invalid_argument("Missing instance info"));
        };

        self.servers.insert(
            instance.instance_id.clone(),
            RegisteredServer {
                info: instance,
                last_heartbeat: std::time::Instant::now(),
            },
        );

        Ok(Response::new(RegisterInstanceResponse { success: true }))
    }

    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        let req = request.into_inner();
        if let Some(mut server) = self.servers.get_mut(&req.instance_id) {
            server.info.players = req.players;
            server.last_heartbeat = std::time::Instant::now();
            Ok(Response::new(HeartbeatResponse { ok: true }))
        } else {
            Err(Status::not_found("Instance not registered"))
        }
    }
}

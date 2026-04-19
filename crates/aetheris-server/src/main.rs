use aetheris_protocol::auth::v1::auth_service_server::AuthServiceServer;
use aetheris_protocol::matchmaking::v1::matchmaking_service_server::MatchmakingServiceServer;
use aetheris_protocol::telemetry::v1::telemetry_service_server::TelemetryServiceServer;
use aetheris_server::{
    auth::AuthServiceImpl,
    auth::email::{EmailSender, LettreSmtpEmailSender, LogEmailSender, ResendEmailSender},
    config::ServerConfig,
    matchmaking::MatchmakingServiceImpl,
    telemetry::{AetherisTelemetryService, json_telemetry_handler},
};
use axum::Router;
use axum::routing::post;
use std::sync::Arc;
use tokio::sync::broadcast;
use tonic::codegen::http::{Method, header};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tower_http::cors::{Any, CorsLayer};

#[cfg(feature = "phase1")]
use aetheris_ecs_bevy::BevyWorldAdapter;
#[cfg(feature = "phase1")]
use aetheris_encoder_serde::SerdeEncoder;
#[cfg(feature = "phase1")]
use aetheris_server::MultiTransport;
#[cfg(feature = "phase1")]
use aetheris_transport_webtransport::WebTransportBridge;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls provider");

    // M10105 — layered tracing subscriber: fmt + OTLP (non-fatal on failure)
    let _provider = {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        let fmt_layer = tracing_subscriber::fmt::layer().with_target(true);

        let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4317".to_string());

        let build_exporter = || -> Result<_, Box<dyn std::error::Error + Send + Sync>> {
            use opentelemetry_otlp::WithExportConfig;
            Ok(opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&otlp_endpoint)
                .build()?)
        };

        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

        match build_exporter() {
            Ok(exporter) => {
                use opentelemetry_sdk::trace::SdkTracerProvider;
                let provider = SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(
                        opentelemetry_sdk::Resource::builder()
                            .with_attributes(vec![opentelemetry::KeyValue::new(
                                "service.name",
                                "aetheris-server",
                            )])
                            .build(),
                    )
                    .build();
                let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "aetheris");
                let otlp_layer = tracing_opentelemetry::layer().with_tracer(tracer);

                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt_layer)
                    .with(otlp_layer)
                    .init();

                tracing::info!(otlp_endpoint = %otlp_endpoint, "OTLP tracing initialised");
                Some(provider)
            }
            Err(e) => {
                // OTLP unavailable — continue with fmt-only logging (non-fatal)
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt_layer)
                    .init();

                tracing::warn!(error = %e, otlp_endpoint = %otlp_endpoint, "OTLP init failed, tracing continues without Jaeger");
                None
            }
        }
    };

    let config = ServerConfig::load();
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(([0, 0, 0, 0], config.metrics_port))
        .set_buckets(&[
            0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 250.0, 500.0, 1000.0,
        ])
        .expect("Failed to set buckets")
        .install()
        .expect("Failed to install Prometheus metrics exporter");
    tracing::info!(
        "Prometheus metrics exporter listening on :{}/metrics",
        config.metrics_port
    );

    let grpc_addr =
        std::env::var("AETHERIS_GRPC_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".to_string());
    let addr = grpc_addr.parse()?;

    tracing::info!("Aetheris Control Plane listening on {}", addr);

    let sender_type = std::env::var("OTP_EMAIL_SENDER").unwrap_or_else(|_| "log".to_string());
    let env = std::env::var("AETHERIS_ENV").unwrap_or_else(|_| "dev".to_string());
    let email_sender: Arc<dyn EmailSender> = match sender_type.as_str() {
        "log" => {
            if env == "production" {
                return Err("OTP_EMAIL_SENDER=log is forbidden in AETHERIS_ENV=production".into());
            }
            Arc::new(LogEmailSender)
        }
        "smtp" => Arc::new(
            LettreSmtpEmailSender::from_env()
                .map_err(|e| format!("Failed to initialize SMTP sender: {e}"))?,
        ),
        "resend" => Arc::new(
            ResendEmailSender::from_env()
                .map_err(|e| format!("Failed to initialize Resend sender: {e}"))?,
        ),
        other => {
            return Err(format!("Unknown OTP_EMAIL_SENDER: {other}").into());
        }
    };

    let auth_service = AuthServiceImpl::new(email_sender).await;
    let matchmaking_service = MatchmakingServiceImpl::new(Arc::new(auth_service.clone()));
    let telemetry_service = AetherisTelemetryService::new();

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // M10105 — JSON telemetry HTTP server (for WASM fetch() path)
    // Runs on AETHERIS_TELEMETRY_HTTP_PORT (default 50055), separate from gRPC.
    // CORS allows all origins in dev; restricted in production (see CONTRIBUTING.md).
    {
        let telemetry_svc_clone = telemetry_service.clone();
        let mut shutdown_http = shutdown_tx.subscribe();
        let http_port: u16 = std::env::var("AETHERIS_TELEMETRY_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50055);
        let http_addr = std::net::SocketAddr::from(([0, 0, 0, 0], http_port));

        let cors_http = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::POST, Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE]);

        let app = Router::new()
            .route("/telemetry/json", post(json_telemetry_handler))
            .layer(cors_http)
            .with_state(telemetry_svc_clone)
            .into_make_service_with_connect_info::<std::net::SocketAddr>();

        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(http_addr)
                .await
                .expect("Failed to bind telemetry HTTP server");
            tracing::info!("Telemetry JSON endpoint on :{}/telemetry/json", http_port);
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_http.recv().await;
                })
                .await
                .ok();
        });
    }

    let use_tls = std::env::var("AETHERIS_GRPC_TLS")
        .map(|v| {
            let v = v.to_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false);

    // Security: Fail fast if misconfigured for production
    if env == "production" {
        if !use_tls {
            return Err("AETHERIS_GRPC_TLS=true is REQUIRED in AETHERIS_ENV=production".into());
        }
        tracing::info!("Production mode verified: Secure transport enabled.");
    }

    #[cfg(feature = "phase1")]
    {
        use aetheris_server::TickScheduler;
        let mut transport = MultiTransport::new();
        let wt_addr_str =
            std::env::var("AETHERIS_WT_ADDR").unwrap_or_else(|_| "[::]:4433".to_string());
        let wt_addr = wt_addr_str.parse()?;
        let wt = WebTransportBridge::new(wt_addr).await;
        transport.add_transport(Box::new(wt));

        let mut world = BevyWorldAdapter::new(bevy_ecs::world::World::new());
        world.register_replicator(std::sync::Arc::new(aetheris_ecs_bevy::DefaultReplicator::<
            aetheris_ecs_bevy::Transform,
        >::new(
            aetheris_protocol::types::ComponentKind(1),
        )));
        let encoder = SerdeEncoder::new();

        let mut scheduler = TickScheduler::new(60, auth_service.clone());
        let shutdown_clone = shutdown_tx.subscribe();

        let scheduler_handle = tokio::spawn(async move {
            scheduler
                .run(
                    Box::new(transport),
                    Box::new(world),
                    Box::new(encoder),
                    shutdown_clone,
                )
                .await;
        });

        let shutdown_auth_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received, triggering cancellation...");
            let _ = shutdown_auth_tx.send(());
        });

        tracing::info!(
            "Aetheris Game Server (WebTransport) listening on {}",
            wt_addr
        );

        let cors = if env == "production" {
            // Restrictive CORS for production
            CorsLayer::new().allow_origin(
                std::env::var("ALLOWED_ORIGINS")
                    .unwrap_or_else(|_| "https://aetheris.io".to_string())
                    .parse::<header::HeaderValue>()
                    .map_err(|e| format!("Invalid ALLOWED_ORIGINS: {e}"))?,
            )
        } else {
            CorsLayer::new().allow_origin(Any).allow_credentials(false)
        };

        let cors = cors
            .allow_methods([Method::POST, Method::OPTIONS])
            .allow_headers([
                header::CONTENT_TYPE,
                header::USER_AGENT,
                header::HeaderName::from_static("x-grpc-web"),
                header::HeaderName::from_static("x-user-agent"),
            ])
            .expose_headers([
                header::HeaderName::from_static("grpc-status"),
                header::HeaderName::from_static("grpc-message"),
                header::HeaderName::from_static("grpc-status-details-bin"),
            ]);

        let mut builder = Server::builder()
            .accept_http1(true)
            .layer(cors)
            .layer(tonic_web::GrpcWebLayer::new());

        if use_tls {
            let (cert_path, key_path) = get_tls_paths();
            let tls_config = build_tls_config(&cert_path, &key_path).await?;
            builder = builder.tls_config(tls_config)?;
            tracing::info!("Aetheris Control Plane (TLS Enabled) listening on {}", addr);
        } else {
            tracing::info!("Aetheris Control Plane (Insecure) listening on {}", addr);
        }

        let router = register_services(
            builder,
            auth_service,
            matchmaking_service,
            telemetry_service,
        );

        router
            .serve_with_shutdown(addr, async move {
                let _ = shutdown_tx.subscribe().recv().await;
            })
            .await?;

        tracing::info!("gRPC server drained, waiting for scheduler...");
        let _ = scheduler_handle.await;
    }

    #[cfg(not(feature = "phase1"))]
    {
        let shutdown_fallback_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received, triggering cancellation...");
            let _ = shutdown_fallback_tx.send(());
        });

        let cors = if env == "production" {
            CorsLayer::new().allow_origin(
                std::env::var("ALLOWED_ORIGINS")
                    .unwrap_or_else(|_| "https://aetheris.io".to_string())
                    .parse::<header::HeaderValue>()
                    .map_err(|e| format!("Invalid ALLOWED_ORIGINS: {e}"))?,
            )
        } else {
            CorsLayer::new().allow_origin(Any)
        };

        let cors = cors
            .allow_methods([Method::POST, Method::OPTIONS])
            .allow_headers([
                header::CONTENT_TYPE,
                header::USER_AGENT,
                header::HeaderName::from_static("x-grpc-web"),
                header::HeaderName::from_static("x-user-agent"),
            ])
            .expose_headers([
                header::HeaderName::from_static("grpc-status"),
                header::HeaderName::from_static("grpc-message"),
                header::HeaderName::from_static("grpc-status-details-bin"),
            ]);

        let mut builder = Server::builder()
            .accept_http1(true)
            .layer(cors)
            .layer(tonic_web::GrpcWebLayer::new());

        if use_tls {
            let (cert_path, key_path) = get_tls_paths();
            let tls_config = build_tls_config(&cert_path, &key_path).await?;
            builder = builder.tls_config(tls_config)?;
            tracing::info!("Aetheris Control Plane (TLS Enabled) listening on {}", addr);
        } else {
            tracing::info!("Aetheris Control Plane (Insecure) listening on {}", addr);
        }

        let router = register_services(
            builder,
            auth_service,
            matchmaking_service,
            telemetry_service,
        );

        router
            .serve_with_shutdown(addr, async move {
                let _ = shutdown_tx.subscribe().recv().await;
            })
            .await?;
    }

    Ok(())
}

fn get_tls_paths() -> (String, String) {
    let cert_path = std::env::var("AETHERIS_GRPC_TLS_CERT_PATH")
        .unwrap_or_else(|_| "target/dev-certs/cert.pem".to_string());
    let key_path = std::env::var("AETHERIS_GRPC_TLS_KEY_PATH")
        .unwrap_or_else(|_| "target/dev-certs/key.pem".to_string());
    (cert_path, key_path)
}

async fn build_tls_config(cert_path: &str, key_path: &str) -> Result<ServerTlsConfig, String> {
    let cert = tokio::fs::read(cert_path)
        .await
        .map_err(|e| format!("Failed to read TLS cert from {cert_path}: {e}"))?;
    let key = tokio::fs::read(key_path)
        .await
        .map_err(|e| format!("Failed to read TLS key from {key_path}: {e}"))?;
    let identity = Identity::from_pem(cert, key);
    Ok(ServerTlsConfig::new().identity(identity))
}

fn register_services<L: Clone>(
    mut builder: Server<L>,
    auth_service: AuthServiceImpl,
    matchmaking_service: MatchmakingServiceImpl,
    telemetry_service: AetherisTelemetryService,
) -> tonic::transport::server::Router<L> {
    builder
        .add_service(AuthServiceServer::new(auth_service))
        .add_service(MatchmakingServiceServer::new(matchmaking_service))
        .add_service(TelemetryServiceServer::new(telemetry_service))
}

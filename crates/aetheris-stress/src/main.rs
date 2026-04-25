use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::time::interval;
use tracing::{info, info_span, warn};
use tracing_subscriber::EnvFilter;

use aetheris_encoder_serde::SerdeEncoder;
use aetheris_protocol::auth::v1::{
    ClientMetadata, LoginRequest, OtpLoginRequest, auth_service_client::AuthServiceClient,
};
use aetheris_protocol::events::{GameEvent, NetworkEvent};
use aetheris_protocol::traits::Encoder;
use aetheris_protocol::types::{
    ClientId, INPUT_COMMAND_KIND, InputCommand, NetworkId, PlayerInputKind,
};
use renet::{ChannelConfig, ConnectionConfig, RenetClient, SendType};
use renet_netcode::{ClientAuthentication, NetcodeClientTransport};

#[derive(Debug, Default, Clone)]
struct BotStats {
    _id: usize,
    authenticated: bool,
    connected: bool,
    possessed: bool,
    time_to_auth: Option<Duration>,
    time_to_possession: Option<Duration>,
    inputs_sent: u64,
    rtt_samples: Vec<f64>,
    packet_loss_samples: Vec<f64>,
    error: Option<String>,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Number of concurrent clients to simulate
    #[arg(short, long, default_value_t = 50)]
    clients: usize,

    /// Duration to run the test in seconds (0 for indefinite)
    #[arg(short, long, default_value_t = 0)]
    duration: u64,

    /// gRPC Auth server host
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    auth_host: String,

    /// Game server UDP host
    #[arg(long, default_value = "127.0.0.1:5000")]
    game_host: String,

    /// Protocol ID for Renet
    #[arg(long, default_value_t = 0)]
    protocol_id: u64,
}

const CHANNEL_UNRELIABLE: u8 = 0;
const CHANNEL_RELIABLE: u8 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let args = Args::parse();
    info!(
        "Starting Aetheris Stress Test Bot with {} clients",
        args.clients
    );

    let mut handles = Vec::new();

    for i in 0..args.clients {
        let bot_id = i + 1;
        let auth_host = args.auth_host.clone();
        let game_host = args.game_host.clone();
        let protocol_id = args.protocol_id;
        let duration = args.duration;

        let handle = tokio::spawn(async move {
            run_bot(bot_id, auth_host, game_host, protocol_id, duration).await
        });
        handles.push(handle);
    }

    let mut all_stats = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(stats) => all_stats.push(stats),
            Err(e) => warn!("Task panicked: {:?}", e),
        }
    }

    print_summary(&all_stats, args.duration);

    Ok(())
}

fn print_summary(stats: &[BotStats], duration: u64) {
    let total = stats.len();
    let authenticated = stats.iter().filter(|s| s.authenticated).count();
    let connected = stats.iter().filter(|s| s.connected).count();
    let possessed = stats.iter().filter(|s| s.possessed).count();
    let total_inputs: u64 = stats.iter().map(|s| s.inputs_sent).sum();

    let avg_inputs = if total > 0 {
        total_inputs / total as u64
    } else {
        0
    };
    let inputs_per_sec = if duration > 0 {
        total_inputs as f64 / duration as f64
    } else {
        0.0
    };

    let auth_times: Vec<f64> = stats
        .iter()
        .filter_map(|s| s.time_to_auth.map(|d| d.as_secs_f64() * 1000.0))
        .collect();
    let possession_times: Vec<f64> = stats
        .iter()
        .filter_map(|s| s.time_to_possession.map(|d| d.as_secs_f64() * 1000.0))
        .collect();
    let all_rtt: Vec<f64> = stats.iter().flat_map(|s| s.rtt_samples.clone()).collect();
    let all_loss: Vec<f64> = stats
        .iter()
        .flat_map(|s| s.packet_loss_samples.clone())
        .collect();

    println!("\n{}", "=".repeat(60));
    println!("                AETHERIS STRESS TEST SUMMARY");
    println!("{}", "=".repeat(60));
    println!("Total Bots:           {}", total);
    println!("Duration:             {}s", duration);
    println!("------------------------------------------------------------");
    println!("Success Rates:");
    println!(
        "  Authentication:     {:3} / {:3} ({:5.1}%)",
        authenticated,
        total,
        (authenticated as f64 / total.max(1) as f64) * 100.0
    );
    println!(
        "  Game Connection:    {:3} / {:3} ({:5.1}%)",
        connected,
        total,
        (connected as f64 / total.max(1) as f64) * 100.0
    );
    println!(
        "  Entity Possession:  {:3} / {:3} ({:5.1}%)",
        possessed,
        total,
        (possessed as f64 / total.max(1) as f64) * 100.0
    );
    println!("------------------------------------------------------------");
    println!("Performance Timings (ms):");
    println!(
        "  Time to Auth:       Avg: {:7.2} | P99: {:7.2}",
        calculate_avg(&auth_times),
        calculate_p99(&auth_times)
    );
    println!(
        "  Time to Possess:    Avg: {:7.2} | P99: {:7.2}",
        calculate_avg(&possession_times),
        calculate_p99(&possession_times)
    );
    println!("------------------------------------------------------------");
    println!("Network Health:");
    println!(
        "  Round Trip (RTT):   Avg: {:7.2}ms| P99: {:7.2}ms",
        calculate_avg(&all_rtt),
        calculate_p99(&all_rtt)
    );
    println!(
        "  Packet Loss:        Avg: {:7.2}% | P99: {:7.2}%",
        calculate_avg(&all_loss) * 100.0,
        calculate_p99(&all_loss) * 100.0
    );
    println!("------------------------------------------------------------");
    println!("Throughput:");
    println!("  Total Inputs Sent:  {}", total_inputs);
    println!("  Avg Inputs / Bot:   {}", avg_inputs);
    println!("  Inputs / Second:    {:.2}", inputs_per_sec);
    println!("{}", "=".repeat(60));

    let errors: Vec<_> = stats.iter().filter_map(|s| s.error.as_ref()).collect();
    if !errors.is_empty() {
        println!("\nTop Errors:");
        let mut error_counts = std::collections::HashMap::new();
        for err in errors {
            *error_counts.entry(err).or_insert(0) += 1;
        }
        let mut sorted_errors: Vec<_> = error_counts.into_iter().collect();
        sorted_errors.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (err, count) in sorted_errors.iter().take(5) {
            println!("  [{}] {}", count, err);
        }
    }
    println!();
}

async fn run_bot(
    id: usize,
    auth_host: String,
    game_host: String,
    protocol_id: u64,
    duration_secs: u64,
) -> BotStats {
    let mut stats = BotStats {
        _id: id,
        ..Default::default()
    };

    match run_bot_inner(
        id,
        auth_host,
        game_host,
        protocol_id,
        duration_secs,
        &mut stats,
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            stats.error = Some(e.to_string());
        }
    }
    stats
}

fn calculate_p99(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let index = (sorted.len() as f64 * 0.99) as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn calculate_avg(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().sum::<f64>() / samples.len() as f64
}

async fn run_bot_inner(
    id: usize,
    auth_host: String,
    game_host: String,
    protocol_id: u64,
    duration_secs: u64,
    stats: &mut BotStats,
) -> Result<()> {
    let span = info_span!("bot", id);
    let _guard = span.enter();

    let start_time = Instant::now();
    let username = format!("bot_{:02}@aetheris.dev", id);

    info!("Authenticating as {}", username);

    // Step 1: Auth
    let mut auth_client = AuthServiceClient::connect(auth_host).await?;

    // Step 1.1: Request OTP
    let otp_resp = auth_client
        .request_otp(aetheris_protocol::auth::v1::OtpRequest {
            email: username.clone(),
        })
        .await?
        .into_inner();
    let request_id = otp_resp.request_id;
    info!("OTP requested, request_id: {}", request_id);

    // Step 1.2: Login
    let login_request = LoginRequest {
        method: Some(aetheris_protocol::auth::v1::login_request::Method::Otp(
            OtpLoginRequest {
                request_id,
                code: "000001".to_string(), // Canonical success code for bypass
            },
        )),
        metadata: Some(ClientMetadata {
            client_version: "stress-test".to_string(),
            platform: "linux-bot".to_string(),
        }),
    };

    let login_response = auth_client.login(login_request).await?.into_inner();
    let session_token = login_response.session_token;
    stats.authenticated = true;
    stats.time_to_auth = Some(start_time.elapsed());
    info!(
        "Authenticated successfully in {:?}",
        stats.time_to_auth.unwrap()
    );

    // Step 2: Transport Initialization
    let connection_config = ConnectionConfig {
        client_channels_config: vec![
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

    let mut client = RenetClient::new(connection_config);
    let server_addr: SocketAddr = game_host.parse()?;

    // In a real scenario we'd use Secure authentication with the token,
    // but the engine's RenetTransport currently uses Unsecure.
    // However, the instructions say "passes the token".
    // Since renet-netcode's Unsecure doesn't pass a token, and we need to pass it
    // for the server to authenticate us via WireEvent::Auth, we'll connect
    // and then send the Auth event.

    let client_id = id as u64; // Using id as client_id for simplicity in stress test
    let auth = ClientAuthentication::Unsecure {
        protocol_id,
        client_id,
        server_addr,
        user_data: None,
    };

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut transport = NetcodeClientTransport::new(Duration::ZERO, auth, socket)?;
    let encoder = SerdeEncoder::new();

    info!("Connecting to game server at {}", game_host);

    // Step 3: Tick Loop
    let mut tick_rate = interval(Duration::from_secs_f64(1.0 / 60.0));
    let mut last_log = Instant::now();
    let mut inputs_sent = 0;
    let mut current_tick = 0u64;
    let mut possessed_id: Option<NetworkId> = None;
    let mut auth_sent = false;
    let mut last_stats_sample = Instant::now();
    let test_start_time = Instant::now();

    let mut encoder_buffer = vec![0u8; encoder.max_encoded_size()];
    loop {
        if duration_secs > 0 && test_start_time.elapsed().as_secs() >= duration_secs {
            info!("Test duration reached, bot {} exiting", id);
            break;
        }
        tick_rate.tick().await;
        let now = Instant::now();
        let loop_duration = Duration::from_secs_f64(1.0 / 60.0);

        transport
            .update(loop_duration, &mut client)
            .context("Transport update failed")?;
        client.update(loop_duration);

        if client.is_connected() {
            stats.connected = true;
            if !auth_sent {
                info!("Transport connected, sending Auth + StartSession in the same tick");
                // Send Auth and StartSession in the same tick so the server can process
                // both events in a single poll pass, saving one full tick period (~16.6 ms).
                // The server's Stage 2 processes events sequentially: Auth inserts the
                // client into authenticated_clients, then StartSession (next event for the
                // same client) finds it there and spawns the ship immediately.
                // A-06 fix: was sending them in separate tick iterations (one each loop).
                let auth_event = NetworkEvent::Auth {
                    session_token: session_token.clone(),
                };
                let data = encoder.encode_event(&auth_event)?;
                client.send_message(CHANNEL_RELIABLE, data);

                let start_event = NetworkEvent::StartSession {
                    client_id: ClientId(0), // Server ignores this and uses connection ID
                };
                let data = encoder.encode_event(&start_event)?;
                client.send_message(CHANNEL_RELIABLE, data);

                auth_sent = true;
            }

            // Poll for events (like Possession)
            while let Some(data) = client.receive_message(CHANNEL_RELIABLE) {
                if let Ok(NetworkEvent::GameEvent {
                    event: GameEvent::Possession { network_id },
                    ..
                }) = encoder.decode_event(&data)
                {
                    let p_time = start_time.elapsed();
                    info!("Possessed entity {} in {:?}", network_id.0, p_time);
                    possessed_id = Some(network_id);
                    stats.possessed = true;
                    stats.time_to_possession = Some(p_time);
                }
            }

            // Step 4: Send movement if possessed
            if let Some(nid) = possessed_id {
                let move_x = rand::random_range(-1.0..1.0);
                let move_y = rand::random_range(-1.0..1.0);

                let input = InputCommand {
                    tick: current_tick,
                    actions: vec![PlayerInputKind::Move {
                        x: move_x,
                        y: move_y,
                    }],
                    actions_mask: 0,
                    last_seen_input_tick: None,
                };

                let payload = rmp_serde::to_vec(&input)?;

                // Wrap in ReplicationEvent for the encoder
                let replication_event = aetheris_protocol::events::ReplicationEvent {
                    network_id: nid,
                    component_kind: INPUT_COMMAND_KIND,
                    payload,
                    tick: current_tick,
                };

                let len = encoder.encode(&replication_event, &mut encoder_buffer)?;
                client.send_message(CHANNEL_UNRELIABLE, encoder_buffer[..len].to_vec());
                inputs_sent += 1;
                stats.inputs_sent += 1;
            }
        }

        transport
            .send_packets(&mut client)
            .context("Transport send failed")?;

        if now.duration_since(last_stats_sample) >= Duration::from_secs(1) {
            let info = client.network_info();
            stats.rtt_samples.push(info.rtt);
            stats.packet_loss_samples.push(info.packet_loss);
            last_stats_sample = now;
        }

        if now.duration_since(last_log) >= Duration::from_secs(10) {
            info!(
                "Sent {} inputs in last 10s. Connected: {}",
                inputs_sent,
                client.is_connected()
            );
            inputs_sent = 0;
            last_log = now;
        }

        current_tick += 1;
    }

    Ok(())
}

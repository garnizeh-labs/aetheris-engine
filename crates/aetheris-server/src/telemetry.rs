//! Telemetry gRPC service implementation + JSON HTTP handler.
//!
//! Receives structured log events from WASM clients over gRPC-web (TCP/HTTP)
//! and forwards them to the tracing pipeline (Loki via Promtail).
//!
//! Also exposes a plain JSON endpoint (`POST /telemetry/json`) consumed by the
//! WASM `metrics.rs` flush path (no gRPC framing needed on the client side).
//!
//! This is an **out-of-band** diagnostic channel, intentionally independent of
//! WebTransport, so it remains reachable during QUIC failures.
//!
//! # M10105 — Prometheus Metrics (static labels only, anti-cardinality rule)
//!
//! All metrics use `client_type = "wasm_playground"` as the only label.
//! No UUIDs or client IDs appear in Prometheus label sets to prevent
//! time-series cardinality explosion during continuous Playground testing.

use aetheris_protocol::telemetry::v1::{
    TelemetryBatch, TelemetryLevel, TelemetryResponse, telemetry_service_server::TelemetryService,
};
use axum::{Json, extract::ConnectInfo, http::StatusCode, response::IntoResponse};
use dashmap::DashMap;
use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Requests allowed per IP within the rate-limit window.
const RATE_LIMIT_MAX: u32 = 60;
/// Duration of the rate-limit sliding window in seconds.
const RATE_LIMIT_WINDOW_SECS: u64 = 60;
/// Maximum length of `message` and `target` fields after server-side sanitization.
const MAX_FIELD_LEN: usize = 512;
/// Maximum number of events allowed in a single batch.
const MAX_BATCH_SIZE: usize = 256;
/// Threshold of rate limit entries before an opportunistic prune is triggered.
const PRUNE_THRESHOLD: usize = 1000;

/// Returns the current Unix timestamp in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Strips ASCII control characters and truncates to `max_len` bytes at a char boundary.
/// Ensures the invariant that the output byte length is <= `max_len`.
fn sanitize(s: &str, max_len: usize) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.len() <= max_len {
        clean
    } else {
        clean
            .char_indices()
            .take_while(|(i, c)| i + c.len_utf8() <= max_len)
            .last()
            .map_or_else(String::new, |(i, c)| clean[..i + c.len_utf8()].to_string())
    }
}

/// Extract the client's real IP address from metadata (Forwarded or X-Forwarded-For)
/// falling back to the remote address provided by tonic.
fn extract_client_ip<T>(request: &Request<T>) -> Option<IpAddr> {
    let metadata = request.metadata();

    // 1. Check Forwarded header (RFC 7239)
    if let Some(forwarded) = metadata.get("forwarded").and_then(|v| v.to_str().ok()) {
        for part in forwarded.split(';') {
            let part = part.trim();
            if let Some(for_val) = part.strip_prefix("for=") {
                let ip_str = for_val.trim_matches('"').split(':').next()?;
                if let Ok(ip) = ip_str.parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }

    // 2. Check X-Forwarded-For (Common legacy proxy header)
    if let Some(xff) = metadata
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        // First entry is the original client
        if let Some(first) = xff.split(',').next()
            && let Ok(ip) = first.trim().parse::<IpAddr>()
        {
            return Some(ip);
        }
    }

    // 3. Fallback to physical peer address
    request.remote_addr().map(|addr| addr.ip())
}

#[derive(Clone)]
pub struct AetherisTelemetryService {
    /// Per-IP sliding-window rate limiter: maps IP → (`request_count`, `window_start_secs`).
    rate_limits: Arc<DashMap<IpAddr, (u32, u64)>>,
}

impl AetherisTelemetryService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rate_limits: Arc::new(DashMap::new()),
        }
    }

    /// Performs an opportunistic sweep of the rate limit map to remove expired entries.
    fn prune_expired_entries(&self) {
        let now = now_secs();
        // Entry is considered stale if the window started more than 2x window duration ago.
        self.rate_limits.retain(|_, (_, window_start)| {
            now.saturating_sub(*window_start) < 2 * RATE_LIMIT_WINDOW_SECS
        });
    }

    /// Returns `true` if the request from `ip` is within the allowed rate.
    fn check_rate_limit(&self, ip: IpAddr) -> bool {
        // Trigger opportunistic pruning if the map is getting large
        if self.rate_limits.len() > PRUNE_THRESHOLD {
            self.prune_expired_entries();
        }

        let now = now_secs();
        let mut entry = self.rate_limits.entry(ip).or_insert((0, now));
        let (count, window_start) = entry.value_mut();

        if now.saturating_sub(*window_start) >= RATE_LIMIT_WINDOW_SECS {
            *window_start = now;
            *count = 1;
            true
        } else if *count < RATE_LIMIT_MAX {
            *count += 1;
            true
        } else {
            false
        }
    }
}

impl Default for AetherisTelemetryService {
    fn default() -> Self {
        Self::new()
    }
}

#[tonic::async_trait]
impl TelemetryService for AetherisTelemetryService {
    async fn submit_telemetry(
        &self,
        request: Request<TelemetryBatch>,
    ) -> Result<Response<TelemetryResponse>, Status> {
        // Resolve the real client IP, denying request if it cannot be determined (security hardening)
        let client_ip = extract_client_ip(&request).ok_or_else(|| {
            Status::permission_denied("Identification protocol failed: client IP indeterminate")
        })?;

        if !self.check_rate_limit(client_ip) {
            return Err(Status::resource_exhausted(
                "Telemetry rate limit exceeded. Try again in 60 seconds.",
            ));
        }

        let batch = request.into_inner();

        // Enforce batch size limit before heavy processing
        if batch.events.len() > MAX_BATCH_SIZE {
            return Err(Status::invalid_argument(format!(
                "Batch size policy violation: limit is {MAX_BATCH_SIZE} events"
            )));
        }

        // Validate session_id length — user-supplied, treat as untrusted input.
        if batch.session_id.len() > 128 {
            return Err(Status::invalid_argument("session_id exceeds 128 bytes"));
        }

        let session_id = sanitize(&batch.session_id, 128);
        process_events_grpc(&batch.events, &session_id);
        Ok(Response::new(TelemetryResponse {}))
    }
}

// ---------------------------------------------------------------------------
// Shared event processing — used by both gRPC and JSON handlers
// ---------------------------------------------------------------------------

fn process_events_grpc(
    events: &[aetheris_protocol::telemetry::v1::TelemetryEvent],
    session_id: &str,
) {
    metrics::counter!(
        "aetheris_wasm_telemetry_batches_total",
        "client_type" => "wasm_playground"
    )
    .increment(1);

    for event in events {
        let target = sanitize(&event.target, MAX_FIELD_LEN);
        let message = sanitize(&event.message, MAX_FIELD_LEN);
        let trace_id = sanitize(&event.trace_id, 64);
        let span_name = sanitize(&event.span_name, 128);
        let ts = event.timestamp_ms;

        // Record Prometheus metrics parsed from the metrics_snapshot event
        if event.span_name == "metrics_snapshot" {
            record_wasm_metrics(&event.message);
        }

        let level =
            TelemetryLevel::try_from(event.level).unwrap_or(TelemetryLevel::LevelUnspecified);
        metrics::counter!(
            "aetheris_wasm_telemetry_events_total",
            "client_type" => "wasm_playground",
            "level" => level_str(level),
        )
        .increment(1);

        // Emit into tracing so events flow through Loki via Promtail.
        // trace_id field enables log-to-trace correlation in Grafana.
        match level {
            TelemetryLevel::Error => tracing::error!(
                session_id = %session_id,
                trace_id = %trace_id,
                span_name = %span_name,
                target = %target,
                timestamp_ms = ts,
                rtt_ms = ?event.rtt_ms,
                "wasm: {}", message
            ),
            TelemetryLevel::Warn => tracing::warn!(
                session_id = %session_id,
                trace_id = %trace_id,
                span_name = %span_name,
                target = %target,
                timestamp_ms = ts,
                rtt_ms = ?event.rtt_ms,
                "wasm: {}", message
            ),
            TelemetryLevel::Info | TelemetryLevel::LevelUnspecified => tracing::trace!(
                session_id = %session_id,
                trace_id = %trace_id,
                span_name = %span_name,
                target = %target,
                timestamp_ms = ts,
                rtt_ms = ?event.rtt_ms,
                "wasm: {}", message
            ),
        }
    }
}

fn level_str(level: TelemetryLevel) -> &'static str {
    match level {
        TelemetryLevel::Error => "error",
        TelemetryLevel::Warn => "warn",
        TelemetryLevel::Info | TelemetryLevel::LevelUnspecified => "info",
    }
}

/// Parse `metrics_snapshot` message fields and record as Prometheus histograms.
/// Message format: `fps=60.0 frame_p99=2.10ms sim_p99=0.50ms rtt=12.3ms entities=5 snapshots=3 dropped=0`
///
/// Uses static `client_type` label only — no UUIDs (anti-cardinality rule, M10105 §5.2).
fn record_wasm_metrics(msg: &str) {
    for part in msg.split_whitespace() {
        if let Some((key, val)) = part.split_once('=') {
            let num = val
                .trim_end_matches("ms")
                .parse::<f64>()
                .unwrap_or(f64::NAN);

            // M10105 — Validation: ignore non-finite, out-of-range, or negative values.
            if !num.is_finite() || !(0.0..=1_000_000.0).contains(&num) {
                continue;
            }

            match key {
                "fps" => {
                    metrics::histogram!(
                        "aetheris_wasm_fps",
                        "client_type" => "wasm_playground"
                    )
                    .record(num);
                }
                "frame_p99" => {
                    metrics::histogram!(
                        "aetheris_wasm_frame_time_ms",
                        "client_type" => "wasm_playground"
                    )
                    .record(num);
                }
                "sim_p99" => {
                    metrics::histogram!(
                        "aetheris_wasm_sim_time_ms",
                        "client_type" => "wasm_playground"
                    )
                    .record(num);
                }
                "rtt" => {
                    metrics::histogram!(
                        "aetheris_wasm_rtt_ms",
                        "client_type" => "wasm_playground"
                    )
                    .record(num);
                }
                "entities" => {
                    metrics::gauge!(
                        "aetheris_wasm_entity_count",
                        "client_type" => "wasm_playground"
                    )
                    .set(num);
                }
                "dropped" => {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let count = num as u64;
                    metrics::counter!(
                        "aetheris_wasm_telemetry_dropped_total",
                        "client_type" => "wasm_playground"
                    )
                    .increment(count);
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// JSON HTTP handler (POST /telemetry/json) — consumed by WASM metrics.rs
// ---------------------------------------------------------------------------

/// JSON wire format matching `TelemetryEventJson` in WASM `metrics.rs`.
#[derive(Debug, Deserialize)]
pub struct JsonTelemetryEvent {
    pub timestamp_ms: f64,
    pub level: u32,
    pub target: String,
    pub message: String,
    #[serde(default)]
    pub rtt_ms: Option<f64>,
    pub trace_id: String,
    pub span_name: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonTelemetryBatch {
    pub events: Vec<JsonTelemetryEvent>,
    pub session_id: String,
}

/// Axum handler for `POST /telemetry/json`.
///
/// Accepts the JSON batch emitted by the WASM `MetricsCollector::flush()`.
/// Rate-limited per IP using the same `DashMap` as the gRPC handler.
/// Returns 429 on rate-limit exceeded, 400 on validation failure.
#[allow(clippy::unused_async)]
pub async fn json_telemetry_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    axum::extract::State(svc): axum::extract::State<AetherisTelemetryService>,
    Json(batch): Json<JsonTelemetryBatch>,
) -> impl IntoResponse {
    let ip = addr.ip();

    if !svc.check_rate_limit(ip) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    if batch.events.len() > MAX_BATCH_SIZE {
        return (StatusCode::BAD_REQUEST, "batch too large").into_response();
    }
    if batch.session_id.len() > 128 {
        return (StatusCode::BAD_REQUEST, "session_id too long").into_response();
    }

    let session_id = sanitize(&batch.session_id, 128);

    // Emit a batch-level Jaeger span so all events from this flush are correlated.
    // The trace_id from the first event (if present) is recorded as an attribute;
    // full W3C trace context injection into Jaeger is deferred to M1062.
    let first_trace_id = batch
        .events
        .first()
        .map(|e| sanitize(&e.trace_id, 64))
        .unwrap_or_default();

    let _span = tracing::trace_span!(
        "wasm_telemetry",
        session_id = %session_id,
        trace_id = %first_trace_id,
        event_count = batch.events.len(),
    )
    .entered();

    metrics::counter!(
        "aetheris_wasm_telemetry_batches_total",
        "client_type" => "wasm_playground"
    )
    .increment(1);

    for event in &batch.events {
        let target = sanitize(&event.target, MAX_FIELD_LEN);
        let message = sanitize(&event.message, MAX_FIELD_LEN);
        let trace_id = sanitize(&event.trace_id, 64);
        let span_name = sanitize(&event.span_name, 128);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ts = event.timestamp_ms as u64;

        if event.span_name == "metrics_snapshot" {
            record_wasm_metrics(&event.message);
        }

        let level_label = match event.level {
            3 => "error",
            2 => "warn",
            _ => "info",
        };
        metrics::counter!(
            "aetheris_wasm_telemetry_events_total",
            "client_type" => "wasm_playground",
            "level" => level_label,
        )
        .increment(1);

        match event.level {
            3 => tracing::error!(
                session_id = %session_id,
                trace_id = %trace_id,
                span_name = %span_name,
                target = %target,
                timestamp_ms = ts,
                rtt_ms = ?event.rtt_ms,
                "wasm: {}", message
            ),
            2 => tracing::warn!(
                session_id = %session_id,
                trace_id = %trace_id,
                span_name = %span_name,
                target = %target,
                timestamp_ms = ts,
                rtt_ms = ?event.rtt_ms,
                "wasm: {}", message
            ),
            _ => tracing::trace!(
                session_id = %session_id,
                trace_id = %trace_id,
                span_name = %span_name,
                target = %target,
                timestamp_ms = ts,
                rtt_ms = ?event.rtt_ms,
                "wasm: {}", message
            ),
        }
    }

    (StatusCode::OK, "accepted").into_response()
}

#[cfg(test)]
mod tests {
    use super::{AetherisTelemetryService, sanitize};
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize("hello\x00\nworld", 512), "helloworld");
    }

    #[test]
    fn sanitize_truncates_at_boundary() {
        let long = "a".repeat(600);
        let result = sanitize(&long, 512);
        assert_eq!(result.len(), 512);
    }

    #[test]
    fn sanitize_handles_multibyte() {
        // '€' is 3 bytes; 171 × 3 = 513 bytes — must truncate to 170 chars (510 bytes).
        let s = "€".repeat(200);
        let result = sanitize(&s, 512);
        assert!(result.len() <= 512);
        let _ = result.chars().count(); // Must not panic (valid UTF-8).
    }

    #[test]
    fn rate_limit_allows_up_to_max() {
        let svc = AetherisTelemetryService::new();
        let ip = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
        for _ in 0..60 {
            assert!(svc.check_rate_limit(ip));
        }
        assert!(!svc.check_rate_limit(ip));
    }

    #[test]
    fn rate_limit_resets_after_window() {
        let svc = AetherisTelemetryService::new();
        let ip = IpAddr::V4(Ipv4Addr::new(5, 6, 7, 8));
        for _ in 0..60 {
            svc.check_rate_limit(ip);
        }
        // Ensure 61st call is rejected before window reset
        assert!(!svc.check_rate_limit(ip));
        // Backdate the window start to simulate expiry.
        svc.rate_limits.entry(ip).and_modify(|e| e.1 = 0);
        assert!(svc.check_rate_limit(ip));
    }

    #[test]
    fn json_deserialization_handles_missing_rtt() {
        let json = r#"{
            "session_id": "01JSZG2XKQP4V3R8N0CDWM7HFT",
            "events": [
                {
                    "timestamp_ms": 123456789.0,
                    "level": 1,
                    "target": "test",
                    "message": "hello",
                    "trace_id": "trace1",
                    "span_name": "span1"
                }
            ]
        }"#;
        let batch: super::JsonTelemetryBatch =
            serde_json::from_str(json).expect("Should deserialize even without rtt_ms");
        assert!(batch.events[0].rtt_ms.is_none());
    }
}

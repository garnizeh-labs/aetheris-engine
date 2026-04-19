#![allow(clippy::missing_errors_doc)]
#![allow(clippy::result_large_err)]
#![allow(clippy::cast_sign_loss)]

use aetheris_protocol::auth::v1::{
    ConnectTokenRequest, ConnectTokenResponse, GoogleLoginNonceRequest, GoogleLoginNonceResponse,
    LoginMethod, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, OtpRequest,
    OtpRequestAck, QuicConnectToken, RefreshRequest, RefreshResponse,
    auth_service_server::AuthService, login_request::Method,
};
use async_trait::async_trait;
use base64::Engine;
use blake2::{Blake2b, Digest, digest::consts::U32};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::RngExt;
use rusty_paseto::prelude::{
    CustomClaim, ExpirationClaim, IssuedAtClaim, Key, Local, PasetoBuilder, PasetoParser,
    PasetoSymmetricKey, SubjectClaim, TokenIdentifierClaim, V4,
};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tonic::{Request, Response, Status};
use ulid::Ulid;

pub mod email;
pub mod google;

use email::EmailSender;
use google::GoogleOidcClient;

pub struct OtpRecord {
    pub email: String,
    pub code_hash: Vec<u8>,
    pub google_nonce: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub attempts: u8,
}

#[derive(Clone)]
pub struct AuthServiceImpl {
    otp_store: Arc<DashMap<String, OtpRecord>>,
    /// Maps Session JTI -> Last Activity Unix Timestamp
    session_activity: Arc<DashMap<String, i64>>,
    /// Maps Player ID -> () (existence check for P1)
    player_registry: Arc<DashMap<String, ()>>,
    email_sender: Arc<dyn EmailSender>,
    google_client: Arc<Option<GoogleOidcClient>>,
    pub(crate) session_key: Arc<PasetoSymmetricKey<V4, Local>>,
    transport_key: Arc<PasetoSymmetricKey<V4, Local>>,
    bypass_enabled: bool,
}

impl std::fmt::Debug for AuthServiceImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthServiceImpl").finish_non_exhaustive()
    }
}

impl AuthServiceImpl {
    /// Creates a new `AuthServiceImpl` with the provided `email_sender`.
    ///
    /// # Panics
    ///
    /// - If `AETHERIS_ENV` is set to "production" but `AETHERIS_AUTH_BYPASS` is enabled.
    /// - If `AETHERIS_ENV` is set to "production" but `SESSION_PASETO_KEY` or `TRANSPORT_PASETO_KEY` are missing.
    /// - If the provided PASETO keys are not exactly 32 bytes long.
    pub async fn new(email_sender: Arc<dyn EmailSender>) -> Self {
        let env = std::env::var("AETHERIS_ENV").unwrap_or_else(|_| "dev".to_string());

        let session_key_str =
            std::env::var("SESSION_PASETO_KEY").map_err(|_| "SESSION_PASETO_KEY missing");
        let transport_key_str =
            std::env::var("TRANSPORT_PASETO_KEY").map_err(|_| "TRANSPORT_PASETO_KEY missing");

        let bypass_enabled = std::env::var("AETHERIS_AUTH_BYPASS").is_ok_and(|v| {
            let v = v.to_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        });

        if env == "production" {
            assert!(
                !bypass_enabled,
                "AETHERIS_AUTH_BYPASS=1 is forbidden in production"
            );
            assert!(
                !(session_key_str.is_err() || transport_key_str.is_err()),
                "PASETO keys must be explicitly set in production"
            );
        }

        let session_key_val = session_key_str.unwrap_or_else(|_| {
            assert!(env != "production", "Missing SESSION_PASETO_KEY");
            "01234567890123456789012345678901".to_string()
        });
        let transport_key_val = transport_key_str.unwrap_or_else(|_| {
            assert!(env != "production", "Missing TRANSPORT_PASETO_KEY");
            "01234567890123456789012345678901".to_string()
        });

        assert!(
            !(session_key_val.len() != 32 || transport_key_val.len() != 32),
            "PASETO keys must be exactly 32 bytes"
        );

        let session_key =
            PasetoSymmetricKey::<V4, Local>::from(Key::<32>::from(session_key_val.as_bytes()));
        let transport_key =
            PasetoSymmetricKey::<V4, Local>::from(Key::<32>::from(transport_key_val.as_bytes()));

        let google_client = GoogleOidcClient::new().await.ok();

        if bypass_enabled {
            tracing::warn!(
                "Authentication bypass is ENABLED (DEV ONLY) — 000001 code will work for smoke-test@aetheris.dev"
            );
        } else {
            tracing::info!("Authentication bypass is disabled");
        }

        Self {
            otp_store: Arc::new(DashMap::new()),
            session_activity: Arc::new(DashMap::new()),
            player_registry: Arc::new(DashMap::new()),
            email_sender,
            google_client: Arc::new(google_client),
            session_key: Arc::new(session_key),
            transport_key: Arc::new(transport_key),
            bypass_enabled,
        }
    }

    /// Normalizes email according to M1005 spec: trim whitespace and lowercase the entire address.
    fn normalize_email(email: &str) -> String {
        email.trim().to_lowercase()
    }

    fn derive_player_id(method: &str, identifier: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(format!("aetheris:{method}:{identifier}").as_bytes());
        let hash = hasher.finalize();
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&hash[0..16]);
        Ulid::from(u128::from_be_bytes(buf)).to_string()
    }

    #[must_use]
    pub fn is_authorized(&self, token: &str) -> bool {
        self.validate_and_get_jti(token, None).is_some()
    }

    /// Validates a session token and returns the JTI if authorized.
    #[must_use]
    pub fn validate_and_get_jti(&self, token: &str, tick: Option<u64>) -> Option<String> {
        let claims = PasetoParser::<V4, Local>::default()
            .parse(token, &self.session_key)
            .ok()?;

        let jti = claims.get("jti").and_then(|v| v.as_str())?;

        if self.is_session_authorized(jti, tick) {
            Some(jti.to_string())
        } else {
            None
        }
    }

    /// Validates a session by JTI, checking for revocation and enforcing 1h sliding idle window.
    ///
    /// # Performance
    /// if `tick` is provided, activity updates are coalesced to once per 60 ticks (~1s)
    /// to reduce write-lock contention on the session map.
    #[must_use]
    pub fn is_session_authorized(&self, jti: &str, tick: Option<u64>) -> bool {
        // Optimistic Read: Check for existence and idle timeout without write lock first.
        let (needs_update, now_ts) = if let Some(activity) = self.session_activity.get(jti) {
            let now = Utc::now().timestamp();
            // Idle timeout: 1 hour (3600 seconds)
            if now - *activity > 3600 {
                return false;
            }

            // Coalescing: Only update if tick is unavailable or it's a multiple of 60 (with jitter).
            // We use a simple summation of jti bytes to stagger updates across the 60-tick window
            // and avoid synchronized lock contention from 1000s of clients simultaneously.
            let jitter = jti
                .as_bytes()
                .iter()
                .fold(0u64, |acc, &x| acc.wrapping_add(u64::from(x)));
            let needs_update = tick.is_none_or(|t| (t.wrapping_add(jitter)) % 60 == 0);
            (needs_update, now)
        } else {
            // Not in activity map -> revoked or expired
            return false;
        };

        if needs_update && let Some(mut activity) = self.session_activity.get_mut(jti) {
            *activity = now_ts;
        }

        true
    }

    pub fn mint_session_token_for_test(&self, player_id: &str) -> Result<(String, u64), Status> {
        self.mint_session_token(player_id)
    }

    fn mint_session_token(&self, player_id: &str) -> Result<(String, u64), Status> {
        let jti = Ulid::new().to_string();
        let iat = Utc::now();
        let exp = iat + Duration::hours(24);

        let token = PasetoBuilder::<V4, Local>::default()
            .set_claim(SubjectClaim::from(player_id))
            .set_claim(TokenIdentifierClaim::from(jti.as_str()))
            .set_claim(IssuedAtClaim::try_from(iat.to_rfc3339().as_str()).unwrap())
            .set_claim(ExpirationClaim::try_from(exp.to_rfc3339().as_str()).unwrap())
            .build(&self.session_key)
            .map_err(|e| Status::internal(format!("{e:?}")))?;

        // Initialize session activity
        self.session_activity.insert(jti, iat.timestamp());

        Ok((token, exp.timestamp() as u64))
    }
}

#[async_trait]
impl AuthService for AuthServiceImpl {
    async fn request_otp(
        &self,
        request: Request<OtpRequest>,
    ) -> Result<Response<OtpRequestAck>, Status> {
        let req = request.into_inner();
        let email = Self::normalize_email(&req.email);

        // TODO: Implement per-email 5/h and per-IP 30/h rate limits (M1005 §3.4.2)
        // Link to spec: docs/roadmap/phase-1-playable-mvp/specs/M1005_control_plane_services.md

        let mut rng = rand::rng();
        let code = format!("{:06}", rng.random_range(0..1_000_000));
        let request_id = Ulid::new().to_string();
        let expires_at = Utc::now() + Duration::minutes(10);

        let mut hasher = Blake2b::<U32>::new();
        hasher.update(code.as_bytes());
        hasher.update(request_id.as_bytes());
        let code_hash = hasher.finalize().to_vec();

        self.otp_store.insert(
            request_id.clone(),
            OtpRecord {
                email: email.clone(),
                code_hash,
                google_nonce: None,
                expires_at,
                attempts: 0,
            },
        );

        let sender = self.email_sender.clone();
        let code_clone = code.clone();
        let env = std::env::var("AETHERIS_ENV").unwrap_or_else(|_| "dev".to_string());
        if env == "production" {
            tracing::info!(request_id = %request_id, "Generated OTP");
        } else {
            tracing::info!(request_id = %request_id, email = %email, code = %code, "Generated OTP (DEV ONLY)");
        }
        tokio::spawn(async move {
            let _ = sender
                .send(
                    &email,
                    "Your Aetheris OTP",
                    &format!("Code: {code_clone}"),
                    &format!("<h1>Code: {code_clone}</h1>"),
                )
                .await;
        });

        Ok(Response::new(OtpRequestAck {
            request_id,
            expires_at_unix_ms: expires_at.timestamp() as u64,
            retry_after_seconds: Some(0), // 0 on normal path per spec
        }))
    }

    #[allow(clippy::too_many_lines)]
    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let req = request.into_inner();
        let metadata = req.metadata.unwrap_or_default();
        let method = req
            .method
            .ok_or_else(|| Status::invalid_argument("Missing login method"))?;

        tracing::info!(
            version = metadata.client_version,
            platform = metadata.platform,
            "Processing login request"
        );

        match method {
            Method::Otp(otp_req) => {
                let (request_id, code) = (otp_req.request_id, otp_req.code);

                let (status, delay) = if let Some(_e) = self.otp_store.get_mut(&request_id) {
                    (None, false)
                } else {
                    (Some(Status::unauthenticated("Invalid credentials")), true)
                };

                if delay {
                    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                }

                if let Some(status) = status {
                    return Err(status);
                }

                let mut entry = self.otp_store.get_mut(&request_id).unwrap();

                if self.bypass_enabled && entry.email == "smoke-test@aetheris.dev" {
                    if code == "000000" {
                        entry.attempts += 1;
                        if entry.attempts >= 3 {
                            drop(entry);
                            self.otp_store.remove(&request_id);
                        }
                        return Err(Status::unauthenticated("Bypass: Forced failure for 000000"));
                    }

                    if Utc::now() > entry.expires_at {
                        drop(entry);
                        self.otp_store.remove(&request_id);
                        return Err(Status::deadline_exceeded("OTP expired"));
                    }

                    tracing::warn!(email = entry.email, "Bypass authentication successful");
                    let player_id = Self::derive_player_id("email", &entry.email);

                    // Check if new player
                    let is_new_player =
                        self.player_registry.insert(player_id.clone(), ()).is_none();

                    let (token, exp) = self.mint_session_token(&player_id)?;
                    drop(entry);
                    self.otp_store.remove(&request_id);

                    return Ok(Response::new(LoginResponse {
                        session_token: token,
                        expires_at_unix_ms: exp,
                        player_id,
                        is_new_player,
                        login_method: LoginMethod::EmailOtp as i32,
                    }));
                }

                if Utc::now() > entry.expires_at {
                    drop(entry);
                    self.otp_store.remove(&request_id);
                    return Err(Status::deadline_exceeded("OTP expired"));
                }

                let mut hasher = Blake2b::<U32>::new();
                hasher.update(code.as_bytes());
                hasher.update(request_id.as_bytes());
                let hash = hasher.finalize();

                if hash.as_slice().ct_eq(&entry.code_hash).into() {
                    let player_id = Self::derive_player_id("email", &entry.email);

                    // Check if new player
                    let is_new_player =
                        self.player_registry.insert(player_id.clone(), ()).is_none();

                    let (token, exp) = self.mint_session_token(&player_id)?;
                    drop(entry);
                    self.otp_store.remove(&request_id);

                    Ok(Response::new(LoginResponse {
                        session_token: token,
                        expires_at_unix_ms: exp,
                        player_id,
                        is_new_player,
                        login_method: LoginMethod::EmailOtp as i32,
                    }))
                } else {
                    entry.attempts += 1;
                    if entry.attempts >= 3 {
                        drop(entry);
                        self.otp_store.remove(&request_id);
                    }
                    Err(Status::unauthenticated("Invalid code"))
                }
            }
            Method::Google(google_req) => {
                let google_client = self
                    .google_client
                    .as_ref()
                    .as_ref()
                    .ok_or_else(|| Status::internal("Google OIDC not configured"))?;

                let nonce = if let Some(entry) = self.otp_store.get(&google_req.nonce_handle) {
                    entry.google_nonce.clone()
                } else {
                    None
                };

                let Some(nonce) = nonce else {
                    return Err(Status::unauthenticated("Invalid nonce_handle"));
                };

                let claims = google_client.verify_token(&google_req.google_id_token, &nonce)?;
                self.otp_store.remove(&google_req.nonce_handle);
                let email = claims
                    .email()
                    .map(|e| e.to_string())
                    .ok_or_else(|| Status::unauthenticated("Email missing from Google ID token"))?;
                let player_id = Self::derive_player_id("google", &email);

                // Check if new player
                let is_new_player = self.player_registry.insert(player_id.clone(), ()).is_none();

                let (token, exp) = self.mint_session_token(&player_id)?;

                Ok(Response::new(LoginResponse {
                    session_token: token,
                    expires_at_unix_ms: exp,
                    player_id,
                    is_new_player,
                    login_method: LoginMethod::GoogleOidc as i32,
                }))
            }
        }
    }

    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let mut jti_to_revoke = None;

        let token_from_metadata = request.metadata().get("authorization").and_then(|t| {
            t.to_str()
                .ok()
                .map(|s| s.trim_start_matches("Bearer ").to_string())
        });

        let token_str = token_from_metadata.or_else(|| {
            let body = request.get_ref();
            if body.session_token.is_empty() {
                None
            } else {
                Some(body.session_token.clone())
            }
        });

        if let Some(token_clean) = token_str
            && let Ok(claims) =
                PasetoParser::<V4, Local>::default().parse(&token_clean, &self.session_key)
            && let Some(jti) = claims.get("jti").and_then(|v| v.as_str())
        {
            jti_to_revoke = Some(jti.to_string());
        }

        if let Some(jti) = jti_to_revoke {
            self.session_activity.remove(&jti);
        }

        Ok(Response::new(LogoutResponse { revoked: true }))
    }

    async fn refresh_token(
        &self,
        request: Request<RefreshRequest>,
    ) -> Result<Response<RefreshResponse>, Status> {
        let req = request.into_inner();
        let token = req.session_token;

        let Ok(claims) = PasetoParser::<V4, Local>::default().parse(&token, &self.session_key)
        else {
            return Err(Status::unauthenticated("Invalid session token"));
        };

        let Some(jti) = claims.get("jti").and_then(|v| v.as_str()) else {
            return Err(Status::unauthenticated("Token missing jti"));
        };

        let Some(sub) = claims.get("sub").and_then(|v| v.as_str()) else {
            return Err(Status::unauthenticated("Token missing sub"));
        };

        if !self.is_session_authorized(jti, None) {
            return Err(Status::unauthenticated("Session revoked or expired"));
        }

        // Revoke old token
        self.session_activity.remove(jti);

        // Mint new token
        let (new_token, exp) = self.mint_session_token(sub)?;

        Ok(Response::new(RefreshResponse {
            session_token: new_token,
            expires_at_unix_ms: exp,
        }))
    }

    async fn issue_connect_token(
        &self,
        request: Request<ConnectTokenRequest>,
    ) -> Result<Response<ConnectTokenResponse>, Status> {
        let req = request.into_inner();

        let client_id = rand::random::<u64>();
        let mut rng = rand::rng();
        let mut nonce = [0u8; 24];
        rng.fill(&mut nonce);
        let server_nonce = base64::engine::general_purpose::STANDARD.encode(nonce);

        let iat = Utc::now();
        let exp = iat + Duration::minutes(5);

        let token = PasetoBuilder::<V4, Local>::default()
            .set_claim(CustomClaim::try_from(("client_id", serde_json::json!(client_id))).unwrap())
            .set_claim(
                CustomClaim::try_from(("server", serde_json::json!(req.server_address))).unwrap(),
            )
            .set_claim(
                CustomClaim::try_from(("server_nonce", serde_json::json!(server_nonce))).unwrap(),
            )
            .set_claim(IssuedAtClaim::try_from(iat.to_rfc3339().as_str()).unwrap())
            .set_claim(ExpirationClaim::try_from(exp.to_rfc3339().as_str()).unwrap())
            .build(&self.transport_key)
            .map_err(|e| Status::internal(format!("{e:?}")))?;

        Ok(Response::new(ConnectTokenResponse {
            token: Some(QuicConnectToken {
                paseto: token,
                server_address: req.server_address,
                expires_at_unix_ms: exp.timestamp().unsigned_abs() * 1000,
                client_id,
            }),
        }))
    }

    async fn create_google_login_nonce(
        &self,
        _request: Request<GoogleLoginNonceRequest>,
    ) -> Result<Response<GoogleLoginNonceResponse>, Status> {
        let mut nonce_bytes = [0u8; 16];
        rand::rng().fill(&mut nonce_bytes);
        let nonce = hex::encode(nonce_bytes);

        let nonce_handle = Ulid::new().to_string();
        let expires_at = Utc::now() + Duration::minutes(10);

        self.otp_store.insert(
            nonce_handle.clone(),
            OtpRecord {
                email: String::new(),
                code_hash: Vec::new(),
                google_nonce: Some(nonce.clone()),
                expires_at,
                attempts: 0,
            },
        );

        Ok(Response::new(GoogleLoginNonceResponse {
            nonce_handle,
            nonce,
            expires_at_unix_ms: expires_at.timestamp() as u64,
        }))
    }
}

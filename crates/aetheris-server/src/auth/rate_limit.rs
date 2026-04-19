use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use tonic::Status;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitType {
    Email,
    Ip,
}

#[derive(Debug)]
struct RateLimitEntry {
    count: u32,
    reset_at: Instant,
}

/// In-memory rate limiter for authentication attempts.
///
/// M10146 — Implements per-email (5/h) and per-IP (30/h) limits
/// to prevent OTP brute-force and resource exhaustion.
#[derive(Clone, Default)]
pub struct InMemoryRateLimiter {
    /// Maps (Type, Identity) -> Entry
    state: Arc<DashMap<(RateLimitType, String), RateLimitEntry>>,
}

impl InMemoryRateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(DashMap::new()),
        }
    }

    /// Checks if a request should be rate-limited.
    ///
    /// returns `Ok(())` if allowed, or `Err(Status)` if limited.
    pub fn check_limit(&self, limit_type: RateLimitType, identity: &str) -> Result<(), Status> {
        let key = (limit_type, identity.to_string());
        let now = Instant::now();

        // 1. Get or create entry
        let mut entry = self
            .state
            .entry(key.clone())
            .or_insert_with(|| RateLimitEntry {
                count: 0,
                reset_at: now + Duration::from_hours(1),
            });

        // 2. Check for reset
        if now > entry.reset_at {
            entry.count = 0;
            entry.reset_at = now + Duration::from_hours(1);
        }

        // 3. Enforce limit
        let limit = match limit_type {
            RateLimitType::Email => 5,
            RateLimitType::Ip => 30,
        };

        if entry.count >= limit {
            warn!(
                type = ?limit_type,
                identity = %identity,
                count = entry.count,
                "Rate limit exceeded"
            );
            return Err(Status::resource_exhausted(format!(
                "Rate limit exceeded for {limit_type:?}: {identity}. Try again later."
            )));
        }

        // 4. Increment
        entry.count += 1;
        info!(
            type = ?limit_type,
            identity = %identity,
            count = entry.count,
            "Rate limit check passed"
        );

        Ok(())
    }

    /// Periodic cleanup of expired entries (optional for MVP, but good for hygiene).
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.state.retain(|_, entry| entry.reset_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_rate_limit() {
        let limiter = InMemoryRateLimiter::new();
        let email = "test@example.com";

        // First 5 should succeed
        for _ in 0..5 {
            assert!(limiter.check_limit(RateLimitType::Email, email).is_ok());
        }

        // 6th should fail
        let result = limiter.check_limit(RateLimitType::Email, email);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::ResourceExhausted);
    }

    #[test]
    fn test_ip_rate_limit() {
        let limiter = InMemoryRateLimiter::new();
        let ip = "127.0.0.1";

        // First 30 should succeed
        for _ in 0..30 {
            assert!(limiter.check_limit(RateLimitType::Ip, ip).is_ok());
        }

        // 31st should fail
        let result = limiter.check_limit(RateLimitType::Ip, ip);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::ResourceExhausted);
    }

    #[tokio::test]
    async fn test_rate_limit_reset() {
        tokio::time::pause();
        let limiter = InMemoryRateLimiter::new();
        let email = "reset@example.com";

        // Exhaust the limit
        for _ in 0..5 {
            let _ = limiter.check_limit(RateLimitType::Email, email);
        }
        assert!(limiter.check_limit(RateLimitType::Email, email).is_err());

        // Advance time by 1 hour + 1 second
        tokio::time::advance(Duration::from_secs(3601)).await;

        // Should succeed now
        assert!(limiter.check_limit(RateLimitType::Email, email).is_ok());
    }

    #[tokio::test]
    async fn test_rate_limit_concurrency() {
        let limiter = Arc::new(InMemoryRateLimiter::new());
        let ip = "192.168.1.1";
        let mut handles = vec![];

        // Spawn 100 tasks hitting the same IP
        for _ in 0..100 {
            let l = Arc::clone(&limiter);
            let target = ip.to_string();
            handles.push(tokio::spawn(async move {
                l.check_limit(RateLimitType::Ip, &target)
            }));
        }

        let results = futures::future::join_all(handles).await;
        let success_count = results
            .into_iter()
            .filter(|r| r.as_ref().unwrap().is_ok())
            .count();

        // Exactly 30 should have succeeded
        assert_eq!(success_count, 30);
    }

    #[test]
    fn test_rate_limit_cleanup() {
        let limiter = InMemoryRateLimiter::new();
        let now = Instant::now();
        let email_stale = "stale@example.com";
        let email_fresh = "fresh@example.com";

        // Create a stale entry
        limiter.state.insert(
            (RateLimitType::Email, email_stale.to_string()),
            RateLimitEntry {
                count: 5,
                reset_at: now.checked_sub(Duration::from_hours(1)).unwrap(),
            },
        );

        // Create a fresh entry
        limiter.state.insert(
            (RateLimitType::Email, email_fresh.to_string()),
            RateLimitEntry {
                count: 1,
                reset_at: now + Duration::from_hours(1),
            },
        );

        assert_eq!(limiter.state.len(), 2);

        // Cleanup
        limiter.cleanup();

        // Only fresh should remain
        assert_eq!(limiter.state.len(), 1);
        assert!(
            limiter
                .state
                .contains_key(&(RateLimitType::Email, email_fresh.to_string()))
        );
    }
}

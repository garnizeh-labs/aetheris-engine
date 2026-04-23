//! Server configuration management.
//!
//! Loads settings from environment variables with sensible defaults.

/// Authoritative server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Port for Prometheus metrics scraping.
    pub metrics_port: u16,
    /// Authoritative tick rate in Hz.
    pub tick_rate: u64,
    /// Number of threads for parallel encoding.
    pub encode_threads: usize,
}

impl ServerConfig {
    /// Loads configuration from environment variables with safe defaults.
    #[must_use]
    pub fn load() -> Self {
        let metrics_port = std::env::var("AETHERIS_METRICS_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(9000);

        let tick_rate = std::env::var("AETHERIS_TICK_RATE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);

        let encode_threads = std::env::var("AETHERIS_ENCODE_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2);

        Self {
            metrics_port,
            tick_rate,
            encode_threads,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        // Clear env to ensure defaults
        unsafe {
            std::env::remove_var("AETHERIS_METRICS_PORT");
        }
        let config = ServerConfig::load();
        assert_eq!(config.metrics_port, 9000);
    }

    #[test]
    fn test_config_env_override() {
        unsafe {
            std::env::set_var("AETHERIS_METRICS_PORT", "9500");
        }
        let config = ServerConfig::load();
        assert_eq!(config.metrics_port, 9500);
        unsafe {
            std::env::remove_var("AETHERIS_METRICS_PORT");
        }
    }
}

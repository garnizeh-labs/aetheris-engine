//! Aetheris server library.
//!
//! Contains the core logic for the authoritative game server, including
//! the tick scheduler and configuration management.

#![warn(clippy::all, clippy::pedantic)]

#[cfg(not(target_arch = "wasm32"))]
/// Authentication and session management for the game server.
pub mod auth;
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod matchmaking;
pub mod multi_transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod telemetry;
#[cfg(not(target_arch = "wasm32"))]
pub mod tick;

pub use multi_transport::MultiTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use tick::TickScheduler;

//! Aetheris Quinn-based transport logic.
//!
//! **Phase:** P3 - Production Hardening
//! **Constraint:** Native QUIC/UDP with WebTransport protocol compatibility.
//! **Purpose:** Replaces P1 Renet with a production-grade QUIC implementation for
//! high-performance native and web clients.

#![warn(clippy::all, clippy::pedantic)]
#![cfg(not(target_arch = "wasm32"))]

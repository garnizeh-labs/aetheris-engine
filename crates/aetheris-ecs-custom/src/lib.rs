//! Aetheris custom ECS implementation.
//!
//! **Phase:** P3 - Production Hardening
//! **Constraint:** `SoA` (Structure of Arrays) layout with manual dirty-bit tracking.
//! **Purpose:** Replaces Bevy ECS with a specialized, hyper-optimized storage engine
//! to minimize `extract_deltas` latency at massive scale.

#![warn(clippy::all, clippy::pedantic)]

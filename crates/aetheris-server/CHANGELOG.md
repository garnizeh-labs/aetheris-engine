# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [0.8.0] - 2026-04-23

### 🚀 Features

- Sync with protocol v0.2.13 and implement replication batching

### 🐛 Bug Fixes

- *(engine)* Handle inbound replication batches and update tests for multi-thread runtime
- Resolve clippy warnings and stabilize integration tests with multi-thread runtime
- *(server)* Stabilize fragmentation pipeline and integration tests

### 🚜 Refactor

- Define AuthService trait and implement session verification logic in AuthServiceImpl
## [0.7.0] - 2026-04-22

### 🚀 Features

- Implement stress test bot and engine optimizations for high concurrency

### 🚜 Refactor

- Replace broadcast with targeted per-client unreliable sends and optimize stress test and transport configuration
- Implement `ReplicationBatch` protocol variant to mitigate packet explosion in Stage 5
- Implement graceful shutdown with `SIGTERM`/`SIGINT` handlers and explicit OTLP flushing
- Downgrade high-frequency simulation logs to `DEBUG` to reduce I/O pressure
- Parallelize Stage 5 (Encode) using a dedicated Rayon thread pool with `tokio::task::block_in_place`
## [0.6.1] - 2026-04-22

### 🚀 Features

- *(engine)* Integrate VS-05/VS-06 session flow and ADR-0003 compliance

### 🐛 Bug Fixes

- Address PR #45 review findings

### 📚 Documentation

- Update README to reflect VS-06 completion

### 🚀 Features

- *(server)* Implement `StartSession` flow: spawns session ship on-demand and grants Possession.
- *(server)* Implement `SystemManifest` (on-demand pull): extensible metadata with permission-aware filtering (JTI="admin").
- *(engine)* Upgrade to Protocol v0.2.11.

### 🐛 Bug Fixes

- *(server)* `TickScheduler::tick_step` now calls `world.post_extract()` immediately after Stage 4 extraction — part of the fix for silent replication failure caused by premature `clear_trackers()` invocation in `simulate()`.

## [0.6.0] - 2026-04-21

### 🚀 Features

- *(engine)* Implement asteroid depletion loop with respawn and metrics
- Integrate protocol v3 and enforce input validation
## [0.5.1] - 2026-04-21

### ⚙️ Miscellaneous Tasks

- Release v0.5.0
## [0.5.0] - 2026-04-20

### 📚 Documentation

- Synchronize workspace crate badges
- Standardize readme badges with protocol

### 🚀 Features

- Complete VS-01 engine logic and documentation
- Harden engine security and align specs with 6-stage pipeline
## [0.4.1] - 2026-04-20
## [0.4.0] - 2026-04-20

### 🚀 Features

- *(ecs)* Synchronize component registry and ship stats per M1020

### 📚 Documentation

- *(engine)* Update README for M1020 milestone
## [0.3.3] - 2026-04-19

### 🐛 Bug Fixes

- *(server)* Suppress clippy duration lint due to unstable constructors
## [0.3.2] - 2026-04-19

### 🚀 Features

- *(engine)* Consolidate multirepo (M10146) and upgrade to protocol 0.2.5
- *(engine)* Finalize M10146 multirepo consolidation and hardening

### 🐛 Bug Fixes

- *(server)* Replace unstable Duration::from_hours with stable total secs
## [0.3.1] - 2026-04-19

### 🚀 Features

- Harden transport and auth infrastructure

### 🐛 Bug Fixes

- Infrastructure hardening

### 📚 Documentation

- Initial commit of engine documentation (redacted)
- Enhance README with technical summary and links
- Align with aetheris premium templates and port infrastructure
- Fix cross-repo links and placeholders
- Add README.md files to all workspace crates
- Align engine READMEs and CI with protocol standards

### ⚙️ Miscellaneous Tasks

- *(engine)* Stabilize infrastructure, fix integration tests, and bump version to 0.2.0
- Release v0.2.0
- Release v0.3.0
## [0.3.0] - 2026-04-19

### 🚀 Features

- Harden transport and auth infrastructure

### 🐛 Bug Fixes

- Infrastructure hardening

### 📚 Documentation

- Initial commit of engine documentation (redacted)
- Enhance README with technical summary and links
- Align with aetheris premium templates and port infrastructure
- Fix cross-repo links and placeholders
- Add README.md files to all workspace crates
- Align engine READMEs and CI with protocol standards

### ⚙️ Miscellaneous Tasks

- *(engine)* Stabilize infrastructure, fix integration tests, and bump version to 0.2.0
- Release v0.2.0
## [0.2.0] - 2026-04-19

### 🚀 Features

- Harden transport and auth infrastructure

### 🐛 Bug Fixes

- Infrastructure hardening

### 📚 Documentation

- Initial commit of engine documentation (redacted)
- Enhance README with technical summary and links
- Align with aetheris premium templates and port infrastructure
- Fix cross-repo links and placeholders
- Add README.md files to all workspace crates
- Align engine READMEs and CI with protocol standards

### ⚙️ Miscellaneous Tasks

- *(engine)* Stabilize infrastructure, fix integration tests, and bump version to 0.2.0

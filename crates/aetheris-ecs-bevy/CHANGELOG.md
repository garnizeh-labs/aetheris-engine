# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [0.8.2] - 2026-04-24

### 🚀 Features

- Implement toroidal wrapping in Bevy adapter for infinite playground

### 🐛 Bug Fixes

- Log original client tick for input validation and update toroidal wrapping documentation
- Add warning for missing ServerTick resource and prevent LatestInput creation when SessionShip is absent

### 🚜 Refactor

- Modularize input command processing and improve safety in ECS adapter logic
- Reduce log noise by demoting diagnostic levels, improve integration test robustness, and update RoomBounds schema

### ⚙️ Miscellaneous Tasks

- Vs-01 refinement - sync with client hardening
## [0.8.1] - 2026-04-23

### 🚀 Features

- *(engine)* VS-07 performance and stability hardening baseline
## [0.8.0] - 2026-04-23

### 🚀 Features

- Sync with protocol v0.2.13 and implement replication batching

### 🚜 Refactor

- Define AuthService trait and implement session verification logic in AuthServiceImpl
## [0.7.0] - 2026-04-22
## [0.6.1] - 2026-04-22

### 🚀 Features

- *(engine)* Integrate VS-05/VS-06 session flow and ADR-0003 compliance
- *(ecs-bevy)* Adopt RoomName newtype for RoomDefinition and PermissionString for RoomAccessPolicy

### 🐛 Bug Fixes

- Address PR #45 review findings

### 📚 Documentation

- Update README to reflect VS-06 completion

### 🐛 Bug Fixes

- *(ecs)* Fix silent replication failure: moved `world.clear_trackers()` from `simulate()` into new `post_extract()` hook — physics mutations were invisible to `extract_deltas()`, causing server to broadcast zero world-state updates despite processing inputs correctly. All entity positions were permanently frozen at their spawn coordinates on the client.

### 🚀 Features

- *(ecs)* Add `WorldState::post_extract()` default hook; `BevyWorldAdapter` implements it to call `world.clear_trackers()` at the correct pipeline stage (after Stage 4 extraction, before next tick).

## [0.6.0] - 2026-04-21

### 🚀 Features

- *(engine)* Implement asteroid depletion loop with respawn and metrics
- Integrate protocol v3 and enforce input validation
- *(mining)* Implement input edge-detection and fix respawn timing
- *(mining)* Handle at most one ToggleMining action per tick
## [0.5.1] - 2026-04-21

### ⚙️ Miscellaneous Tasks

- Release v0.5.0
## [0.5.0] - 2026-04-20

### 🚀 Features

- Complete VS-01 engine logic and documentation
- Harden engine security and align specs with 6-stage pipeline
- Gate InputCommand updates on Ownership component

### 🐛 Bug Fixes

- Resolve clippy lints in ecs-bevy
- *(registry)* Sync command tick and add replicator edge cases

### 📚 Documentation

- Standardize readme badges with protocol

## [0.4.1] - 2026-04-20

### 🐛 Bug Fixes

- *(ecs)* Use non-colliding ComponentKind for adapter tests

### 🚜 Refactor

- *(ecs)* Optimize simulation loop and harden component registration
## [0.4.0] - 2026-04-20

### 🚀 Features

- *(ecs)* Synchronize component registry and ship stats per M1020

### 🐛 Bug Fixes

- *(engine)* Resolve architectural findings, fix OTEL URL, and update transform initializers

### 📚 Documentation

- *(engine)* Update README for M1020 milestone
- Fix registry docstrings and optimize Z-clamp change detection
## [0.3.3] - 2026-04-19
## [0.3.2] - 2026-04-19

### 🚀 Features

- *(engine)* Consolidate multirepo (M10146) and upgrade to protocol 0.2.5
## [0.3.1] - 2026-04-19

### ⚙️ Miscellaneous Tasks

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

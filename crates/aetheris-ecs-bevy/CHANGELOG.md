# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
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

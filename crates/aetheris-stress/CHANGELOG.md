# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
## [0.10.1] - 2026-04-26

### ⚙️ Miscellaneous Tasks

- Update Cargo.toml dependencies
## [0.10.0] - 2026-04-26

### 🚀 Features

- *(engine)* Implement authoritative combat loop and cargo drops (VS-03)
## [0.9.0] - 2026-04-25

### 📚 Documentation

- Remove redaction labels from changelogs and security documentation
## [0.8.2] - 2026-04-24

### ⚙️ Miscellaneous Tasks

- Update Cargo.toml dependencies
## [0.8.1] - 2026-04-23

### 🚀 Features

- *(engine)* VS-07 performance and stability hardening baseline
## [0.8.0] - 2026-04-23

### 🚜 Refactor

- Define AuthService trait and implement session verification logic in AuthServiceImpl
## [0.7.0] - 2026-04-22

### 🚀 Features

- *(engine)* Consolidate multirepo (M10146) and upgrade to protocol 0.2.5
- *(engine)* Integrate VS-05/VS-06 session flow and ADR-0003 compliance
- Implement stress test bot and engine optimizations for high concurrency

### 🐛 Bug Fixes

- Address PR #45 review findings
- Remove unused futures dependency in aetheris-stress

### 🚜 Refactor

- Replace broadcast with targeted per-client unreliable sends and optimize stress test and transport configuration

### 📚 Documentation

- Initial commit of engine documentation
- Enhance README with technical summary and links
- Align with aetheris premium templates and port infrastructure
- Fix cross-repo links and placeholders
- Add README.md files to all workspace crates
- *(engine)* Update README for M1020 milestone
- Synchronize workspace crate badges
- Standardize readme badges with protocol
- Include missing ecs-custom and transport-quinn crates in readme
- Update README to reflect VS-06 completion

### ⚙️ Miscellaneous Tasks

- *(engine)* Stabilize infrastructure, fix integration tests, and bump version to 0.2.0

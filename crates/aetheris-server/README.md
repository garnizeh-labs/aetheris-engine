# aetheris-server

The central authoritative simulation server for the Aetheris multiplayer platform.

## Overview

`aetheris-server` is the core authoritative component of the Aetheris Engine. It manages the simulation loop, handles entity replication via interest management, and provides gRPC services for the control plane.

## Features

- **Authoritative Simulation**: Tick-based deterministic execution.
- **Interest Management**: Spatial partitioning for bandwidth optimization.
- **gRPC Services**: Authentication, matchmaking, and transactional operations.
- **Observability**: Built-in Prometheus metrics and OpenTelemetry tracing.

## Usage

For more details, see the [main repository README](https://github.com/garnizeh-labs/aetheris-engine).

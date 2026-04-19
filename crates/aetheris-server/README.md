# aetheris-server

The central authoritative simulation heart of the Aetheris Engine.

## Overview

`aetheris-server` provides the core authoritative simulation loop for the Aetheris multiplayer platform. It is responsible for deterministic tick scheduling, configuration management, and the high-performance bridging of the Simulation and Data Planes.

## Technical Specifications

- **Role**: Authoritative Game Server
- **Capabilities**: Multi-transport support (renet, quinn, wtransport), integrated gRPC control plane, and real-time telemetry.

## Features

- **Authoritative Tick Scheduler**: Ensures sub-millisecond precision for the 60Hz simulation heart.
- **Multi-Transport Bridge**: Seamlessly routes events between native UDP (renet/quinn) and browser-native (WebTransport) connections.
- **Observability**: Built-in Prometheus metrics and OpenTelemetry tracing for production monitoring.
- **Control Plane**: Transactional authentication and matchmaking services via Axum and Tonic.

## Usage

For more details, see the [Engine Design Document](https://github.com/garnizeh-labs/aetheris-engine/blob/main/ENGINE_DESIGN.md).

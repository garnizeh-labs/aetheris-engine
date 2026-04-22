# Aetheris Stress Test Bot

A headless Rust client designed to simulate high-concurrency workloads on the Aetheris Engine. It fulfills the stress-testing requirements outlined in `TESTING_DESIGN.md` and validates the engine's performance under load (P1/P2 milestones).

## Overview

The `aetheris-bot` simulates "headless" players that perform the full game lifecycle:
1.  **Authentication**: Connects to the Control Plane (gRPC) to obtain a session token.
2.  **Transport Connection**: Connects to the Data Plane (UDP/Renet) using the session token.
3.  **Session Lifecycle**: Sends a `StartSession` event and waits for entity possession.
4.  **Input Simulation**: Once possessed, it sends randomized `InputCommand` payloads (movement X/Y) at a fixed 60Hz frequency.
5.  **Metrics Reporting**: Collects and reports performance stats (auth success, connection stability, input throughput) at the end of the run.

## Features

-   **High Concurrency**: Efficiently manages 50+ concurrent clients using `tokio` tasks.
-   **No-Panic Networking**: Robust error handling for network timeouts and protocol mismatches.
-   **Phase 1 Protocol Support**: Uses `SerdeEncoder` (MessagePack) and `RenetTransport` (UDP).
-   **Dynamic Duration**: Supports timed tests with automatic shutdown and summary reporting.
-   **Dev Bypass Support**: Automatically interfaces with the server's authentication bypass in development environments.

## Getting Started

### Prerequisites

-   A running Aetheris Game Server (`just server`).
-   `AETHERIS_AUTH_BYPASS=1` enabled on the server (for bot login).

### Execution

The easiest way to run a stress test is via the engine's `justfile`:

```bash
# Run 50 bots for 30 seconds (default)
just stress

# Run 100 bots for 60 seconds
just stress 100 60
```

Alternatively, run the binary directly:

```bash
cargo run -p aetheris-stress -- --clients 50 --duration 30
```

## CLI Arguments

| Argument | Short | Default | Description |
| :--- | :--- | :--- | :--- |
| `--clients` | `-c` | `50` | Number of concurrent bots to spawn. |
| `--duration` | `-d` | `0` | Test duration in seconds (0 = indefinite). |
| `--auth-host` | | `http://0.0.0.0:50051` | gRPC Auth server endpoint. |
| `--game-host` | | `127.0.0.1:5000` | Game server UDP endpoint. |

## Technical Architecture

The bot is built on the core `aetheris-protocol` traits to ensure it behaves exactly like a real client:
-   **Tonic**: Handles the gRPC authentication flow.
-   **Renet**: Provides the low-level UDP transport layer.
-   **SerdeEncoder**: Serializes inputs into the expected binary format.
-   **Tokio**: Manages the 60Hz tick loop and concurrency.

## Performance Reporting

At the end of a timed run, the bot outputs a summary table:
-   **Success Rates**: Auth, Connection, and Possession percentages.
-   **Throughput**: Total inputs sent and inputs per second (TPS).
-   **Error Log**: Aggregated view of the most common failures encountered during the test.

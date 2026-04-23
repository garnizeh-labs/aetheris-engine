# Run fast quality gate checks (fmt, clippy, test, security, docs-check)

[group('check')]
check: fmt clippy test security docs-check

# Run ALL CI-equivalent checks (fast + docs-strict, udeps)

[group('check')]
check-all: check docs-strict udeps

# Check formatting

[group('lint')]
fmt:
    cargo fmt --all --check

# Link to local aetheris-protocol for development

[group('dev')]
link-dev:
    python3 scripts/dev_toggle.py --enable --path Cargo.toml

# Unlink local aetheris-protocol (switch back to crates.io)

[group('dev')]
unlink-dev:
    python3 scripts/dev_toggle.py --disable --path Cargo.toml

# Run clippy lints

[group('lint')]
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Automatically apply formatting and clippy fixes

[group('lint')]
fix:
    cargo fmt --all
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# Run all unit and integration tests

[group('test')]
test:
    cargo nextest run --workspace --profile ci

# Run a full stress test cycle and persist results in stress_results/<timestamp>
[group('test')]
stress clients='50' duration='30':
    #!/usr/bin/env bash
    set -e
    # Step 1: Build both release binaries upfront so the two cargo processes
    # never contend on the artifact directory lock.
    echo "Building release binaries (server + stress bot)..."
    cargo build --release -p aetheris-server --features phase1
    cargo build --release -p aetheris-stress

    RUN_ID=$(date +%Y%m%d_%H%M%S)
    RESULTS_DIR="stress_results/${RUN_ID}"
    mkdir -p "${RESULTS_DIR}"
    echo "Stress Test Run: ${RUN_ID}"
    echo "# Stress Test Report - ${RUN_ID}" > "${RESULTS_DIR}/README.md"
    echo "" >> "${RESULTS_DIR}/README.md"
    echo "**Configuration:**" >> "${RESULTS_DIR}/README.md"
    echo "- Clients: {{ clients }}" >> "${RESULTS_DIR}/README.md"
    echo "- Duration: {{ duration }}s" >> "${RESULTS_DIR}/README.md"
    echo "- Build: release" >> "${RESULTS_DIR}/README.md"
    echo "" >> "${RESULTS_DIR}/README.md"

    METRICS_PORT=${AETHERIS_METRICS_PORT:-9000}

    # Step 2: Launch pre-built server binary directly (no cargo compilation at runtime)
    AETHERIS_ENV=dev AETHERIS_AUTH_BYPASS=1 RUST_LOG=info \
        ./target/release/aetheris-server > "${RESULTS_DIR}/server.log" 2>&1 &
    SERVER_PID=$!
    echo "Server PID: ${SERVER_PID}"

    # Ensure server is killed on exit
    trap 'echo "Cleaning up server..."; kill "${SERVER_PID}" 2>/dev/null || true; wait "${SERVER_PID}" 2>/dev/null || true' EXIT

    # Step 3: Wait for server to be ready (poll metrics endpoint, max 30s)
    echo "Waiting for server to initialize on port ${METRICS_PORT}..."
    READY=0
    for i in $(seq 1 30); do
        if curl -sf localhost:${METRICS_PORT}/metrics > /dev/null 2>&1; then
            echo "Server ready after ${i}s"
            READY=1
            break
        fi
        sleep 1
    done

    if [ $READY -ne 1 ]; then
        echo "Error: Server failed to become ready within 30s"
        exit 1
    fi

    # Step 4: Run stress bot (pre-built binary)
    echo "Stress test in progress ({{clients}} clients, {{duration}}s)..."
    ./target/release/aetheris-bot --clients {{clients}} --duration {{duration}} > "${RESULTS_DIR}/bot.log" 2>&1
    BOT_EXIT=$?

    # Step 5: Capture metrics immediately after bot finishes
    echo "Capturing server metrics from port ${METRICS_PORT}..."
    curl -s localhost:${METRICS_PORT}/metrics > "${RESULTS_DIR}/server_metrics.txt" || echo "Failed to capture metrics"

    if [ $BOT_EXIT -ne 0 ]; then
        echo "Error: Stress bot failed with exit code $BOT_EXIT"
        exit $BOT_EXIT
    fi

    # Step 6: Stop server (handled by trap EXIT, but we can do it explicitly here too if we want early return)

    echo "## Final Summary" >> "${RESULTS_DIR}/README.md"
    echo '```text' >> "${RESULTS_DIR}/README.md"
    awk '/====/{i++} i==1, i==3' "${RESULTS_DIR}/bot.log" >> "${RESULTS_DIR}/README.md"
    echo '```' >> "${RESULTS_DIR}/README.md"
    echo "" >> "${RESULTS_DIR}/README.md"
    echo "## Server Metrics" >> "${RESULTS_DIR}/README.md"
    echo '```text' >> "${RESULTS_DIR}/README.md"
    grep "aetheris_tick_duration" "${RESULTS_DIR}/server_metrics.txt" | grep -v "#" >> "${RESULTS_DIR}/README.md" || echo "No metrics found" >> "${RESULTS_DIR}/README.md"
    echo '```' >> "${RESULTS_DIR}/README.md"
    echo ""
    awk '/====/{i++} i==1, i==3' "${RESULTS_DIR}/bot.log"
    echo ""
    echo "Results persisted in: ${RESULTS_DIR}"

# Run security audits (licenses, advisories, vulnerabilities)

[group('security')]
security:
    cargo deny check
    cargo audit

# Build documentation

[group('doc')]
docs:
    cargo doc --workspace --no-deps

# Check documentation quality (linting, frontmatter, spelling, links, branding)

[group('doc')]
docs-check:
    python3 scripts/doc_lint.py
    python3 scripts/check_links.py
    python3 scripts/check_branding.py
    uvx codespell

# Build documentation (mirrors the CI job — warnings are errors)

[group('doc')]
docs-strict:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Run the game server (debug build, background)

[group('run')]
server:
    AETHERIS_ENV=dev AETHERIS_AUTH_BYPASS=1 RUST_LOG=info cargo run -p aetheris-server --features phase1 &

# Start server for stress test with output redirected to a specific file
_stress-server log_file:
    @AETHERIS_ENV=dev AETHERIS_AUTH_BYPASS=1 RUST_LOG=info cargo run -p aetheris-server --release --features phase1 > {{ log_file }} 2>&1 &

# Run the game server with ECS possession/input debug logging (foreground)
# Shows [apply_updates] ownership checks, [InputCmd] gate checks, and [spawn_*] events.
# Use this to diagnose "wrong entity receives input" problems.

[group('run')]
server-debug:
    AETHERIS_ENV=dev AETHERIS_AUTH_BYPASS=1 \
    RUST_LOG="info,aetheris_ecs_bevy=debug" \
    cargo run -p aetheris-server --features phase1

# Run a lightweight server for telemetry only

[group('run')]
server-telemetry:
    RUST_LOG=info cargo run -p aetheris-server --no-default-features &

# Run the game server (release build, background)

[group('run')]
server-release:
    cargo build -p aetheris-server --release --features phase1
    cargo run -p aetheris-server --release --features phase1 &

# Run server with full observability

[group('run')]
server-obs:
    @mkdir -p logs
    cargo build -p aetheris-server --release --features phase1
    LOG_FORMAT=json OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 RUST_LOG=info \
        AETHERIS_ENV=dev AETHERIS_AUTH_BYPASS=1 ./target/release/aetheris-server >> logs/server.log 2>&1 &

# Stop all background processes

[group('maintenance')]
stop:
    -fuser -k 9000/tcp 4433/udp 5000/udp 50051/tcp >/dev/null 2>&1 || true

# Pinned nightly for udeps / wasm (matches Aetheris workspace)

wasm_nightly := "nightly-2025-07-01"

# Check for unused dependencies (requires nightly; runs on main in CI)

[group('lint')]
udeps:
    cargo +{{wasm_nightly}} udeps --workspace --all-targets --all-features

# Remove all build artefacts reproducible via just build

[group('maintenance')]
clean:
    cargo clean

# Check semver compatibility for library crates before a release

# Check semver compatibility for library crates before a release

[group('release')]
semver:
    cargo semver-checks --workspace

# Follow logs from the last session
[group('maintenance')]
logs:
    @mkdir -p logs
    tail -f logs/*.log || true

# Run all microbenchmarks
[group('bench')]
bench:
    cargo bench --workspace

# Run benchmarks and seal baseline with a tag
[group('bench')]
bench-baseline tag:
    cargo bench --workspace -- --save-baseline {{ tag }}


# Record golden file for determinism validation (VS-07 §3.3)
[group('bench')]
record-golden:
    AETHERIS_RECORD_GOLDEN=600 AETHERIS_AUTH_BYPASS=1 AETHERIS_METRICS_PORT=9001 AETHERIS_GRPC_ADDR=127.0.0.1:50052 AETHERIS_TELEMETRY_HTTP_PORT=50056 AETHERIS_RENET_ADDR=0.0.0.0:5001 AETHERIS_WT_ADDR=[::]:4434 rtk cargo run -p aetheris-server --features phase1 -- --tick-rate 60

# Run benchmarks and record results in benchmarks/<timestamp>
[group('bench')]
bench-record:
    #!/usr/bin/env bash
    set -e
    RUN_ID=$(date +%Y-%m-%d_%H%M)
    RESULTS_DIR="benchmarks/${RUN_ID}"
    mkdir -p "${RESULTS_DIR}"
    echo "Running benchmarks..."
    cargo bench --workspace
    echo "Copying results to ${RESULTS_DIR}..."
    cp -r target/criterion "${RESULTS_DIR}/criterion"
    echo "Done. Results persisted in: ${RESULTS_DIR}"

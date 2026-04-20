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
    AETHERIS_AUTH_BYPASS=1 RUST_LOG=info cargo run -p aetheris-server --features phase1 &

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
    LOG_FORMAT=json OTEL_EXPORTER_OTLP_ENDPOINT=<http://localhost:4317> RUST_LOG=info \
        AETHERIS_AUTH_BYPASS=1 ./target/release/aetheris-server >> logs/server.log 2>&1 &

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

[group('release')]
semver:
    cargo semver-checks --workspace

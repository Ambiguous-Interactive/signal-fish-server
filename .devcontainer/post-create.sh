#!/usr/bin/env bash
# Signal Fish Server — Post-create setup for the dev container
set -euo pipefail

echo ""
echo "============================================"
echo "  Signal Fish Server — Setting up dev env"
echo "============================================"
echo ""

# Pre-download all dependencies
echo "[setup] Fetching cargo dependencies..."
cargo fetch
echo "[setup] Dependencies fetched."

echo "[setup] Pre-building (cargo check --all-features)..."
cargo check --all-features 2>&1 || echo "[setup] Warning: cargo check failed, continuing..."
echo "[setup] Build cache warmed."

# Make project scripts executable
if [ -d "scripts" ]; then
    chmod +x scripts/*.sh
    echo "[setup] Made scripts/*.sh executable."
fi

# Install git hooks if the script exists
if [ -f "scripts/enable-hooks.sh" ]; then
    bash scripts/enable-hooks.sh --quiet
    echo "[setup] Git hooks configured."
fi

echo ""
echo "============================================"
echo "  Signal Fish Server — Ready!"
echo "============================================"
echo ""
echo "  Useful commands:"
echo ""
echo "    cargo build                         Build the server"
echo "    cargo run                           Run the server (port 3536)"
echo "    cargo test --all-features           Run all tests"
echo "    cargo nextest run --all-features    Run tests with nextest"
echo "    cargo clippy --all-targets --all-features"
echo "                                        Lint with clippy"
echo "    cargo fmt                           Format code"
echo "    cargo deny check                    Check dependencies"
echo "    cargo llvm-cov --all-features --html"
echo "                                        Generate coverage report"
echo "    cargo bench                         Run benchmarks"
echo ""
echo "  Full check (mandatory before commit):"
echo "    cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features"
echo ""
echo "  VS Code tasks are available via Terminal > Run Task"
echo ""

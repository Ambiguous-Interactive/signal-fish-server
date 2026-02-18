#!/usr/bin/env bash

# run-server.sh - Run the Signal Fish server locally
#
# Sets sensible default environment variables and launches the server
# via cargo run. Any extra arguments are forwarded to cargo run.
#
# Usage:
#   ./scripts/run-server.sh                    # Run with defaults
#   ./scripts/run-server.sh --release          # Run in release mode
#   ./scripts/run-server.sh -- --help          # Pass args to the binary

set -euo pipefail

echo "Starting Signal Fish server..."

# Set default environment variables if not set
export SIGNAL_FISH__PORT=${SIGNAL_FISH__PORT:-3536}
export SIGNAL_FISH__LOGGING__LEVEL=${SIGNAL_FISH__LOGGING__LEVEL:-info}

echo "Port: $SIGNAL_FISH__PORT"
echo "Log level: $SIGNAL_FISH__LOGGING__LEVEL"

cargo run "$@"

#!/usr/bin/env bash

# upgrade-deps.sh - Upgrade Rust dependencies for Signal Fish Server
#
# Runs cargo update to upgrade all Rust dependencies to their latest
# compatible versions, then reports results.
#
# Usage:
#   ./scripts/upgrade-deps.sh

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_status() {
	echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
	echo -e "${GREEN}[OK]${NC} $1"
}

print_error() {
	echo -e "${RED}[FAIL]${NC} $1"
}

echo "Running dependency upgrades for Signal Fish Server..."

# Check that cargo is available
if ! command -v cargo >/dev/null 2>&1; then
	print_error "cargo not found on PATH. Please install Rust."
	exit 1
fi

CARGO_V=$(cargo --version)
echo "Cargo: $CARGO_V"

FAILED=0

# Update Rust dependencies
echo ""
print_status "Updating Rust dependencies..."

if cargo update; then
	print_success "Rust dependencies updated successfully"
else
	print_error "cargo update failed"
	FAILED=1
fi

# Summary
echo ""
echo "========================================"
echo "Upgrade summary:"

if [ "$FAILED" -eq 0 ]; then
	echo "  - rust: ok (cargo update completed)"
	print_success "All dependency upgrades completed successfully."
else
	echo "  - rust: failed (cargo update)"
	print_error "Some dependency upgrades failed."
fi

echo "========================================"

exit $FAILED

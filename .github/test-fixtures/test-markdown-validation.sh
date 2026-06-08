#!/usr/bin/env bash
#
# Compatibility wrapper for the canonical markdown fixture validation.
#
# This file remains for existing developer muscle memory. The robust extractor
# parity checks live in validate-test-cases.sh and are run directly by CI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$SCRIPT_DIR/validate-test-cases.sh"

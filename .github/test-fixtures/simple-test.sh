#!/usr/bin/env bash
#
# Compatibility wrapper for the canonical markdown fixture validation.
#
# Historically this script carried a separate validation implementation. Keep it
# as an entry point for old local workflows, but delegate to the single source
# of truth so local checks cannot drift from CI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$SCRIPT_DIR/validate-test-cases.sh"

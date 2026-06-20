#!/usr/bin/env bash
# Run the Z3 SMT proofs of the protocol v3 pure-logic invariants.
#
# The SMT counterpart to scripts/run-tla-model-check.sh: where TLC explores
# reachable states, these proofs discharge universally quantified properties of
# the pure decision functions (ladder selection, all_support relay-floor
# invariant, glare rule, host election) over unbounded inputs.
#
# Requires Python 3 with the `z3` module. The dev container ships `python3-z3`
# (apt); CI installs `z3-solver` (pip wheel). If neither is present this script
# attempts a best-effort install, then fails loudly if z3 is still unavailable.
#
# Usage: bash scripts/run-z3-proofs.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROOFS="${ROOT}/formal/z3/protocol_invariants.py"

if [[ ! -f "${PROOFS}" ]]; then
  echo "error: ${PROOFS} not found" >&2
  exit 2
fi

python3 -c "import z3" 2>/dev/null || {
  echo "z3 python module not found; attempting install..." >&2
  # Install via `python3 -m pip` (not bare `pip`) so the wheel lands in the SAME
  # interpreter that imports z3 above and runs the proofs below; a bare `pip` can
  # resolve to a different Python and silently install where it is never seen.
  python3 -m pip install --quiet z3-solver 2>/dev/null \
    || python3 -m pip install --quiet --break-system-packages z3-solver 2>/dev/null \
    || { echo "error: could not import or install the z3 module" >&2; exit 2; }
}

echo "Running Z3 protocol-invariant proofs (${PROOFS})"
exec python3 "${PROOFS}"

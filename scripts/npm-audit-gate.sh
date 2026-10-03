#!/usr/bin/env bash
# Signal Fish Server - npm audit gate
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Fails on any npm audit advisory that is not explicitly accepted for the
# audited npm graph. This mirrors deny.toml's documented advisory ignore for
# the npm graphs in this repository (root tooling, browser reference client).
#
# Why not plain `npm audit`? npm has no advisory-ignore mechanism, so a single
# unfixable advisory (no patched release exists upstream) would keep the gate
# permanently red and bury new findings. The allowlist keeps the gate
# actionable: any advisory outside the list fails, and every accepted entry
# must carry a justification plus a revisit date and starts failing again as
# soon as npm stops reporting it for its graph, so stale entries are pruned.
# Entries are scoped per graph because each graph resolves its own tree.
#
# The registry's advisory bulk endpoint answers 503 under load, which failed
# the whole lane on an otherwise-green tree (PR #533, 2026-09-04). Attempts
# that do not produce a parseable report are treated as transient and retried
# with backoff; a parseable report is authoritative whether npm exits 0
# (clean) or 1 (findings), and is never retried (issue #601: a real finding
# must never be swallowed behind a retry loop's exit status).
#
# Usage:
#   bash scripts/npm-audit-gate.sh root       # audit the repository root graph
#   bash scripts/npm-audit-gate.sh browser    # audit the browser client graph
#
# Exit codes:
#   0 = every reported advisory is accepted and every entry still applies
#   1 = unaccepted advisory, stale allowlist entry, or audit failure
#   2 = npm unavailable or invalid usage

set -euo pipefail

# Per-graph advisories accepted as unfixable. One GHSA id per entry with a
# justification and a revisit date, in the same spirit as deny.toml's
# [advisories] ignore. An entry must still be reported by `npm audit` for its
# graph; when an advisory disappears, remove the entry.
ROOT_ALLOWED_ADVISORIES=(
    # GHSA-vfj7-8cjw-p6xm (CVE-2026-93687; braces <= 3.0.3, stack-exhaustion DoS
    # through deeply nested glob patterns): no patched braces release exists and
    # npm's only "fix" is downgrading markdownlint-cli2 to 0.0.4, which is a
    # non-remediation. The chain is dev tooling only
    # (markdownlint-cli2 -> micromatch -> braces) over maintainer-controlled
    # patterns, not untrusted input. Tracked upstream: micromatch/braces#70.
    # Revisit by 2026-12-01.
    "GHSA-vfj7-8cjw-p6xm"
)
BROWSER_ALLOWED_ADVISORIES=()

GRAPH="${1:-}"
if [ "$#" -gt 1 ]; then
    echo "Usage: $0 [root|browser]" >&2
    exit 2
fi
case "$GRAPH" in
    root)
        ALLOWED_ADVISORIES=("${ROOT_ALLOWED_ADVISORIES[@]+"${ROOT_ALLOWED_ADVISORIES[@]}"}")
        GRAPH_DIR="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
        ;;
    browser)
        ALLOWED_ADVISORIES=("${BROWSER_ALLOWED_ADVISORIES[@]+"${BROWSER_ALLOWED_ADVISORIES[@]}"}")
        GRAPH_DIR="$(git rev-parse --show-toplevel 2>/dev/null || pwd)/clients/browser"
        ;;
    *)
        echo "Usage: $0 [root|browser]" >&2
        exit 2
        ;;
esac
if [ ! -f "$GRAPH_DIR/package.json" ]; then
    echo "[FAIL] $GRAPH: no package.json at $GRAPH_DIR" >&2
    exit 2
fi
cd "$GRAPH_DIR"

if ! command -v npm >/dev/null 2>&1; then
    echo "[FAIL] npm is not installed or not on PATH" >&2
    exit 2
fi
if ! command -v node >/dev/null 2>&1; then
    echo "[FAIL] node is not installed or not on PATH (required to parse the audit report)" >&2
    exit 2
fi

REPORT=$(mktemp)
STDERR=$(mktemp)
trap 'rm -f "$REPORT" "${REPORT}.advisories" "$STDERR"' EXIT

# A parseable audit report is authoritative whether it exits 0 (clean) or 1
# (findings); only unparsable or malformed output is transient (issue #601:
# never swallow a real finding behind the retry loop's exit status). The
# shape probe also rejects parseable non-report JSON (an error body), which
# would otherwise read as "zero advisories" — a false green on graphs whose
# allowlist is empty.
ATTEMPTS_OK=false
for attempt in 1 2 3; do
    if npm audit --json >"$REPORT" 2>"$STDERR"; then
        ATTEMPTS_OK=true
        break
    fi
    if node -e '
const report = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (report === null || typeof report !== "object" ||
    report.vulnerabilities === null || typeof report.vulnerabilities !== "object") {
    process.exit(1);
}
' "$REPORT" 2>/dev/null; then
        ATTEMPTS_OK=true
        break
    fi
    echo "audit attempt $attempt did not produce a report; retrying after backoff" >&2
    sleep "$((attempt * 15))"
done

if [ "$ATTEMPTS_OK" != true ]; then
    echo "[FAIL] $GRAPH: npm audit failed after 3 attempts without a usable report" >&2
    echo "--- npm audit stderr (last 20 lines) ---" >&2
    tail -n 20 "$STDERR" >&2 || true
    exit 1
fi

# Emit "GHSA-id<TAB>package<TAB>severity<TAB>title<TAB>url" per unique advisory.
node -e '
const report = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
const advisories = new Map();
for (const vuln of Object.values(report.vulnerabilities || {})) {
    for (const via of vuln.via || []) {
        if (typeof via === "object" && via !== null && typeof via.url === "string") {
            const ghsa = via.url.match(/GHSA-[A-Za-z0-9-]+/);
            if (ghsa) {
                advisories.set(ghsa[0], {
                    id: ghsa[0],
                    package: via.name || vuln.name,
                    severity: via.severity || vuln.severity || "unknown",
                    title: via.title || "",
                    url: via.url,
                });
            }
        }
    }
}
for (const a of advisories.values()) {
    console.log([a.id, a.package, a.severity, a.title, a.url].join("\t"));
}
' "$REPORT" >"${REPORT}.advisories"

FAILED=0
UNACCEPTED=0
while IFS=$'\t' read -r id package severity title url; do
    [ -n "$id" ] || continue
    accepted=false
    for allowed in ${ALLOWED_ADVISORIES[@]+"${ALLOWED_ADVISORIES[@]}"}; do
        if [ "$id" = "$allowed" ]; then
            accepted=true
            break
        fi
    done
    if [ "$accepted" = true ]; then
        echo "[OK]   $GRAPH: $id ($package, $severity): accepted advisory - $title"
        echo "       $url"
    else
        UNACCEPTED=$((UNACCEPTED + 1))
        echo "[FAIL] $GRAPH: $id ($package, $severity): $title"
        echo "       $url"
    fi
done <"${REPORT}.advisories"

for allowed in ${ALLOWED_ADVISORIES[@]+"${ALLOWED_ADVISORIES[@]}"}; do
    if ! awk -F'\t' -v id="$allowed" '$1 == id { found = 1; exit } END { exit !found }' "${REPORT}.advisories"; then
        echo "[FAIL] $GRAPH: allowlist entry $allowed is no longer reported by npm audit; remove it from scripts/npm-audit-gate.sh"
        FAILED=1
    fi
done

if [ "$UNACCEPTED" -gt 0 ]; then
    echo ""
    echo "[FAIL] $GRAPH: $UNACCEPTED unaccepted npm audit advisory/advisories."
    echo "Resolve by upgrading to a patched version, or - only when no fix exists -"
    echo "add a documented entry to that graph's ALLOWED_ADVISORIES list in"
    echo "scripts/npm-audit-gate.sh with a justification and a revisit date."
    FAILED=1
fi

if [ "$FAILED" = 0 ]; then
    echo "[OK] $GRAPH: npm audit clean (unaccepted advisories: none)"
fi
exit "$FAILED"

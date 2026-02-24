#!/usr/bin/env bash
# Documentation + changelog consistency checks.
#
# Enforces:
# 1) Selected project version references are synchronized with Cargo.toml package version.
# 2) CHANGELOG.md follows Keep a Changelog structure and link conventions.
# 3) Non-internal changed files are accompanied by a CHANGELOG.md update.
# 4) README/.llm protocol quick references do not drift to removed message shapes.
#
# Usage:
#   ./scripts/check-doc-consistency.sh
#   ./scripts/check-doc-consistency.sh --staged
#   ./scripts/check-doc-consistency.sh --changed-files <file1> <file2> ...

set -euo pipefail

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

ERRORS=0
WARNINGS=0
CHANGED_MODE=none

declare -a CHANGED_FILES=()

action_error() {
    echo -e "${RED}[ERROR]${NC} $1"
    ERRORS=$((ERRORS + 1))
}

action_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
    WARNINGS=$((WARNINGS + 1))
}

action_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

action_ok() {
    echo -e "${GREEN}[OK]${NC} $1"
}

usage() {
    cat <<'USAGE'
Usage:
  ./scripts/check-doc-consistency.sh
  ./scripts/check-doc-consistency.sh --staged
  ./scripts/check-doc-consistency.sh --changed-files <file1> <file2> ...
USAGE
}

# Parse args
while [ "$#" -gt 0 ]; do
    case "$1" in
        --staged)
            CHANGED_MODE=staged
            shift
            ;;
        --changed-files)
            CHANGED_MODE=explicit
            shift
            if [ "$#" -eq 0 ]; then
                action_error "--changed-files requires at least one file path"
                usage
                exit 2
            fi
            while [ "$#" -gt 0 ]; do
                CHANGED_FILES+=("$1")
                shift
            done
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            action_error "Unknown argument: $1"
            usage
            exit 2
            ;;
    esac
done

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

action_info "Doc consistency check"
action_info "Repository: $REPO_ROOT"

read_cargo_package_version() {
    awk '
        BEGIN { in_package = 0 }
        /^\[package\][[:space:]]*$/ { in_package = 1; next }
        /^\[[^]]+\][[:space:]]*$/ { if (in_package == 1) in_package = 0 }
        in_package == 1 {
            if ($0 ~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/) {
                line = $0
                sub(/^[[:space:]]*version[[:space:]]*=[[:space:]]*"/, "", line)
                sub(/".*$/, "", line)
                print line
                exit
            }
        }
    ' Cargo.toml
}

CARGO_VERSION=$(read_cargo_package_version || true)
if [ -z "$CARGO_VERSION" ]; then
    action_error "Could not parse [package].version from Cargo.toml"
else
    action_ok "Cargo package version: $CARGO_VERSION"
fi

# ---------------------------------------------------------------------------
# 1) Version sync checks
# ---------------------------------------------------------------------------
validate_signal_fish_dependency_versions() {
    local file="$1"
    if [ ! -f "$file" ]; then
        action_error "Missing required file: $file"
        return
    fi

    local found=0
    while IFS= read -r line; do
        found=1

        # Allow placeholders in template docs.
        if [[ "$line" == *"<version>"* ]]; then
            continue
        fi

        local observed=""
        observed=$(printf '%s\n' "$line" \
            | sed -nE 's/.*signal-fish-server[[:space:]]*=[[:space:]]*\{[^}]*version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p')
        if [ -z "$observed" ]; then
            observed=$(printf '%s\n' "$line" \
                | sed -nE 's/.*signal-fish-server[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p')
        fi

        if [ -z "$observed" ]; then
            action_error "$file contains a signal-fish-server dependency line without a parseable version: $line"
            continue
        fi

        if [ "$observed" != "$CARGO_VERSION" ]; then
            action_error "$file has stale signal-fish-server version '$observed' (expected '$CARGO_VERSION' from Cargo.toml)"
        fi
    done < <(grep -E 'signal-fish-server[[:space:]]*=' "$file" || true)

    if [ "$found" -eq 0 ]; then
        action_warn "$file contains no signal-fish-server dependency example"
    fi
}

if [ -n "$CARGO_VERSION" ]; then
    validate_signal_fish_dependency_versions "docs/library-usage.md"

    if [ ! -f ".llm/context.md" ]; then
        action_error "Missing required file: .llm/context.md"
    else
        expected_context_line="- **Version:** $CARGO_VERSION"
        if grep -Fqx -- "$expected_context_line" .llm/context.md; then
            action_ok ".llm/context.md version line matches Cargo.toml"
        else
            action_error ".llm/context.md must contain exact line: $expected_context_line"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# 2) CHANGELOG keep-a-changelog structure checks
# ---------------------------------------------------------------------------
validate_changelog() {
    local file="CHANGELOG.md"
    if [ ! -f "$file" ]; then
        action_error "Missing required file: CHANGELOG.md"
        return
    fi

    if ! grep -q "Keep a Changelog" "$file"; then
        action_error "CHANGELOG.md must reference Keep a Changelog"
    fi

    if ! grep -q '^## \[Unreleased\]$' "$file"; then
        action_error "CHANGELOG.md must contain an exact '## [Unreleased]' heading"
    fi

    if grep -q '^## Unreleased$' "$file"; then
        action_error "Use '## [Unreleased]' (bracketed) instead of '## Unreleased'"
    fi

    # Validate unreleased section headings.
    local in_unreleased=0
    local line
    while IFS= read -r line; do
        if [[ "$line" =~ ^##[[:space:]]+\[Unreleased\]$ ]]; then
            in_unreleased=1
            continue
        fi

        if [[ "$line" =~ ^##[[:space:]]+\[.*\] ]]; then
            in_unreleased=0
            continue
        fi

        if [ "$in_unreleased" -eq 1 ] && [[ "$line" =~ ^###[[:space:]]+(.+)$ ]]; then
            section="${BASH_REMATCH[1]}"
            case "$section" in
                Added|Changed|Deprecated|Removed|Fixed|Security)
                    ;;
                *)
                    action_error "CHANGELOG.md has non-keep-a-changelog section under [Unreleased]: '$section'"
                    ;;
            esac
        fi
    done < "$file"

    # Disallow undated current-version headers.
    if [ -n "$CARGO_VERSION" ]; then
        if grep -q "^## \[$CARGO_VERSION\]$" "$file"; then
            action_error "CHANGELOG.md has undated current-version header '## [$CARGO_VERSION]'. Use '## [$CARGO_VERSION] - YYYY-MM-DD' only at release cutover."
        fi

        if grep -q "^## \[$CARGO_VERSION\][[:space:]]*-[[:space:]]*" "$file"; then
            # Dated is acceptable.
            :
        elif grep -q "^## \[$CARGO_VERSION\]" "$file"; then
            action_error "CHANGELOG.md has malformed current-version header for $CARGO_VERSION"
        fi
    fi

    # Collect released versions from headers.
    local versions
    versions=$(grep -E '^## \[[0-9]+\.[0-9]+\.[0-9]+\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$' "$file" \
        | sed -E 's/^## \[([0-9]+\.[0-9]+\.[0-9]+)\].*/\1/' || true)

    if [ -z "$versions" ]; then
        action_warn "No dated release sections found in CHANGELOG.md"
        return
    fi

    local latest_version
    latest_version=$(printf '%s\n' "$versions" | sort -V | tail -n 1)

    local unreleased_ref
    unreleased_ref=$(grep -E '^\[Unreleased\]:' "$file" | sed -E 's/^\[Unreleased\]:[[:space:]]*(\S+).*$/\1/' || true)
    if [ -z "$unreleased_ref" ]; then
        action_error "CHANGELOG.md must define a [Unreleased]: link reference"
    else
        if [[ ! "$unreleased_ref" =~ /compare/v${latest_version}\.\.\.HEAD([#?].*)?$ ]]; then
            action_error "[Unreleased] link must compare from v${latest_version}...HEAD (found: $unreleased_ref)"
        fi
    fi

    local latest_ref
    latest_ref=$(grep -E "^\[$latest_version\]:" "$file" | sed -E "s/^\[$latest_version\]:[[:space:]]*(\\S+).*$/\\1/" || true)
    if [ -z "$latest_ref" ]; then
        action_error "CHANGELOG.md must define a [$latest_version]: link reference"
    else
        if [[ "$latest_ref" =~ /releases/tag/v${latest_version}([#?].*)?$ ]]; then
            :
        elif [[ "$latest_ref" =~ /compare/v[0-9]+\.[0-9]+\.[0-9]+\.\.\.v${latest_version}([#?].*)?$ ]]; then
            :
        else
            action_error "[$latest_version] link must point to /releases/tag/v$latest_version or /compare/vPREV...v$latest_version (found: $latest_ref)"
        fi
    fi
}

validate_changelog

# ---------------------------------------------------------------------------
# 3) Protocol quick-reference anti-drift checks
# ---------------------------------------------------------------------------
validate_protocol_stale_tokens() {
    local file="$1"
    if [ ! -f "$file" ]; then
        action_error "Missing required file: $file"
        return
    fi

    # Removed/stale names from older protocol revisions.
    local stale
    for stale in "server_version" "CreateRoom" "SetReady" "RoomCreated" "AuthorityGranted"; do
        if grep -q "$stale" "$file"; then
            action_error "$file still references stale protocol token '$stale'"
        fi
    done

}

validate_protocol_authenticated_payload_shape() {
    local file="$1"
    if [ ! -f "$file" ]; then
        action_error "Missing required file: $file"
        return
    fi

    if ! grep -q '"app_name"' "$file"; then
        action_error "$file must include Authenticated payload field 'app_name'"
    fi
    if ! grep -q '"rate_limits"' "$file"; then
        action_error "$file must include Authenticated payload field 'rate_limits'"
    fi
}

validate_protocol_sample_reference() {
    local markdown_file="$1"
    local sample_reference="$2"
    if [ ! -f "$markdown_file" ]; then
        action_error "Missing required file: $markdown_file"
        return
    fi

    if ! grep -Fq "$sample_reference" "$markdown_file"; then
        action_error "$markdown_file must reference canonical protocol sample: $sample_reference"
    fi
}

PROTOCOL_SAMPLE_CLIENT=".llm/code-samples/protocol/v2-client-messages.jsonl"
PROTOCOL_SAMPLE_SERVER=".llm/code-samples/protocol/v2-server-messages.jsonl"

validate_protocol_stale_tokens "README.md"
validate_protocol_stale_tokens ".llm/context.md"
validate_protocol_stale_tokens "$PROTOCOL_SAMPLE_CLIENT"
validate_protocol_stale_tokens "$PROTOCOL_SAMPLE_SERVER"
validate_protocol_authenticated_payload_shape "$PROTOCOL_SAMPLE_SERVER"

validate_protocol_sample_reference "README.md" "$PROTOCOL_SAMPLE_CLIENT"
validate_protocol_sample_reference "README.md" "$PROTOCOL_SAMPLE_SERVER"
validate_protocol_sample_reference ".llm/context.md" "code-samples/protocol/v2-client-messages.jsonl"
validate_protocol_sample_reference ".llm/context.md" "code-samples/protocol/v2-server-messages.jsonl"

# ---------------------------------------------------------------------------
# 4) Changelog-required gate for non-internal changed files
# ---------------------------------------------------------------------------
is_internal_path() {
    local path="$1"
    case "$path" in
        .github/*|.githooks/*|scripts/*|tests/*|test-fixtures/*|.llm/*|target/*|progress/*)
            return 0
            ;;
        Cargo.lock|PLAN.md|pre-push.txt|logs_*.zip)
            return 0
            ;;
        .markdownlint*|.lychee.toml|.typos.toml|.yamllint.yml)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

collect_changed_files() {
    if [ "$CHANGED_MODE" = "staged" ]; then
        mapfile -t CHANGED_FILES < <(git diff --cached --name-only --diff-filter=ACMRTUXB)
    fi
}

collect_changed_files

if [ "$CHANGED_MODE" != "none" ]; then
    action_info "Evaluating changelog gate for changed files mode: $CHANGED_MODE"

    local_has_changelog=0
    for path in "${CHANGED_FILES[@]}"; do
        if [ "$path" = "CHANGELOG.md" ]; then
            local_has_changelog=1
            break
        fi
    done

    declare -a NON_INTERNAL_CHANGED=()
    for path in "${CHANGED_FILES[@]}"; do
        [ -z "$path" ] && continue
        [ "$path" = "CHANGELOG.md" ] && continue
        if is_internal_path "$path"; then
            continue
        fi
        NON_INTERNAL_CHANGED+=("$path")
    done

    if [ "${#NON_INTERNAL_CHANGED[@]}" -gt 0 ] && [ "$local_has_changelog" -ne 1 ]; then
        action_error "Detected non-internal changes without CHANGELOG.md update:"
        for path in "${NON_INTERNAL_CHANGED[@]}"; do
            echo "  - $path"
        done
        echo "Add a Keep a Changelog entry under '## [Unreleased]' for user-facing impact."
    elif [ "${#NON_INTERNAL_CHANGED[@]}" -gt 0 ]; then
        action_ok "CHANGELOG.md updated alongside non-internal changes"
    else
        action_ok "No non-internal changed files detected for changelog gate"
    fi
fi

echo
if [ "$ERRORS" -gt 0 ]; then
    action_error "Doc consistency checks failed with $ERRORS error(s) and $WARNINGS warning(s)"
    exit 1
fi

if [ "$WARNINGS" -gt 0 ]; then
    action_warn "Doc consistency checks passed with $WARNINGS warning(s)"
else
    action_ok "Doc consistency checks passed"
fi

exit 0

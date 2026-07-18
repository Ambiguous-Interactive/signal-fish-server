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
#   ./scripts/check-doc-consistency.sh [--skip-changelog-gate] [--staged|--changed-files ...]

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
VERSION_DRIFT=0
CHANGED_MODE=none
SKIP_CHANGELOG_GATE=0

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
  ./scripts/check-doc-consistency.sh --skip-changelog-gate [--staged|--changed-files ...]

Options:
  --skip-changelog-gate  Skip the changelog-required gate (check 3). All other
                         consistency checks still run. Intended for automated
                         PRs (e.g. dependabot) where changelog entries are not
                         expected.
USAGE
}

# Parse args
while [ "$#" -gt 0 ]; do
    case "$1" in
        --staged)
            CHANGED_MODE=staged
            shift
            ;;
        --skip-changelog-gate)
            SKIP_CHANGELOG_GATE=1
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
                if [[ "$1" == --* ]]; then
                    action_error "Unexpected flag '$1' after --changed-files (flags like --skip-changelog-gate must come before --changed-files)"
                    usage
                    exit 2
                fi
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
# Every doc that quotes the crate as a dependency (`signal-fish-server = "X"` or
# `signal-fish-server = { version = "X" }`) must pin the SAME version as the
# Cargo.toml [package].version. Discovery is a filesystem SUPERSET scan, not a
# hand-maintained file list: any tracked Markdown doc that quotes the version is
# covered automatically and can never drift silently -- the exact class of bug
# that broke six CI jobs after the 0.2.0 -> 0.3.0 bump. Generated/vendored trees
# are skipped so a stale rendered copy of an example cannot false-positive.

# Dependency-line shape: `signal-fish-server =` followed by a quote (`"X"`) or a
# brace table (`{ version = "X" }`). Anchoring on the value start avoids matching
# prose that merely mentions the crate name. Single source of truth for the
# discovery scan and the per-file validator below.
SFS_DEP_PATTERN='signal-fish-server[[:space:]]*=[[:space:]]*["{]'

# The canonical usage doc must always exist and carry an example, so the scan can
# never pass vacuously if it is renamed or loses its dependency snippet.
CANONICAL_USAGE_DOC="docs/library-usage.md"

validate_signal_fish_dependency_versions() {
    local file="$1"
    while IFS= read -r line; do
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
            # Versionless path/git/workspace/registry examples cannot be
            # auto-synced when Cargo.toml changes, so they are rejected just
            # like malformed dependency examples. Use an explicit current
            # version or the `<version>` placeholder in template docs.
            action_error "$file contains a signal-fish-server dependency line without a parseable version: $line"
            continue
        fi

        if [ "$observed" != "$CARGO_VERSION" ]; then
            action_error "$file has stale signal-fish-server version '$observed' (expected '$CARGO_VERSION' from Cargo.toml)"
            VERSION_DRIFT=1
        fi
    done < <(grep -E "$SFS_DEP_PATTERN" "$file" || true)
}

# All first-party Markdown docs, discovered from the filesystem (works in the
# non-git fixture harness too) and excluding generated, vendored, and fixture
# trees. NUL-delimited (-print0) for path-safety and identical behavior on GNU
# and BSD/macOS find. The version filter is applied per-file in the loop with a
# plain `grep -qE`; we deliberately avoid `grep -Z` here because BSD/macOS grep
# treats `-Z` as zgrep (decompress), not GNU's --null -- the same portable idiom
# used by check-internal-links.sh.
#
# `.llm` is intentionally NOT pruned: it is first-party agent guidance, and a
# dependency example there should drift-fail just like docs/ or README.md.
find_markdown_docs() {
    find . \
        \( -type d \( -name 'target' \
        -o -name 'third_party' \
        -o -name 'node_modules' \
        -o -name 'site' \
        -o -name '.git' \
        -o -name 'test-fixtures' \
        -o -name 'mutants.out' \) \) -prune \
        -o -type f -name '*.md' -print0
}

if [ -n "$CARGO_VERSION" ]; then
    # Canonical usage doc must exist and carry a version example.
    if [ ! -f "$CANONICAL_USAGE_DOC" ]; then
        action_error "Missing required file: $CANONICAL_USAGE_DOC"
    elif ! grep -qE "$SFS_DEP_PATTERN" "$CANONICAL_USAGE_DOC"; then
        action_error "$CANONICAL_USAGE_DOC must contain a signal-fish-server dependency example"
    fi

    # Superset scan: validate the pinned version in EVERY doc that quotes it.
    canonical_seen=0
    while IFS= read -r -d '' doc; do
        grep -qE "$SFS_DEP_PATTERN" "$doc" || continue
        [ "${doc#./}" = "$CANONICAL_USAGE_DOC" ] && canonical_seen=1
        validate_signal_fish_dependency_versions "$doc"
    done < <(find_markdown_docs)

    # Discovery sanity: if the canonical doc quotes a version but the scan missed
    # it, filesystem discovery is broken -- fail loud rather than pass vacuously
    # (mirrors the REQUIRED_NESTED_LOCK assertion in the lockfile guard).
    if grep -qE "$SFS_DEP_PATTERN" "$CANONICAL_USAGE_DOC" 2>/dev/null \
        && [ "$canonical_seen" -eq 0 ]; then
        action_error "version scan did not discover $CANONICAL_USAGE_DOC despite its dependency example; doc discovery is broken"
    fi

    if [ ! -f ".llm/context.md" ]; then
        action_error "Missing required file: .llm/context.md"
    else
        expected_context_line="- **Version:** $CARGO_VERSION"
        context_without_cr=$(tr -d '\r' < .llm/context.md)
        if grep -Fqx -- "$expected_context_line" <<< "$context_without_cr"; then
            action_ok ".llm/context.md version line matches Cargo.toml"
        else
            action_error ".llm/context.md must contain exact line: $expected_context_line"
            VERSION_DRIFT=1
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

    # Read once with carriage returns stripped, then match against this content
    # instead of the raw file. `$`-anchored patterns (e.g. '^## \[Unreleased\]$')
    # would otherwise fail to match a CRLF-checked-out CHANGELOG (Windows/WSL2
    # under `* text=auto`), and we must not depend on a particular grep build's
    # platform-specific CRLF handling. Mirrors the .llm/context.md check above.
    local changelog
    changelog="$(tr -d '\r' < "$file")"

    if ! grep -q "Keep a Changelog" <<< "$changelog"; then
        action_error "CHANGELOG.md must reference Keep a Changelog"
    fi

    local unreleased_count
    unreleased_count=$(grep -c '^## \[Unreleased\]$' <<< "$changelog" || true)
    if [ "$unreleased_count" -ne 1 ]; then
        action_error "CHANGELOG.md must contain an exact '## [Unreleased]' heading exactly once (found: $unreleased_count)"
    fi

    if grep -q '^## Unreleased$' <<< "$changelog"; then
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
    done <<< "$changelog"

    # Disallow undated current-version headers.
    if [ -n "$CARGO_VERSION" ]; then
        if grep -q "^## \[$CARGO_VERSION\]$" <<< "$changelog"; then
            action_error "CHANGELOG.md has undated current-version header '## [$CARGO_VERSION]'. Use '## [$CARGO_VERSION] - YYYY-MM-DD' only at release cutover."
        fi

        if grep -q "^## \[$CARGO_VERSION\][[:space:]]*-[[:space:]]*" <<< "$changelog"; then
            # Dated is acceptable.
            :
        elif grep -q "^## \[$CARGO_VERSION\]" <<< "$changelog"; then
            action_error "CHANGELOG.md has malformed current-version header for $CARGO_VERSION"
        fi
    fi

    local first_bracketed_heading
    first_bracketed_heading=$(grep -E '^## \[' <<< "$changelog" | head -n 1 || true)
    if [ "$first_bracketed_heading" != '## [Unreleased]' ]; then
        action_error "CHANGELOG.md [Unreleased] must be the first bracketed section"
    fi

    is_real_changelog_date() {
        local value="$1"
        local year month day max_day
        if [[ ! "$value" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})$ ]]; then
            return 1
        fi
        year=$((10#${BASH_REMATCH[1]}))
        month=$((10#${BASH_REMATCH[2]}))
        day=$((10#${BASH_REMATCH[3]}))
        if ((year < 1 || month < 1 || month > 12 || day < 1)); then
            return 1
        fi
        case "$month" in
            2)
                max_day=28
                if ((year % 400 == 0 || (year % 4 == 0 && year % 100 != 0))); then
                    max_day=29
                fi
                ;;
            4|6|9|11) max_day=30 ;;
            *) max_day=31 ;;
        esac
        ((day <= max_day))
    }

    semver_is_greater() {
        local lhs="$1"
        local rhs="$2"
        local lhs_major lhs_minor lhs_patch rhs_major rhs_minor rhs_patch
        IFS=. read -r lhs_major lhs_minor lhs_patch <<< "$lhs"
        IFS=. read -r rhs_major rhs_minor rhs_patch <<< "$rhs"
        if ((10#$lhs_major != 10#$rhs_major)); then
            ((10#$lhs_major > 10#$rhs_major))
        elif ((10#$lhs_minor != 10#$rhs_minor)); then
            ((10#$lhs_minor > 10#$rhs_minor))
        else
            ((10#$lhs_patch > 10#$rhs_patch))
        fi
    }

    # Parse every bracketed level-two heading, not just valid ones, so malformed
    # releases cannot disappear from the comparison chain.
    local -a versions=()
    local heading version release_date seen_version
    local release_heading_pattern='^## \[((0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))\] - ([0-9]{4}-[0-9]{2}-[0-9]{2})$'
    while IFS= read -r heading; do
        [ -z "$heading" ] && continue
        if [ "$heading" = '## [Unreleased]' ]; then
            continue
        fi
        if [[ "$heading" =~ $release_heading_pattern ]]; then
            version="${BASH_REMATCH[1]}"
            release_date="${BASH_REMATCH[5]}"
        else
            action_error "CHANGELOG.md has malformed release heading: '$heading'"
            continue
        fi

        if ! is_real_changelog_date "$release_date"; then
            action_error "CHANGELOG.md release [$version] has invalid calendar date $release_date"
        fi
        # Bash 3.2 (the macOS system Bash) reports a declared-but-empty array as
        # unbound under `set -u`. Do not expand the array before its first item.
        if [ "${#versions[@]}" -gt 0 ]; then
            for seen_version in "${versions[@]}"; do
                if [ "$seen_version" = "$version" ]; then
                    action_error "CHANGELOG.md has duplicate dated release section for $version"
                fi
            done
        fi
        versions+=("$version")
    done < <(grep -E '^## \[' <<< "$changelog" || true)

    if [ "${#versions[@]}" -eq 0 ]; then
        action_warn "No dated release sections found in CHANGELOG.md"
        return
    fi

    local index
    for ((index = 1; index < ${#versions[@]}; index++)); do
        if ! semver_is_greater "${versions[index - 1]}" "${versions[index]}"; then
            action_error "CHANGELOG.md release sections must be in strictly descending semantic-version order (${versions[index - 1]} before ${versions[index]})"
        fi
    done

    local latest_version="${versions[0]}"

    local unreleased_ref
    unreleased_ref=$(grep -E '^\[Unreleased\]:' <<< "$changelog" | sed -E 's/^\[Unreleased\]:[[:space:]]*(\S+).*$/\1/' || true)
    if [ -z "$unreleased_ref" ]; then
        action_error "CHANGELOG.md must define a [Unreleased]: link reference"
    else
        if [[ ! "$unreleased_ref" =~ /compare/v${latest_version}\.\.\.HEAD([#?].*)?$ ]]; then
            action_error "[Unreleased] link must compare from v${latest_version}...HEAD (found: $unreleased_ref)"
        fi
    fi

    # Validate a single, contiguous comparison chain using repository files
    # alone. Checkout depth and fetched tags must never alter this result.
    local release_ref release_ref_lines release_ref_count previous_version linked_previous
    for ((index = 0; index < ${#versions[@]}; index++)); do
        version="${versions[index]}"
        release_ref_lines=$(grep -E "^\[$version\]:" <<< "$changelog" || true)
        release_ref_count=$(awk 'NF { count++ } END { print count + 0 }' <<< "$release_ref_lines")
        if [ "$release_ref_count" -ne 1 ]; then
            action_error "CHANGELOG.md must define exactly one [$version]: link reference (found: $release_ref_count)"
            action_error "CHANGELOG.md must define a [$version]: link reference"
            continue
        fi
        release_ref=$(sed -E "s/^\[$version\]:[[:space:]]*(\\S+).*$/\\1/" <<< "$release_ref_lines")

        if [ "$index" -eq "$((${#versions[@]} - 1))" ]; then
            if [[ ! "$release_ref" =~ /releases/tag/v${version}([#?].*)?$ ]]; then
                action_error "CHANGELOG.md oldest release [$version] must link directly to /releases/tag/v$version (found: $release_ref)"
            fi
            continue
        fi

        previous_version="${versions[index + 1]}"
        if [[ "$release_ref" =~ /compare/v([0-9]+\.[0-9]+\.[0-9]+)\.\.\.v${version}([#?].*)?$ ]]; then
            linked_previous="${BASH_REMATCH[1]}"
            if [[ " ${versions[*]} " != *" $linked_previous "* ]]; then
                action_error "[$version] compare link references unknown previous version v$linked_previous (link: $release_ref)"
            fi
        fi
        if [[ ! "$release_ref" =~ /compare/v${previous_version}\.\.\.v${version}([#?].*)?$ ]]; then
            action_error "[$version] link must compare adjacent releases v$previous_version...v$version (found: $release_ref)"
        fi
    done
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
        action_error "$markdown_file must reference canonical protocol sample: $sample_reference (derived from PROTOCOL_SAMPLE_FILES in scripts/check-doc-consistency.sh)"
    fi
}

validate_rust_client_game_data_formats() {
    local file="docs/guides/rust-client.md"
    if [ ! -f "$file" ]; then
        action_error "Missing required file: $file"
        return
    fi

    local enum_count
    enum_count=$(grep -c 'pub enum GameDataEncoding' "$file" || true)
    if [ "$enum_count" -eq 0 ]; then
        action_error "$file must define GameDataEncoding in its Rust samples"
        return
    fi

    local diagnostics
    diagnostics=$(awk '
        /pub enum GameDataEncoding[[:space:]]*\{/ {
            in_enum = 1
            depth = 1
            block += 1
            has_json = 0
            has_message_pack = 0
            has_rkyv = 0
            next
        }
        in_enum {
            if ($0 ~ /^[[:space:]]*[A-Z][A-Za-z0-9_]*[[:space:],]*$/) {
                line = $0
                sub(/^[[:space:]]*/, "", line)
                sub(/[[:space:],]*$/, "", line)
                if (line == "Json") {
                    has_json = 1
                } else if (line == "MessagePack") {
                    has_message_pack = 1
                } else if (line == "Rkyv") {
                    has_rkyv = 1
                }
            }

            text = $0
            opens = gsub(/\{/, "{", text)
            text = $0
            closes = gsub(/\}/, "}", text)
            depth += opens - closes
            if (depth <= 0) {
                if (has_rkyv) {
                    print "GameDataEncoding sample " block " must not list Rkyv; ProtocolInfo.game_data_formats only advertises json and optional message_pack"
                }
                if (!has_json) {
                    print "GameDataEncoding sample " block " must include Json"
                }
                if (!has_message_pack) {
                    print "GameDataEncoding sample " block " must include MessagePack"
                }
                in_enum = 0
            }
        }
    ' "$file")

    while IFS= read -r diagnostic; do
        if [ -n "$diagnostic" ]; then
            action_error "$file $diagnostic"
        fi
    done <<< "$diagnostics"
}

PROTOCOL_SAMPLE_CLIENT=".llm/code-samples/protocol/v2-client-messages.jsonl"
PROTOCOL_SAMPLE_SERVER=".llm/code-samples/protocol/v2-server-messages.jsonl"
PROTOCOL_SAMPLE_FILES=(
    "$PROTOCOL_SAMPLE_CLIENT"
    "$PROTOCOL_SAMPLE_SERVER"
)

validate_protocol_stale_tokens "README.md"
validate_protocol_stale_tokens ".llm/context.md"

for sample_file in "${PROTOCOL_SAMPLE_FILES[@]}"; do
    validate_protocol_stale_tokens "$sample_file"
done

validate_protocol_authenticated_payload_shape "$PROTOCOL_SAMPLE_SERVER"
validate_rust_client_game_data_formats

for sample_file in "${PROTOCOL_SAMPLE_FILES[@]}"; do
    validate_protocol_sample_reference "README.md" "$sample_file"
    validate_protocol_sample_reference ".llm/context.md" "${sample_file#.llm/}"
done

# ---------------------------------------------------------------------------
# 4) Changelog-required gate for non-internal changed files
# ---------------------------------------------------------------------------

# Internal path patterns for changelog gate: paths that never require a CHANGELOG entry.
# The dep-detect step in .github/workflows/ci.yml uses a superset of these patterns
# (adding Cargo.toml and CHANGELOG.md) to skip changelog checks for dependency bumps.
# Cargo.lock is internal here (lockfile-only changes need no changelog), while
# Cargo.toml and CHANGELOG.md are non-internal (they warrant a changelog entry).
is_internal_path() {
    local path="$1"
    case "$path" in
        .github/*|.githooks/*|.devcontainer/*|.config/*|.vscode/*|.claude/*)
            return 0
            ;;
        scripts/*|tests/*|test-fixtures/*|.llm/*|target/*|progress/*)
            return 0
            ;;
        src/*_tests.rs|src/*_test.rs|src/*/tests.rs)
            return 0
            ;;
        docs/ci-cd-*|docs/test-*|docs/git-hooks-*|docs/hooks-*|docs/pre-commit-*|docs/development.md)
            return 0
            ;;
        Cargo.lock|PLAN.md|AGENTS.md|CLAUDE.md|pre-push.txt|pre-commit.txt|logs_*.zip)
            return 0
            ;;
        .markdownlint*|.lychee.toml|.lycheecache|.typos.toml|.yamllint.yml)
            return 0
            ;;
        .gitignore|.dockerignore)
            return 0
            ;;
        clippy.toml|deny.toml|tarpaulin.toml|rust-toolchain.toml|mkdocs.yml|requirements-docs.txt)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

collect_changed_files() {
    if [ "$CHANGED_MODE" = "staged" ]; then
        CHANGED_FILES=()
        while IFS= read -r changed_file; do
            CHANGED_FILES+=("$changed_file")
        done < <(git diff --cached --name-only --diff-filter=ACMRTUXB)
    fi
}

collect_changed_files

if [ "$SKIP_CHANGELOG_GATE" -eq 1 ]; then
    action_info "Changelog gate skipped (--skip-changelog-gate)"
elif [ "$CHANGED_MODE" != "none" ]; then
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
            # Format note: "  - path  (reason)" prefix is used by
            # tests/doc_consistency_script_tests.rs must_not_contain assertions
            # to distinguish error-listed files from diagnostic help text.
            echo "  - $path  (not matched by is_internal_path)"
        done
        echo ""
        echo "See is_internal_path() in scripts/check-doc-consistency.sh for internal path patterns."
        echo ""
        echo "Add a Keep a Changelog entry under '## [Unreleased]' for user-facing impact,"
        echo "or add the path to is_internal_path() in scripts/check-doc-consistency.sh if truly internal."
    elif [ "${#NON_INTERNAL_CHANGED[@]}" -gt 0 ]; then
        action_ok "CHANGELOG.md updated alongside non-internal changes"
    else
        action_ok "No non-internal changed files detected for changelog gate"
    fi
fi

echo
if [ "$ERRORS" -gt 0 ]; then
    if [ "$VERSION_DRIFT" -ne 0 ]; then
        action_info "Crate version drift detected. Cargo.toml [package].version is the single source of truth."
        action_info "The Signal Fish pre-commit hook auto-syncs these docs on commit; to fix the working tree now run:"
        action_info "  pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree"
    fi
    action_error "Doc consistency checks failed with $ERRORS error(s) and $WARNINGS warning(s)"
    exit 1
fi

if [ "$WARNINGS" -gt 0 ]; then
    action_warn "Doc consistency checks passed with $WARNINGS warning(s)"
else
    action_ok "Doc consistency checks passed"
fi

exit 0

#!/usr/bin/env bash
# Prepare one Signal Fish Server release commit from the current source tree.
#
# The script is intentionally deterministic and side-effect free outside the
# checked-out worktree: it updates the root package version, every tracked
# lockfile that embeds that path package, synchronized public version references,
# and the Keep a Changelog release boundary. Publishing remains release.yml's job.

set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: scripts/prepare-release.sh --bump <major|minor|patch> [--date YYYY-MM-DD]

Options:
  --bump  Required semantic-version component to increment.
  --date  UTC release date. Defaults to today's UTC date.
USAGE
}

BUMP=""
RELEASE_DATE=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bump)
            [ "$#" -ge 2 ] || {
                echo "ERROR: --bump requires a value." >&2
                usage >&2
                exit 2
            }
            BUMP="$2"
            shift 2
            ;;
        --date)
            [ "$#" -ge 2 ] || {
                echo "ERROR: --date requires a value." >&2
                usage >&2
                exit 2
            }
            RELEASE_DATE="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$BUMP" in
    major|minor|patch) ;;
    *)
        echo "ERROR: --bump must be one of: major, minor, patch." >&2
        exit 2
        ;;
esac

if [ -z "$RELEASE_DATE" ]; then
    RELEASE_DATE=$(date -u +%F)
fi

# Validate Gregorian calendar dates in Bash instead of relying on GNU `date -d`,
# which is unavailable on the macOS runners used by the required CI matrix.
is_real_calendar_date() {
    local candidate="$1"
    local year month day max_day

    [[ "$candidate" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})$ ]] || return 1
    year=$((10#${BASH_REMATCH[1]}))
    month=$((10#${BASH_REMATCH[2]}))
    day=$((10#${BASH_REMATCH[3]}))

    ((year >= 1 && month >= 1 && month <= 12 && day >= 1)) || return 1
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

if ! is_real_calendar_date "$RELEASE_DATE"; then
    echo "ERROR: --date must be a real calendar date in YYYY-MM-DD form." >&2
    exit 2
fi

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "ERROR: prepare-release.sh must run inside a Git worktree." >&2
    exit 1
}
cd "$REPO_ROOT"

for required in \
    Cargo.toml Cargo.lock CHANGELOG.md .llm/context.md docs/library-usage.md \
    clients/native/Cargo.lock scripts/check-doc-consistency.sh; do
    if [ ! -f "$required" ]; then
        echo "ERROR: Required release file is missing: $required" >&2
        exit 1
    fi
done

read_package_version() {
    awk '
        BEGIN { in_package = 0 }
        /^\[package\][[:space:]]*$/ { in_package = 1; next }
        /^\[[^]]+\][[:space:]]*$/ { if (in_package == 1) in_package = 0 }
        in_package == 1 && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
            line = $0
            sub(/^[[:space:]]*version[[:space:]]*=[[:space:]]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            exit
        }
    ' Cargo.toml
}

CURRENT_VERSION=$(read_package_version)
if [[ ! "$CURRENT_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "ERROR: Cargo.toml [package].version is not strict X.Y.Z semver: $CURRENT_VERSION" >&2
    exit 1
fi

IFS=. read -r CURRENT_MAJOR CURRENT_MINOR CURRENT_PATCH <<< "$CURRENT_VERSION"

# Bash arithmetic is signed 64-bit on the supported runners. Fail closed before
# incrementing instead of wrapping a syntactically valid (but extreme) semver.
increment_component() {
    local value="$1"
    local max_incrementable="9223372036854775806"
    # Equal-width decimal strings are intentionally compared lexicographically.
    # shellcheck disable=SC2071
    if [ "${#value}" -gt "${#max_incrementable}" ] \
        || { [ "${#value}" -eq "${#max_incrementable}" ] && [[ "$value" > "$max_incrementable" ]]; }; then
        echo "ERROR: Cannot safely increment semver component '$value'." >&2
        exit 1
    fi
    printf '%s\n' "$((value + 1))"
}

case "$BUMP" in
    major)
        NEXT_VERSION="$(increment_component "$CURRENT_MAJOR").0.0"
        ;;
    minor)
        NEXT_VERSION="$CURRENT_MAJOR.$(increment_component "$CURRENT_MINOR").0"
        ;;
    patch)
        NEXT_VERSION="$CURRENT_MAJOR.$CURRENT_MINOR.$(increment_component "$CURRENT_PATCH")"
        ;;
esac

if grep -Eq "^## \\[${NEXT_VERSION//./\\.}\\]([[:space:]]|$)" CHANGELOG.md; then
    echo "ERROR: CHANGELOG.md already contains a release section for $NEXT_VERSION." >&2
    exit 1
fi

LATEST_CHANGELOG_VERSION=$(grep -E '^## \[(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$' CHANGELOG.md \
    | head -n 1 \
    | sed -E 's/^## \[([^]]+)\].*/\1/' || true)
if [ -z "$LATEST_CHANGELOG_VERSION" ] || [ "$LATEST_CHANGELOG_VERSION" != "$CURRENT_VERSION" ]; then
    echo "ERROR: Cargo.toml version $CURRENT_VERSION must equal the latest dated CHANGELOG.md release (found: ${LATEST_CHANGELOG_VERSION:-none})." >&2
    exit 1
fi

CURRENT_TAG="refs/tags/v$CURRENT_VERSION"
if ! git rev-parse --verify --quiet "$CURRENT_TAG" >/dev/null; then
    echo "ERROR: Baseline tag v$CURRENT_VERSION is missing." >&2
    exit 1
fi
if [ "$(git cat-file -t "$CURRENT_TAG")" != "tag" ]; then
    echo "ERROR: Baseline tag v$CURRENT_VERSION must be annotated." >&2
    exit 1
fi
CURRENT_TAG_COMMIT=$(git rev-parse "$CURRENT_TAG^{commit}")
if ! git merge-base --is-ancestor "$CURRENT_TAG_COMMIT" HEAD; then
    echo "ERROR: Baseline tag v$CURRENT_VERSION must resolve to an ancestor of HEAD." >&2
    exit 1
fi
if git rev-parse --verify --quiet "refs/tags/v$NEXT_VERSION" >/dev/null; then
    echo "ERROR: Target tag v$NEXT_VERSION already exists locally." >&2
    exit 1
fi

# These two overrides are narrow test seams. Production and the workflow use
# the real commands, while isolated fixture tests can validate transformations
# without resolving the repository's dependency graph.
PREPARE_RELEASE_CARGO_BIN=${PREPARE_RELEASE_CARGO_BIN:-cargo}
PREPARE_RELEASE_DOC_CHECK=${PREPARE_RELEASE_DOC_CHECK:-scripts/check-doc-consistency.sh}

# Validate the released baseline before touching any of the six output files.
# The documentation checker is deliberately file-only: fetched tags and clone
# depth cannot change whether the comparison chain is valid.
for lockfile in Cargo.lock clients/native/Cargo.lock; do
    lock_entry_state=$(awk -v current_version="$CURRENT_VERSION" '
        BEGIN { target = 0; entries = 0; matching = 0 }
        /^\[\[package\]\]$/ { target = 0 }
        /^name = "signal-fish-server"$/ { target = 1 }
        target == 1 && /^version = "/ {
            entries++
            if ($0 == "version = \"" current_version "\"") matching++
            target = 0
        }
        END { print entries ":" matching }
    ' "$lockfile")
    if [ "$lock_entry_state" != "1:1" ]; then
        echo "ERROR: Expected exactly one signal-fish-server package entry at version $CURRENT_VERSION in $lockfile." >&2
        exit 1
    fi
done
"$PREPARE_RELEASE_DOC_CHECK" --skip-changelog-gate
for manifest in Cargo.toml clients/native/Cargo.toml; do
    "$PREPARE_RELEASE_CARGO_BIN" metadata --locked --no-deps --format-version 1 \
        --manifest-path "$manifest" >/dev/null
done

UNRELEASED_BODY=$(awk '
    /^## \[Unreleased\]$/ { in_unreleased = 1; next }
    in_unreleased && /^## \[/ { exit }
    in_unreleased { print }
' CHANGELOG.md)
if ! grep -Eq '^### (Added|Changed|Deprecated|Removed|Fixed|Security)$' <<< "$UNRELEASED_BODY" \
    || ! grep -Eq '^[[:space:]]*[-*][[:space:]]+' <<< "$UNRELEASED_BODY"; then
    echo "ERROR: CHANGELOG.md [Unreleased] must contain categorized release notes before preparation." >&2
    exit 1
fi

replace_root_package_version() {
    local file="$1"
    local next_version="$2"
    local output count_file
    output=$(mktemp)
    count_file=$(mktemp)
    awk -v next_version="$next_version" -v count_file="$count_file" '
        BEGIN { in_package = 0; changed = 0 }
        /^\[package\][[:space:]]*$/ { in_package = 1; print; next }
        /^\[[^]]+\][[:space:]]*$/ { if (in_package == 1) in_package = 0 }
        in_package == 1 && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
            indent = $0
            sub(/[^[:space:]].*$/, "", indent)
            print indent "version = \"" next_version "\""
            changed++
            next
        }
        { print }
        END { print changed > count_file }
    ' "$file" > "$output"
    if [ "$(cat "$count_file")" -ne 1 ]; then
        echo "ERROR: Expected exactly one [package].version in $file." >&2
        rm -f "$output" "$count_file"
        exit 1
    fi
    mv "$output" "$file"
    rm -f "$count_file"
}

replace_locked_path_package_version() {
    local file="$1"
    local next_version="$2"
    local output count_file
    output=$(mktemp)
    count_file=$(mktemp)
    awk -v next_version="$next_version" -v count_file="$count_file" '
        BEGIN { target = 0; changed = 0 }
        /^\[\[package\]\]$/ { target = 0 }
        /^name = "signal-fish-server"$/ { target = 1 }
        target == 1 && /^version = "/ {
            print "version = \"" next_version "\""
            changed++
            target = 0
            next
        }
        { print }
        END { print changed > count_file }
    ' "$file" > "$output"
    if [ "$(cat "$count_file")" -ne 1 ]; then
        echo "ERROR: Expected exactly one signal-fish-server package entry in $file." >&2
        rm -f "$output" "$count_file"
        exit 1
    fi
    mv "$output" "$file"
    rm -f "$count_file"
}

replace_documented_version() {
    local file="$1"
    local current_version="$2"
    local next_version="$3"
    local output count_file
    output=$(mktemp)
    count_file=$(mktemp)
    awk -v current_version="$current_version" -v next_version="$next_version" -v count_file="$count_file" '
        BEGIN { changed = 0 }
        function replace_all(text, needle, replacement,    result, position) {
            result = ""
            replacements = 0
            while ((position = index(text, needle)) != 0) {
                result = result substr(text, 1, position - 1) replacement
                text = substr(text, position + length(needle))
                replacements++
            }
            return result text
        }
        /signal-fish-server[[:space:]]*=[[:space:]]*["{]/ {
            $0 = replace_all($0, current_version, next_version)
            changed += replacements
        }
        { print }
        END { print changed > count_file }
    ' "$file" > "$output"
    if [ "$(cat "$count_file")" -lt 1 ]; then
        echo "ERROR: No $current_version dependency examples were updated in $file." >&2
        rm -f "$output" "$count_file"
        exit 1
    fi
    mv "$output" "$file"
    rm -f "$count_file"
}

replace_context_version() {
    local file="$1"
    local current_version="$2"
    local next_version="$3"
    local output count_file
    output=$(mktemp)
    count_file=$(mktemp)
    awk -v current="- **Version:** $current_version" -v replacement="- **Version:** $next_version" -v count_file="$count_file" '
        BEGIN { changed = 0 }
        $0 == current { print replacement; changed++; next }
        { print }
        END { print changed > count_file }
    ' "$file" > "$output"
    if [ "$(cat "$count_file")" -ne 1 ]; then
        echo "ERROR: Expected one exact project version line in $file." >&2
        rm -f "$output" "$count_file"
        exit 1
    fi
    mv "$output" "$file"
    rm -f "$count_file"
}

cut_changelog_release() {
    local file="$1"
    local next_version="$2"
    local release_date="$3"
    local latest_release="$4"
    local output count_file
    output=$(mktemp)
    count_file=$(mktemp)
    awk \
        -v next_version="$next_version" \
        -v release_date="$release_date" \
        -v latest_release="$latest_release" \
        -v count_file="$count_file" '
        BEGIN { heading = 0; link = 0 }
        /^## \[Unreleased\]$/ {
            print
            print ""
            print "## [" next_version "] - " release_date
            heading++
            next
        }
        /^\[Unreleased\]:[[:space:]]*/ {
            print "[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v" next_version "...HEAD"
            print "[" next_version "]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v" latest_release "...v" next_version
            link++
            next
        }
        { print }
        END { print heading ":" link > count_file }
    ' "$file" > "$output"
    if [ "$(cat "$count_file")" != "1:1" ]; then
        echo "ERROR: Expected one [Unreleased] heading and link in $file." >&2
        rm -f "$output" "$count_file"
        exit 1
    fi
    mv "$output" "$file"
    rm -f "$count_file"
}

replace_root_package_version Cargo.toml "$NEXT_VERSION"
for lockfile in Cargo.lock clients/native/Cargo.lock; do
    replace_locked_path_package_version "$lockfile" "$NEXT_VERSION"
done
replace_documented_version docs/library-usage.md "$CURRENT_VERSION" "$NEXT_VERSION"
replace_context_version .llm/context.md "$CURRENT_VERSION" "$NEXT_VERSION"
cut_changelog_release CHANGELOG.md "$NEXT_VERSION" "$RELEASE_DATE" "$CURRENT_VERSION"

if [ "$(read_package_version)" != "$NEXT_VERSION" ]; then
    echo "ERROR: Cargo.toml version update did not persist." >&2
    exit 1
fi

for manifest in Cargo.toml clients/native/Cargo.toml; do
    "$PREPARE_RELEASE_CARGO_BIN" metadata --locked --no-deps --format-version 1 \
        --manifest-path "$manifest" >/dev/null
done
"$PREPARE_RELEASE_DOC_CHECK" --changed-files \
    Cargo.toml Cargo.lock clients/native/Cargo.lock \
    CHANGELOG.md docs/library-usage.md .llm/context.md

printf 'Prepared Signal Fish Server %s -> %s (%s).\n' \
    "$CURRENT_VERSION" "$NEXT_VERSION" "$RELEASE_DATE"

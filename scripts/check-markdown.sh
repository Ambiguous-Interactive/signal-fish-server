#!/usr/bin/env bash
# Check tracked Markdown with the pinned rumdl binary.
# Pass fix to apply lint fixes. Exit 1 for lint failures and 2 for tooling errors.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ ! -r .rumdl-version ]]; then
    echo "Missing .rumdl-version." >&2
    exit 2
fi
required_version=$(<.rumdl-version)
if [[ ! "$required_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Invalid version in .rumdl-version." >&2
    exit 2
fi
if ! installed_version=$(rumdl --version 2>/dev/null) || [[ "$installed_version" != "rumdl $required_version" ]]; then
    echo "Expected rumdl $required_version on PATH. Found: ${installed_version:-unavailable}." >&2
    echo "Install it with cargo binstall --locked --disable-strategies quick-install,compile rumdl@$required_version." >&2
    exit 2
fi

lint_args=(check --config .rumdl.toml --deny-config-warnings --no-cache --output-format full)
link_args=()
case "${1:-}" in
    "") ;;
    fix)
        lint_args+=(--fix)
        link_args+=(--fix)
        ;;
    *)
        echo "Usage: bash scripts/check-markdown.sh [fix]" >&2
        exit 2
        ;;
esac

files=()
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    while IFS= read -r -d '' file; do
        case "$file" in
            target/*|third_party/*|node_modules/*|.github/test-fixtures/*|test-fixtures/*) ;;
            *) files+=("./$file") ;;
        esac
    done < <(git ls-files -z -- '*.md')
else
    files=(.)
    lint_args+=(--exclude 'target/**,third_party/**,node_modules/**,.github/test-fixtures/**,test-fixtures/**')
fi

if (( ${#files[@]} == 0 )); then
    echo "No Markdown files found to lint."
    exit 0
fi

if rumdl "${lint_args[@]}" "${files[@]}"; then
    bash scripts/check-markdown-link-text.sh "${link_args[@]}"
else
    status=$?
    if (( status == 1 )); then
        echo "Run bash scripts/check-markdown.sh fix to apply available lint fixes." >&2
    fi
    exit "$status"
fi

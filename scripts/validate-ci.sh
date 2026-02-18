#!/usr/bin/env bash
# Signal Fish Server - CI Configuration Validation Script
# https://github.com/Ambiguous-Interactive/signal-fish-server
#
# Data-driven validation script that catches common CI/CD configuration issues
# locally before they cause failures in GitHub Actions.
#
# Validates:
#   1. AWK files (.awk) parse correctly
#   2. Shell scripts pass shellcheck
#   3. Markdown relative links from docs/ to .llm/ use ../ prefix
#   4. GitHub Actions scripts (.github/scripts/) are valid
#
# Usage:
#   ./scripts/validate-ci.sh              # Run all validations
#   ./scripts/validate-ci.sh --awk        # AWK validation only
#   ./scripts/validate-ci.sh --shell      # Shell script validation only
#   ./scripts/validate-ci.sh --links      # Markdown link validation only
#   ./scripts/validate-ci.sh --quiet      # Suppress success messages
#
# Exit codes:
#   0 = All validations passed
#   1 = One or more validations failed
#   2 = Invalid usage

set -euo pipefail

# -----------------------------------------------------------------------
# Configuration
# -----------------------------------------------------------------------

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    BOLD=''
    NC=''
fi

# -----------------------------------------------------------------------
# Parse arguments
# -----------------------------------------------------------------------

RUN_AWK=true
RUN_SHELL=true
RUN_LINKS=true
QUIET=false

for arg in "$@"; do
    case "$arg" in
        --awk)
            RUN_AWK=true
            RUN_SHELL=false
            RUN_LINKS=false
            ;;
        --shell)
            RUN_AWK=false
            RUN_SHELL=true
            RUN_LINKS=false
            ;;
        --links)
            RUN_AWK=false
            RUN_SHELL=false
            RUN_LINKS=true
            ;;
        --quiet|-q)
            QUIET=true
            ;;
        --help|-h)
            echo "Usage: $0 [--awk] [--shell] [--links] [--quiet]"
            echo ""
            echo "Options:"
            echo "  --awk      Validate AWK files only"
            echo "  --shell    Validate shell scripts only"
            echo "  --links    Validate markdown links only"
            echo "  --quiet    Suppress success messages"
            echo "  --help     Show this help"
            echo ""
            echo "With no options, runs all validations."
            exit 0
            ;;
        *)
            echo "Unknown option: $arg"
            echo "Use --help for usage information"
            exit 2
            ;;
    esac
done

# -----------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------

ERRORS=0
WARNINGS=0
CHECKS_PASSED=0
CHECKS_RUN=0

info() {
    if [ "$QUIET" = false ]; then
        printf '%b[INFO]%b  %s\n' "$BLUE" "$NC" "$1"
    fi
}

success() {
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
    if [ "$QUIET" = false ]; then
        printf '%b[PASS]%b  %s\n' "$GREEN" "$NC" "$1"
    fi
}

warn() {
    WARNINGS=$((WARNINGS + 1))
    printf '%b[WARN]%b  %s\n' "$YELLOW" "$NC" "$1"
}

fail() {
    ERRORS=$((ERRORS + 1))
    printf '%b[FAIL]%b  %s\n' "$RED" "$NC" "$1"
}

# -----------------------------------------------------------------------
# 1. AWK file validation
# -----------------------------------------------------------------------

validate_awk_files() {
    CHECKS_RUN=$((CHECKS_RUN + 1))
    info "Validating AWK files..."

    local awk_files_found=0
    local awk_errors=0

    # Find all .awk files in the repository
    while IFS= read -r -d '' awk_file; do
        awk_files_found=$((awk_files_found + 1))

        # Test 1: Verify AWK can parse the file (syntax check)
        if ! awk -f "$awk_file" < /dev/null > /dev/null 2>&1; then
            fail "AWK syntax error in $awk_file"
            # Show the error
            awk -f "$awk_file" < /dev/null 2>&1 || true
            awk_errors=$((awk_errors + 1))
        fi

        # Test 2: Check for non-POSIX patterns (skip comment lines starting with #)
        if grep -v '^\s*#' "$awk_file" | grep -n 'match(' > /dev/null 2>&1; then
            warn "$awk_file uses match() -- not POSIX compatible (mawk)"
        fi

        # Test 3: Check for \0 in printf (not POSIX, skip comment lines)
        if grep -v '^\s*#' "$awk_file" | grep -nE 'printf.*".*\\0' > /dev/null 2>&1; then
            warn "$awk_file uses \\\\0 in printf -- use printf \"%c\", 0 instead"
        fi

    done < <(find . -type f -name "*.awk" \
        -not -path "./target/*" \
        -not -path "./.git/*" \
        -not -path "./third_party/*" \
        -print0)

    if [ "$awk_files_found" -eq 0 ]; then
        info "No .awk files found"
        return
    fi

    if [ "$awk_errors" -eq 0 ]; then
        success "All $awk_files_found AWK file(s) pass syntax validation"
    else
        fail "$awk_errors of $awk_files_found AWK file(s) have syntax errors"
    fi
}

# -----------------------------------------------------------------------
# 2. Shell script validation (shellcheck)
# -----------------------------------------------------------------------

validate_shell_scripts() {
    CHECKS_RUN=$((CHECKS_RUN + 1))
    info "Validating shell scripts with shellcheck..."

    if ! command -v shellcheck > /dev/null 2>&1; then
        warn "shellcheck not installed -- skipping shell validation"
        warn "Install with: apt-get install shellcheck  OR  brew install shellcheck"
        return
    fi

    local shell_errors=0
    local shell_checked=0

    # Use --severity=warning to catch errors and warnings, but not style/info
    # (style suggestions like SC2126 are informational, not correctness issues)
    local sc_severity="--severity=warning"

    # Validate scripts in scripts/ directory
    for script in scripts/*.sh; do
        [ -f "$script" ] || continue
        shell_checked=$((shell_checked + 1))

        if ! shellcheck -s bash $sc_severity "$script" > /dev/null 2>&1; then
            fail "shellcheck errors in $script"
            shellcheck -s bash $sc_severity "$script" 2>&1 | head -20
            echo ""
            shell_errors=$((shell_errors + 1))
        fi
    done

    # Validate scripts in .github/scripts/ directory
    for script in .github/scripts/*.sh; do
        [ -f "$script" ] || continue
        shell_checked=$((shell_checked + 1))

        if ! shellcheck -s bash $sc_severity "$script" > /dev/null 2>&1; then
            fail "shellcheck errors in $script"
            shellcheck -s bash $sc_severity "$script" 2>&1 | head -20
            echo ""
            shell_errors=$((shell_errors + 1))
        fi
    done

    # Validate the pre-commit hook (uses /bin/sh, not bash)
    if [ -f .githooks/pre-commit ]; then
        shell_checked=$((shell_checked + 1))
        # Detect the shell from the shebang
        local hook_shell="sh"
        if head -1 .githooks/pre-commit | grep -q 'bash'; then
            hook_shell="bash"
        fi

        if ! shellcheck -s "$hook_shell" $sc_severity .githooks/pre-commit > /dev/null 2>&1; then
            fail "shellcheck errors in .githooks/pre-commit"
            shellcheck -s "$hook_shell" $sc_severity .githooks/pre-commit 2>&1 | head -20
            echo ""
            shell_errors=$((shell_errors + 1))
        fi
    fi

    if [ "$shell_checked" -eq 0 ]; then
        info "No shell scripts found to check"
        return
    fi

    if [ "$shell_errors" -eq 0 ]; then
        success "All $shell_checked shell script(s) pass shellcheck"
    else
        fail "$shell_errors of $shell_checked shell script(s) have shellcheck errors"
    fi
}

# -----------------------------------------------------------------------
# 3. Markdown relative link validation
# -----------------------------------------------------------------------

validate_markdown_links() {
    CHECKS_RUN=$((CHECKS_RUN + 1))
    info "Validating markdown relative links in docs/..."

    local link_errors=0
    local links_checked=0
    local files_checked=0

    # Check all markdown files in docs/ directory
    for md_file in docs/*.md docs/**/*.md; do
        [ -f "$md_file" ] || continue
        files_checked=$((files_checked + 1))

        local base_dir
        base_dir=$(dirname "$md_file")

        # Extract relative links from markdown: [text](relative/path)
        # Skip external URLs (http/https), anchor-only (#), and empty links
        while IFS= read -r link_match; do
            [ -z "$link_match" ] && continue

            # Extract the URL portion from [text](url)
            local url
            url=$(echo "$link_match" | sed -E 's/.*\(([^)]+)\).*/\1/')

            # Skip external URLs and anchors
            case "$url" in
                http://*|https://*|mailto:*|\#*) continue ;;
            esac

            # Warn about machine-specific absolute paths (not portable)
            case "$url" in
                /workspaces/*|/home/*|/Users/*|/tmp/*)
                    warn "Non-portable absolute path in $md_file: [$url] -- use a relative path instead"
                    continue
                    ;;
                /*)
                    # Other absolute paths are skipped silently (e.g., root-relative paths)
                    continue
                    ;;
            esac

            # Strip anchor portion for file existence check
            local file_part="${url%%#*}"
            [ -z "$file_part" ] && continue

            links_checked=$((links_checked + 1))

            # Resolve the path relative to the markdown file's directory
            local resolved_path="$base_dir/$file_part"

            # Check if the resolved file exists
            if [ ! -f "$resolved_path" ] && [ ! -d "$resolved_path" ]; then
                fail "Broken link in $md_file: [$url] (resolves to $resolved_path)"
                link_errors=$((link_errors + 1))

                # Provide helpful fix suggestion for common .llm path issues
                if echo "$url" | grep -q '\.llm/' && ! echo "$url" | grep -q '^\.\./'; then
                    printf '  %bFix%b: Change "%s" to "../%s"\n' "$YELLOW" "$NC" "$url" "$url"
                fi
            fi

            # Special check: docs/ files linking to .llm/ must use ../ prefix
            if echo "$url" | grep -qE '^\.llm/'; then
                fail "Invalid relative link in $md_file: [$url] -- links from docs/ to .llm/ must use ../ prefix"
                printf '  %bFix%b: Change "%s" to "../%s"\n' "$YELLOW" "$NC" "$url" "$url"
                link_errors=$((link_errors + 1))
            fi
        done < <(grep -oE '\[[^]]*\]\([^)]+\)' "$md_file" 2>/dev/null || true)
    done

    if [ "$files_checked" -eq 0 ]; then
        info "No markdown files found in docs/"
        return
    fi

    if [ "$link_errors" -eq 0 ]; then
        success "All $links_checked link(s) in $files_checked docs file(s) are valid"
    else
        fail "$link_errors broken link(s) found in docs/ markdown files"
    fi
}

# -----------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------

echo -e "${BOLD}${BLUE}CI Configuration Validator${NC}"
echo "Repository: $REPO_ROOT"
echo ""

if [ "$RUN_AWK" = true ]; then
    validate_awk_files
    echo ""
fi

if [ "$RUN_SHELL" = true ]; then
    validate_shell_scripts
    echo ""
fi

if [ "$RUN_LINKS" = true ]; then
    validate_markdown_links
    echo ""
fi

# -----------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------

echo "=========================================="
if [ "$ERRORS" -gt 0 ]; then
    printf '%b%bFAILED%b: %d error(s), %d warning(s), %d passed (%d checks)\n' \
        "$BOLD" "$RED" "$NC" "$ERRORS" "$WARNINGS" "$CHECKS_PASSED" "$CHECKS_RUN"
    echo ""
    echo "Fix the errors above before pushing to CI."
    echo ""
    echo "Quick reference:"
    echo "  ./scripts/validate-ci.sh --awk      # Re-check AWK files"
    echo "  ./scripts/validate-ci.sh --shell    # Re-check shell scripts"
    echo "  ./scripts/validate-ci.sh --links    # Re-check markdown links"
    exit 1
elif [ "$WARNINGS" -gt 0 ]; then
    printf '%b%bPASSED with warnings%b: %d warning(s), %d passed (%d checks)\n' \
        "$BOLD" "$YELLOW" "$NC" "$WARNINGS" "$CHECKS_PASSED" "$CHECKS_RUN"
    exit 0
else
    printf '%b%bALL PASSED%b: %d check(s) passed\n' \
        "$BOLD" "$GREEN" "$NC" "$CHECKS_PASSED"
    exit 0
fi

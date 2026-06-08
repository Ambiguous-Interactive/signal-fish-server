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
#   5. CI/devcontainer tooling parity stays synchronized
#
# Usage:
#   ./scripts/validate-ci.sh              # Run all validations
#   ./scripts/validate-ci.sh --awk        # AWK validation only
#   ./scripts/validate-ci.sh --shell      # Shell script validation only
#   ./scripts/validate-ci.sh --links      # Markdown link validation only
#   ./scripts/validate-ci.sh --tools      # Tooling parity validation only
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
RUN_TOOLS=true
QUIET=false

for arg in "$@"; do
    case "$arg" in
        --awk)
            RUN_AWK=true
            RUN_SHELL=false
            RUN_LINKS=false
            RUN_TOOLS=false
            ;;
        --shell)
            RUN_AWK=false
            RUN_SHELL=true
            RUN_LINKS=false
            RUN_TOOLS=false
            ;;
        --links)
            RUN_AWK=false
            RUN_SHELL=false
            RUN_LINKS=true
            RUN_TOOLS=false
            ;;
        --tools)
            RUN_AWK=false
            RUN_SHELL=false
            RUN_LINKS=false
            RUN_TOOLS=true
            ;;
        --quiet|-q)
            QUIET=true
            ;;
        --help|-h)
            echo "Usage: $0 [--awk] [--shell] [--links] [--tools] [--quiet]"
            echo ""
            echo "Options:"
            echo "  --awk      Validate AWK files only"
            echo "  --shell    Validate shell scripts only"
            echo "  --links    Validate markdown links only"
            echo "  --tools    Validate CI/devcontainer tooling parity only"
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

        # Test 2: Check for non-POSIX match() capture arrays (skip comments)
        local match_capture_lines
        match_capture_lines=$(awk '
            function remember_line(line_number) {
                if (count < 3) {
                    if (lines != "") {
                        lines = lines ", "
                    }
                    lines = lines line_number
                }
                count++
            }
            function reset_match_call() {
                in_match_call = 0
                match_depth = 0
                match_commas = 0
                match_line = 0
                match_in_string = 0
                match_in_regex = 0
                match_escaped = 0
            }
            function last_significant_char(text, i, char) {
                for (i = length(text); i >= 1; i--) {
                    char = substr(text, i, 1)
                    if (char !~ /[[:space:]]/) {
                        return char
                    }
                }
                return ""
            }
            function last_significant_token(text, i, char, token) {
                token = ""
                for (i = length(text); i >= 1; i--) {
                    char = substr(text, i, 1)
                    if (char ~ /[[:alnum:]_]/) {
                        token = char token
                        continue
                    }
                    if (token != "") {
                        return token
                    }
                    if (char !~ /[[:space:]]/) {
                        return ""
                    }
                }
                return token
            }
            function starts_regex_literal(text, prev, token) {
                prev = last_significant_char(text)
                if (prev == "" || index("~(,=:{;![?+-*%&|", prev) > 0) {
                    return 1
                }
                token = last_significant_token(text)
                return token == "print" || token == "printf" || token == "return"
            }
            function is_identifier_char(char) {
                return char ~ /^[[:alnum:]_]$/
            }
            function is_match_token_at(line, pos, before, after) {
                if (substr(line, pos, 5) != "match") {
                    return 0
                }
                before = pos > 1 ? substr(line, pos - 1, 1) : ""
                after = pos + 5 <= length(line) ? substr(line, pos + 5, 1) : ""
                return !is_identifier_char(before) && !is_identifier_char(after)
            }
            function strip_awk_comment(line, i, char, out, in_string, in_regex, escaped) {
                out = ""
                for (i = 1; i <= length(line); i++) {
                    char = substr(line, i, 1)
                    if (escaped) {
                        out = out char
                        escaped = 0
                        continue
                    }
                    if (char == "\\") {
                        out = out char
                        escaped = 1
                        continue
                    }
                    if (in_string) {
                        if (char == "\"") {
                            in_string = 0
                        }
                        out = out char
                        continue
                    }
                    if (in_regex) {
                        if (char == "/") {
                            in_regex = 0
                        }
                        out = out char
                        continue
                    }
                    if (char == "\"") {
                        in_string = 1
                        out = out char
                        continue
                    }
                    if (char == "/" && starts_regex_literal(out)) {
                        in_regex = 1
                        out = out char
                        continue
                    }
                    if (char == "#") {
                        return out
                    }
                    out = out char
                }
                return out
            }
            function scan_match_calls(line, i, open_pos, char) {
                i = 1
                while (i <= length(line)) {
                    if (!in_match_call) {
                        char = substr(line, i, 1)
                        if (match_escaped) {
                            match_escaped = 0
                            i++
                            continue
                        }
                        if (char == "\\") {
                            match_escaped = 1
                            i++
                            continue
                        }
                        if (match_in_string) {
                            if (char == "\"") {
                                match_in_string = 0
                            }
                            i++
                            continue
                        }
                        if (match_in_regex) {
                            if (char == "/") {
                                match_in_regex = 0
                            }
                            i++
                            continue
                        }
                        if (char == "\"") {
                            match_in_string = 1
                            i++
                            continue
                        }
                        if (char == "/" && starts_regex_literal(substr(line, 1, i - 1))) {
                            match_in_regex = 1
                            i++
                            continue
                        }
                        if (!is_match_token_at(line, i)) {
                            i++
                            continue
                        }
                        open_pos = i + 5
                        while (open_pos <= length(line) && substr(line, open_pos, 1) ~ /[[:space:]]/) {
                            open_pos++
                        }
                        if (substr(line, open_pos, 1) != "(") {
                            i++
                            continue
                        }
                        in_match_call = 1
                        match_depth = 1
                        match_commas = 0
                        match_line = NR
                        match_in_string = 0
                        match_in_regex = 0
                        match_escaped = 0
                        i = open_pos + 1
                        continue
                    }

                    char = substr(line, i, 1)
                    if (match_escaped) {
                        match_escaped = 0
                        i++
                        continue
                    }
                    if (char == "\\") {
                        match_escaped = 1
                        i++
                        continue
                    }
                    if (match_in_string) {
                        if (char == "\"") {
                            match_in_string = 0
                        }
                        i++
                        continue
                    }
                    if (match_in_regex) {
                        if (char == "/") {
                            match_in_regex = 0
                        }
                        i++
                        continue
                    }
                    if (char == "\"") {
                        match_in_string = 1
                        i++
                        continue
                    }
                    if (char == "/" && starts_regex_literal(substr(line, 1, i - 1))) {
                        match_in_regex = 1
                        i++
                        continue
                    }
                    if (char == "(") {
                        match_depth++
                        i++
                        continue
                    }
                    if (char == ")") {
                        match_depth--
                        if (match_depth == 0) {
                            if (match_commas >= 2) {
                                remember_line(match_line)
                            }
                            reset_match_call()
                        }
                        i++
                        continue
                    }
                    if (char == "," && match_depth == 1) {
                        match_commas++
                    }
                    i++
                }
            }
            /^[[:space:]]*#/ { next }
            {
                code_line = strip_awk_comment($0)
                if (code_line !~ /^[[:space:]]*$/) {
                    scan_match_calls(code_line)
                }
            }
            END {
                if (lines != "") {
                    print lines
                }
            }
        ' "$awk_file")
        if [ -n "$match_capture_lines" ]; then
            warn "$awk_file uses match() capture array at line(s) $match_capture_lines -- not POSIX compatible (mawk)"
        fi

        # Test 3: Check for \0 in printf format strings (not POSIX)
        local nul_printf_lines
        nul_printf_lines=$(awk '
            function remember_line(line_number) {
                if (count < 3) {
                    if (lines != "") {
                        lines = lines ", "
                    }
                    lines = lines line_number
                }
                count++
            }
            function last_significant_char(text, i, char) {
                for (i = length(text); i >= 1; i--) {
                    char = substr(text, i, 1)
                    if (char !~ /[[:space:]]/) {
                        return char
                    }
                }
                return ""
            }
            function last_significant_token(text, i, char, token) {
                token = ""
                for (i = length(text); i >= 1; i--) {
                    char = substr(text, i, 1)
                    if (char ~ /[[:alnum:]_]/) {
                        token = char token
                        continue
                    }
                    if (token != "") {
                        return token
                    }
                    if (char !~ /[[:space:]]/) {
                        return ""
                    }
                }
                return token
            }
            function starts_regex_literal(text, prev, token) {
                prev = last_significant_char(text)
                if (prev == "" || index("~(,=:{;![?+-*%&|", prev) > 0) {
                    return 1
                }
                token = last_significant_token(text)
                return token == "print" || token == "printf" || token == "return"
            }
            function is_identifier_char(char) {
                return char ~ /^[[:alnum:]_]$/
            }
            function strip_awk_comment(line, i, char, out, in_string, in_regex, escaped) {
                out = ""
                for (i = 1; i <= length(line); i++) {
                    char = substr(line, i, 1)
                    if (escaped) {
                        out = out char
                        escaped = 0
                        continue
                    }
                    if (char == "\\") {
                        out = out char
                        escaped = 1
                        continue
                    }
                    if (in_string) {
                        if (char == "\"") {
                            in_string = 0
                        }
                        out = out char
                        continue
                    }
                    if (in_regex) {
                        if (char == "/") {
                            in_regex = 0
                        }
                        out = out char
                        continue
                    }
                    if (char == "\"") {
                        in_string = 1
                        out = out char
                        continue
                    }
                    if (char == "/" && starts_regex_literal(out)) {
                        in_regex = 1
                        out = out char
                        continue
                    }
                    if (char == "#") {
                        return out
                    }
                    out = out char
                }
                return out
            }
            function has_nul_printf_format(line, i, j, char, before, after, next_pos, in_string, in_regex, escaped, format_escaped) {
                i = 1
                while (i <= length(line)) {
                    char = substr(line, i, 1)
                    if (escaped) {
                        escaped = 0
                        i++
                        continue
                    }
                    if (char == "\\") {
                        escaped = 1
                        i++
                        continue
                    }
                    if (in_string) {
                        if (char == "\"") {
                            in_string = 0
                        }
                        i++
                        continue
                    }
                    if (in_regex) {
                        if (char == "/") {
                            in_regex = 0
                        }
                        i++
                        continue
                    }
                    if (char == "\"") {
                        in_string = 1
                        i++
                        continue
                    }
                    if (char == "/" && starts_regex_literal(substr(line, 1, i - 1))) {
                        in_regex = 1
                        i++
                        continue
                    }
                    if (substr(line, i, 6) != "printf") {
                        i++
                        continue
                    }

                    before = (i == 1) ? "" : substr(line, i - 1, 1)
                    after = substr(line, i + 6, 1)
                    if ((before != "" && is_identifier_char(before)) ||
                        (after != "" && is_identifier_char(after))) {
                        i += 6
                        continue
                    }

                    next_pos = i + 6
                    while (next_pos <= length(line) && substr(line, next_pos, 1) ~ /[[:space:]]/) {
                        next_pos++
                    }
                    if (substr(line, next_pos, 1) == "(") {
                        next_pos++
                        while (next_pos <= length(line) && substr(line, next_pos, 1) ~ /[[:space:]]/) {
                            next_pos++
                        }
                    }
                    if (substr(line, next_pos, 1) != "\"") {
                        i += 6
                        continue
                    }

                    format_escaped = 0
                    for (j = next_pos + 1; j <= length(line); j++) {
                        char = substr(line, j, 1)
                        if (format_escaped) {
                            if (char == "0") {
                                return 1
                            }
                            format_escaped = 0
                            continue
                        }
                        if (char == "\\") {
                            format_escaped = 1
                            continue
                        }
                        if (char == "\"") {
                            break
                        }
                    }
                    i += 6
                }
                return 0
            }
            {
                code_line = strip_awk_comment($0)
                if (has_nul_printf_format(code_line)) {
                    remember_line(NR)
                }
            }
            END {
                if (lines != "") {
                    print lines
                }
            }
        ' "$awk_file")
        if [ -n "$nul_printf_lines" ]; then
            warn "$awk_file uses \\\\0 in printf at line(s) $nul_printf_lines -- use printf \"%c\", 0 instead"
        fi

    done < <(find . \
        -path "./target" -prune -o \
        -path "./.git" -prune -o \
        -path "./third_party" -prune -o \
        -type f -name "*.awk" -print0)

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

run_shellcheck() {
    local shell="$1"
    local severity="$2"
    local script="$3"
    local report

    if report=$(shellcheck -s "$shell" "$severity" "$script" 2>&1); then
        return 0
    fi

    fail "shellcheck errors in $script"
    printf '%s\n' "$report" | sed -n '1,20p'
    echo ""
    return 1
}

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

        if ! run_shellcheck "bash" "$sc_severity" "$script"; then
            shell_errors=$((shell_errors + 1))
        fi
    done

    # Validate scripts in .github/scripts/ directory
    for script in .github/scripts/*.sh; do
        [ -f "$script" ] || continue
        shell_checked=$((shell_checked + 1))

        if ! run_shellcheck "bash" "$sc_severity" "$script"; then
            shell_errors=$((shell_errors + 1))
        fi
    done

    # Validate all git hook wrappers. PowerShell policy lives in scripts/hooks/*.ps1;
    # extensionless .githooks/* files remain small POSIX wrappers for Git.
    for hook in .githooks/*; do
        [ -f "$hook" ] || continue
        shell_checked=$((shell_checked + 1))

        local hook_shell="sh"
        local first_line=""
        IFS= read -r first_line < "$hook" || true
        if [[ "$first_line" == *bash* ]]; then
            hook_shell="bash"
        fi

        if ! run_shellcheck "$hook_shell" "$sc_severity" "$hook"; then
            shell_errors=$((shell_errors + 1))
        fi
    done

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

    if bash scripts/check-internal-links.sh --docs-only --quiet; then
        success "All docs/ internal markdown links are valid"
    else
        fail "Broken internal link(s) found in docs/ markdown files"
    fi
}

# -----------------------------------------------------------------------
# 4. CI/devcontainer tooling parity validation
# -----------------------------------------------------------------------

validate_tooling_parity() {
    CHECKS_RUN=$((CHECKS_RUN + 1))
    info "Validating CI/devcontainer tooling parity..."

    if [ ! -f scripts/check-tooling-parity.sh ]; then
        fail "scripts/check-tooling-parity.sh not found"
        return
    fi

    if bash scripts/check-tooling-parity.sh --quiet; then
        success "CI/devcontainer tooling parity is synchronized"
    else
        fail "CI/devcontainer tooling parity check failed"
    fi
}

# -----------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------

if [ "$QUIET" = false ]; then
    echo -e "${BOLD}${BLUE}CI Configuration Validator${NC}"
    echo "Repository: $REPO_ROOT"
    echo ""
fi

if [ "$RUN_AWK" = true ]; then
    validate_awk_files
    if [ "$QUIET" = false ]; then
        echo ""
    fi
fi

if [ "$RUN_SHELL" = true ]; then
    validate_shell_scripts
    if [ "$QUIET" = false ]; then
        echo ""
    fi
fi

if [ "$RUN_LINKS" = true ]; then
    validate_markdown_links
    if [ "$QUIET" = false ]; then
        echo ""
    fi
fi

if [ "$RUN_TOOLS" = true ]; then
    validate_tooling_parity
    if [ "$QUIET" = false ]; then
        echo ""
    fi
fi

# -----------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------

if [ "$ERRORS" -gt 0 ]; then
    echo "=========================================="
    printf '%b%bFAILED%b: %d error(s), %d warning(s), %d passed (%d checks)\n' \
        "$BOLD" "$RED" "$NC" "$ERRORS" "$WARNINGS" "$CHECKS_PASSED" "$CHECKS_RUN"
    echo ""
    echo "Fix the errors above before pushing to CI."
    echo ""
    echo "Quick reference:"
    echo "  ./scripts/validate-ci.sh --awk      # Re-check AWK files"
    echo "  ./scripts/validate-ci.sh --shell    # Re-check shell scripts"
    echo "  ./scripts/validate-ci.sh --links    # Re-check markdown links"
    echo "  ./scripts/validate-ci.sh --tools    # Re-check tooling parity"
    exit 1
elif [ "$QUIET" = false ]; then
    echo "=========================================="
    if [ "$WARNINGS" -gt 0 ]; then
        printf '%b%bPASSED with warnings%b: %d warning(s), %d passed (%d checks)\n' \
            "$BOLD" "$YELLOW" "$NC" "$WARNINGS" "$CHECKS_PASSED" "$CHECKS_RUN"
    else
        printf '%b%bALL PASSED%b: %d check(s) passed\n' \
            "$BOLD" "$GREEN" "$NC" "$CHECKS_PASSED"
    fi
fi

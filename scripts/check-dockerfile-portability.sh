#!/usr/bin/env bash
# check-dockerfile-portability.sh - Validate shell portability in Dockerfile RUN commands
#
# Dockerfiles default to /bin/sh (typically dash) for RUN commands. This script
# detects bash-specific constructs that will fail or behave unexpectedly under
# /bin/sh unless a SHELL directive overrides the default.
#
# Checks performed:
#   1. Brace expansion ({a,b}) in RUN commands
#   2. Bash-style test brackets [[ ]]
#   3. Arrays (var=(a b c))
#   4. 'source' instead of '.' for sourcing files
#   5. Process substitution <() or >()
#   6. find commands missing -type f when targeting files by extension
#   7. SHELL directive verification when bash features are present
#
# Usage:
#   ./scripts/check-dockerfile-portability.sh                    # Check all Dockerfiles
#   ./scripts/check-dockerfile-portability.sh --quiet            # Suppress success messages
#   ./scripts/check-dockerfile-portability.sh --files FILE...    # Check specific files only
#
# Exit codes:
#   0 = All Dockerfiles pass portability checks
#   1 = One or more portability issues found

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

QUIET=false
FILES_MODE=false
declare -a FILE_LIST=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --quiet|-q)
            QUIET=true
            shift
            ;;
        --files)
            FILES_MODE=true
            shift
            if [ "$#" -eq 0 ]; then
                echo "Error: --files requires at least one file argument"
                exit 2
            fi
            while [ "$#" -gt 0 ]; do
                FILE_LIST+=("$1")
                shift
            done
            ;;
        --help|-h)
            echo "Usage: $0 [--quiet] [--files FILE...]"
            echo ""
            echo "Options:"
            echo "  --quiet         Suppress success messages"
            echo "  --files FILE... Check only the specified files (must be last option)"
            echo "  --help          Show this help"
            echo ""
            echo "Checks Dockerfiles in the repository for shell portability"
            echo "issues in RUN commands."
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
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
FILES_CHECKED=0

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

suggest() {
    printf '         %bFix%b: %s\n' "$YELLOW" "$NC" "$1"
}

# -----------------------------------------------------------------------
# RUN command extraction
#
# Parses a Dockerfile and joins multi-line RUN commands (backslash
# continuations). Outputs tab-separated lines:
#   <start_line_number>\t<full_joined_command>
#
# Also tracks SHELL directives and outputs them as:
#   SHELL\t<line_number>\t<directive_content>
#
# Skips comment lines (lines starting with #, ignoring leading whitespace).
# -----------------------------------------------------------------------

extract_run_commands() {
    local dockerfile="$1"
    awk '
    # Skip comment lines
    /^[[:space:]]*#/ { next }

    # Track SHELL directives
    /^[[:space:]]*SHELL[[:space:]]/ {
        printf "SHELL\t%d\t%s\n", NR, $0
        next
    }

    # Start of a RUN command
    /^[[:space:]]*RUN[[:space:]]/ {
        # Strip the RUN prefix
        sub(/^[[:space:]]*RUN[[:space:]]+/, "")
        start_line = NR
        cmd = $0

        # Handle backslash continuation
        while (sub(/\\[[:space:]]*$/, "", cmd)) {
            if ((getline next_line) > 0) {
                # Skip comment-only continuation lines
                if (next_line ~ /^[[:space:]]*#/) {
                    cmd = cmd "\\"
                    continue
                }
                # Trim leading whitespace from continuation
                sub(/^[[:space:]]+/, "", next_line)
                cmd = cmd " " next_line
            } else {
                break
            }
        }

        printf "%d\t%s\n", start_line, cmd
    }
    ' "$dockerfile"
}

# -----------------------------------------------------------------------
# Individual checks
# -----------------------------------------------------------------------

# Check for brace expansion: {a,b} patterns
# These expand in bash but are literal strings in dash/sh
check_brace_expansion() {
    local file="$1" line_num="$2" cmd="$3"
    # Match {word,word} patterns but skip shell variable expansions like ${var}
    # and --option={value} with no comma (not brace expansion)
    if echo "$cmd" | grep -qE '\{[^}]*,[^}]*\}'; then
        # Exclude ${var:-default} and ${var:+alt} and ${var} patterns
        local stripped
        stripped=$(echo "$cmd" | sed -E 's/\$\{[^}]*\}//g')
        if echo "$stripped" | grep -qE '\{[^}]*,[^}]*\}'; then
            fail "$file:$line_num: Brace expansion detected (not supported by /bin/sh)"
            local match
            match=$(echo "$stripped" | grep -oE '\{[^}]*,[^}]*\}' | head -1)
            printf '         Found: %s\n' "$match"
            suggest "Expand braces manually, e.g., change {a,b} to explicit arguments"
            return 1
        fi
    fi
    return 0
}

# Check for [[ ]] double-bracket tests
check_double_brackets() {
    local file="$1" line_num="$2" cmd="$3"
    if echo "$cmd" | grep -qE '\[\[[[:space:]]'; then
        fail "$file:$line_num: Bash-style [[ ]] test detected (not supported by /bin/sh)"
        suggest "Use single [ ] brackets or 'test' command instead"
        return 1
    fi
    return 0
}

# Check for bash-style arrays: var=(a b c)
check_arrays() {
    local file="$1" line_num="$2" cmd="$3"
    if echo "$cmd" | grep -qE '[a-zA-Z_][a-zA-Z0-9_]*=\('; then
        fail "$file:$line_num: Bash-style array assignment detected (not supported by /bin/sh)"
        suggest "Use space-separated strings with IFS or individual variables"
        return 1
    fi
    return 0
}

# Check for 'source' command (bash-specific; POSIX uses '.')
check_source_command() {
    local file="$1" line_num="$2" cmd="$3"
    # Match 'source' as a command (beginning of command, after && or ||, after ;)
    if echo "$cmd" | grep -qE '(^|&&|\|\||;)[[:space:]]*source[[:space:]]+'; then
        fail "$file:$line_num: 'source' command detected (not portable; use '.' instead)"
        suggest "Replace 'source file' with '. file'"
        return 1
    fi
    return 0
}

# Check for process substitution <() or >()
check_process_substitution() {
    local file="$1" line_num="$2" cmd="$3"
    if echo "$cmd" | grep -qE '[<>]\('; then
        # Exclude legitimate redirect-to-subshell patterns like >(command)
        # but catch actual process substitution
        fail "$file:$line_num: Process substitution <() or >() detected (not supported by /bin/sh)"
        suggest "Use pipes or temporary files instead of process substitution"
        return 1
    fi
    return 0
}

# Check for find commands missing -type f when targeting files by extension
check_find_missing_type() {
    local file="$1" line_num="$2" cmd="$3"
    # Look for: find ... -name '*.ext' without -type f
    if echo "$cmd" | grep -qE 'find[[:space:]].*-name[[:space:]]'; then
        # Check specifically for -type f, not just any -type flag.
        # e.g., -type d or -type l should still trigger the warning.
        if ! echo "$cmd" | grep -qE 'find[[:space:]].*-type[[:space:]]+f([[:space:]]|$)'; then
            # Check if the -name pattern looks like a file extension
            if echo "$cmd" | grep -qE -- "-name[[:space:]]+['\"]?\*\\."; then
                warn "$file:$line_num: find command with -name '*.ext' pattern but no -type f"
                suggest "Add '-type f' to restrict matches to regular files"
                return 0
            fi
        fi
    fi
    return 0
}

# -----------------------------------------------------------------------
# Per-Dockerfile validation
# -----------------------------------------------------------------------

check_dockerfile() {
    local dockerfile="$1"
    local file_errors=0
    local has_shell_bash=false
    local shell_line=0

    info "Checking $dockerfile..."

    # First pass: check for SHELL directive setting bash
    while IFS= read -r raw_line; do
        local first_field
        first_field=$(printf '%s' "$raw_line" | cut -f1)
        if [ "$first_field" = "SHELL" ]; then
            local directive_content
            directive_content=$(printf '%s' "$raw_line" | cut -f3-)
            if echo "$directive_content" | grep -qE 'bash'; then
                has_shell_bash=true
                shell_line=$(printf '%s' "$raw_line" | cut -f2)
            fi
        fi
    done < <(extract_run_commands "$dockerfile")

    # Second pass: check all RUN commands
    local bash_features_found=false
    local run_count=0

    while IFS= read -r raw_line; do
        local first_field
        first_field=$(printf '%s' "$raw_line" | cut -f1)

        # Skip SHELL directives in this pass
        [ "$first_field" = "SHELL" ] && continue

        # For RUN commands: format is <line_num>\t<cmd>
        local line_num cmd
        line_num="$first_field"
        cmd=$(printf '%s' "$raw_line" | cut -f2-)

        run_count=$((run_count + 1))

        local issue_found=false

        # Bash-ism checks: skip if SHELL sets bash or if command starts with bash -c
        # (both make bash features valid).
        local uses_bash_c=false
        if echo "$cmd" | grep -qE '^bash[[:space:]]+-c[[:space:]]'; then
            uses_bash_c=true
        fi

        if [ "$has_shell_bash" = false ] && [ "$uses_bash_c" = false ]; then
            if ! check_brace_expansion "$dockerfile" "$line_num" "$cmd"; then
                issue_found=true
            fi
            if ! check_double_brackets "$dockerfile" "$line_num" "$cmd"; then
                issue_found=true
            fi
            if ! check_arrays "$dockerfile" "$line_num" "$cmd"; then
                issue_found=true
            fi
            if ! check_source_command "$dockerfile" "$line_num" "$cmd"; then
                issue_found=true
            fi
            if ! check_process_substitution "$dockerfile" "$line_num" "$cmd"; then
                issue_found=true
            fi
        fi

        # find -type f check is a warning regardless of shell
        check_find_missing_type "$dockerfile" "$line_num" "$cmd"

        if [ "$issue_found" = true ]; then
            bash_features_found=true
            file_errors=$((file_errors + 1))
        fi
    done < <(extract_run_commands "$dockerfile")

    # If bash features were found without a SHELL directive, add the summary error
    if [ "$bash_features_found" = true ] && [ "$has_shell_bash" = false ]; then
        fail "$dockerfile: Bash features used without SHELL [\"/bin/bash\", \"-c\"] directive"
        suggest "Add 'SHELL [\"/bin/bash\", \"-c\"]' before RUN commands that use bash features"
        suggest "Or rewrite RUN commands to use POSIX-compatible /bin/sh syntax"
    elif [ "$has_shell_bash" = true ]; then
        info "$dockerfile: SHELL directive sets bash (line $shell_line) -- bash features are valid"
    fi

    if [ "$file_errors" -eq 0 ] && [ "$run_count" -gt 0 ]; then
        success "$dockerfile: $run_count RUN command(s) pass portability checks"
    fi

    return 0
}

# -----------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------

if [ "$QUIET" = false ]; then
    echo -e "${BOLD}${BLUE}Dockerfile Shell Portability Checker${NC}"
    echo "Repository: $REPO_ROOT"
    echo ""
fi

# Find Dockerfiles to check
dockerfiles=()
if [ "$FILES_MODE" = true ]; then
    for df in "${FILE_LIST[@]}"; do
        if [ -f "$df" ]; then
            dockerfiles+=("$df")
        else
            warn "File not found (skipped): $df"
        fi
    done
else
    while IFS= read -r -d '' df; do
        dockerfiles+=("$df")
    done < <(find . -type f \( -name "Dockerfile" -o -name "Dockerfile.*" -o -name "*.Dockerfile" \) \
        -not -path "./target/*" \
        -not -path "./.git/*" \
        -not -path "./third_party/*" \
        -print0)
fi

if [ ${#dockerfiles[@]} -eq 0 ]; then
    info "No Dockerfiles found in the repository"
    if [ "$QUIET" = false ]; then
        echo ""
        echo "=========================================="
        printf '%b%bALL PASSED%b: No Dockerfiles to check\n' "$BOLD" "$GREEN" "$NC"
    fi
    exit 0
fi

info "Found ${#dockerfiles[@]} Dockerfile(s)"
if [ "$QUIET" = false ]; then
    echo ""
fi

for dockerfile in "${dockerfiles[@]}"; do
    FILES_CHECKED=$((FILES_CHECKED + 1))
    check_dockerfile "$dockerfile"
    if [ "$QUIET" = false ]; then
        echo ""
    fi
done

# -----------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------

if [ "$ERRORS" -gt 0 ]; then
    echo "=========================================="
    printf '%b%bFAILED%b: %d error(s), %d warning(s), %d passed (%d file(s) checked)\n' \
        "$BOLD" "$RED" "$NC" "$ERRORS" "$WARNINGS" "$CHECKS_PASSED" "$FILES_CHECKED"
    echo ""
    echo "Dockerfile RUN commands default to /bin/sh (dash), not bash."
    echo "Either rewrite commands using POSIX-compatible syntax or add:"
    echo "  SHELL [\"/bin/bash\", \"-c\"]"
    echo "before the RUN commands that require bash."
    exit 1
elif [ "$QUIET" = false ]; then
    echo "=========================================="
    if [ "$WARNINGS" -gt 0 ]; then
        printf '%b%bPASSED with warnings%b: %d warning(s), %d passed (%d file(s) checked)\n' \
            "$BOLD" "$YELLOW" "$NC" "$WARNINGS" "$CHECKS_PASSED" "$FILES_CHECKED"
    else
        printf '%b%bALL PASSED%b: %d check(s) passed (%d file(s) checked)\n' \
            "$BOLD" "$GREEN" "$NC" "$CHECKS_PASSED" "$FILES_CHECKED"
    fi
fi

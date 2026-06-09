#!/usr/bin/env bash
# validate-lychee-config.sh - Validate .lychee.toml configuration
#
# This script validates that the lychee link checker configuration is correct
# and catches common configuration errors before they cause CI failures.
#
# Usage:
#   ./scripts/validate-lychee-config.sh
#
# Exit codes:
#   0 - Configuration is valid
#   1 - Configuration errors found

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

ERRORS=0
WARNINGS=0

info()    { printf '\033[1;34m[INFO]\033[0m  %s\n' "$1"; }
warn()    { printf '\033[1;33m[WARN]\033[0m  %s\n' "$1"; WARNINGS=$((WARNINGS + 1)); }
error()   { printf '\033[1;31m[ERROR]\033[0m %s\n' "$1"; ERRORS=$((ERRORS + 1)); }
success() { printf '\033[1;32m[OK]\033[0m    %s\n' "$1"; }

toml_key_exists() {
    local key="$1"
    awk -v key="$key" '
        /^[[:space:]]*(#|$)/ { next }
        /^[[:space:]]*\[\[?[^]]+\]\]?[[:space:]]*(#.*)?$/ {
            current = $0
            sub(/[[:space:]]*#.*$/, "", current)
            gsub(/^[[:space:]]*\[\[?|\]\]?[[:space:]]*$/, "", current)
            next
        }
        {
            if (current != "") {
                next
            }
            line = $0
            sub(/^[[:space:]]*/, "", line)
            if (line ~ "^" key "[[:space:]]*=") {
                found = 1
                exit
            }
        }
        END {
            if (found == 1) {
                exit 0
            }
            exit 1
        }
    ' .lychee.toml
}

toml_value_for_key() {
    local key="$1"
    awk -v key="$key" '
        function trim(value) {
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
            return value
        }
        function strip_comment(value,    i, ch, out, quote, in_string, escaped) {
            out = ""
            quote = ""
            in_string = 0
            escaped = 0

            for (i = 1; i <= length(value); i++) {
                ch = substr(value, i, 1)

                if (in_string == 1) {
                    if (ch == quote && (quote != "\"" || escaped == 0)) {
                        in_string = 0
                        quote = ""
                    }
                } else if (ch == "\"" || ch == sprintf("%c", 39)) {
                    in_string = 1
                    quote = ch
                }
                if (ch == "#" && in_string == 0) {
                    break
                }

                out = out ch

                if (quote == "\"" && ch == "\\" && escaped == 0) {
                    escaped = 1
                } else {
                    escaped = 0
                }
            }

            return out
        }
        /^[[:space:]]*(#|$)/ { next }
        /^[[:space:]]*\[\[?[^]]+\]\]?[[:space:]]*(#.*)?$/ {
            current = $0
            sub(/[[:space:]]*#.*$/, "", current)
            gsub(/^[[:space:]]*\[\[?|\]\]?[[:space:]]*$/, "", current)
            next
        }
        {
            if (current != "") {
                next
            }
            line = $0
            sub(/^[[:space:]]*/, "", line)
            if (line ~ "^" key "[[:space:]]*=") {
                sub(/^[^=]*=/, "", line)
                print trim(strip_comment(line))
                found = 1
                exit
            }
        }
        END {
            if (found == 1) {
                exit 0
            }
            exit 1
        }
    ' .lychee.toml
}

toml_array_values() {
    local key="$1"
    awk -v key="$key" '
        function trim(value) {
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
            return value
        }
        function strip_comment(value,    i, ch, out, quote, in_string, escaped) {
            out = ""
            quote = ""
            in_string = 0
            escaped = 0

            for (i = 1; i <= length(value); i++) {
                ch = substr(value, i, 1)

                if (in_string == 1) {
                    if (ch == quote && (quote != "\"" || escaped == 0)) {
                        in_string = 0
                        quote = ""
                    }
                } else if (ch == "\"" || ch == sprintf("%c", 39)) {
                    in_string = 1
                    quote = ch
                }
                if (ch == "#" && in_string == 0) {
                    break
                }

                out = out ch

                if (quote == "\"" && ch == "\\" && escaped == 0) {
                    escaped = 1
                } else {
                    escaped = 0
                }
            }

            return out
        }
        function print_quoted_strings(value,    i, ch, out, quote, in_string, escaped) {
            out = ""
            quote = ""
            in_string = 0
            escaped = 0

            for (i = 1; i <= length(value); i++) {
                ch = substr(value, i, 1)

                if (in_string == 1) {
                    if (quote == "\"" && escaped == 1) {
                        if (ch == "\\" || ch == "\"") {
                            out = out ch
                        } else if (ch == "n") {
                            out = out "\n"
                        } else if (ch == "t") {
                            out = out "\t"
                        } else if (ch == "r") {
                            out = out "\r"
                        } else {
                            out = out "\\" ch
                        }
                        escaped = 0
                    } else if (quote == "\"" && ch == "\\") {
                        escaped = 1
                    } else if (ch == quote) {
                        print out
                        out = ""
                        in_string = 0
                        quote = ""
                    } else {
                        out = out ch
                    }
                } else if (ch == "\"" || ch == sprintf("%c", 39)) {
                    in_string = 1
                    quote = ch
                    out = ""
                }
            }
        }
        function array_is_closed(value,    i, ch, quote, in_string, escaped) {
            quote = ""
            in_string = 0
            escaped = 0

            for (i = 1; i <= length(value); i++) {
                ch = substr(value, i, 1)

                if (in_string == 1) {
                    if (ch == quote && (quote != "\"" || escaped == 0)) {
                        in_string = 0
                        quote = ""
                    }
                } else if (ch == "\"" || ch == sprintf("%c", 39)) {
                    in_string = 1
                    quote = ch
                }
                if (ch == "]" && in_string == 0) {
                    return 1
                }

                if (quote == "\"" && ch == "\\" && escaped == 0) {
                    escaped = 1
                } else {
                    escaped = 0
                }
            }

            return 0
        }
        /^[[:space:]]*(#|$)/ { next }
        /^[[:space:]]*\[\[?[^]]+\]\]?[[:space:]]*(#.*)?$/ {
            current = $0
            sub(/[[:space:]]*#.*$/, "", current)
            gsub(/^[[:space:]]*\[\[?|\]\]?[[:space:]]*$/, "", current)
            next
        }
        {
            if (current != "") {
                next
            }
            line = strip_comment($0)
            trimmed = trim(line)

            if (in_array != 1) {
                if (trimmed ~ "^" key "[[:space:]]*=") {
                    sub(/^[^=]*=/, "", line)
                    line = trim(line)
                    if (line !~ /^\[/) {
                        exit 2
                    }
                    in_array = 1
                } else {
                    next
                }
            }

            print_quoted_strings(line)
            if (array_is_closed(line) == 1) {
                found = 1
                exit
            }
        }
        END {
            if (found == 1) {
                exit 0
            }
            if (in_array == 1) {
                exit 3
            }
            exit 1
        }
    ' .lychee.toml
}

parse_positive_integer() {
    local field="$1"
    local raw="$2"

    if [[ ! "$raw" =~ ^\+?[0-9](_?[0-9])*$ ]]; then
        error "$field must be an integer, got: $raw"
        return 1
    fi
}

regex_array_matches_url() {
    local array_key="$1"
    local url="$2"
    local pattern

    while IFS= read -r pattern; do
        if printf '%s\n' "$url" | grep -Eq -- "$pattern" 2>/dev/null; then
            return 0
        fi
    done < <(toml_array_values "$array_key" || true)

    return 1
}

echo "========================================="
echo "Lychee Configuration Validation"
echo "========================================="
echo ""

# Check if .lychee.toml exists
info "Checking for .lychee.toml..."
if [ ! -f .lychee.toml ]; then
    error ".lychee.toml not found"
    error "Create .lychee.toml with link checker configuration"
    exit 1
fi
success ".lychee.toml found"

# Check if lychee is installed (for validation)
if ! command -v lychee &> /dev/null; then
    warn "lychee is not installed (cannot test configuration)"
    warn "Install with: cargo install lychee"
else
    # Test configuration by running lychee with --dump flag
    info "Testing configuration syntax..."
    if lychee --dump .lychee.toml > /dev/null 2>&1; then
        success "Configuration syntax is valid"
    else
        error "Configuration has syntax errors"
        error "Run: lychee --dump .lychee.toml"
        exit 1
    fi
fi

# Validate required fields
info "Checking required fields..."

required_fields=(
    "max_concurrency"
    "accept"
    "exclude"
    "timeout"
    "user_agent"
)

for field in "${required_fields[@]}"; do
    if toml_key_exists "$field"; then
        success "Found: $field"
    else
        error "Missing required field: $field"
    fi
done

# Validate placeholder URL exclusions
info "Checking placeholder URL exclusions..."

# Common placeholder patterns that should be excluded
placeholder_patterns=(
    "http://localhost"
    "http://127.0.0.1"
    "ws://localhost"
    "mailto:"
)

for pattern in "${placeholder_patterns[@]}"; do
    if regex_array_matches_url "exclude" "$pattern"; then
        success "Excludes: $pattern"
    else
        warn "Missing exclusion for: $pattern"
    fi
done

# Check for common configuration mistakes
info "Checking for common configuration mistakes..."

# Check for quoted booleans (should be unquoted)
if awk '
    function trim(value) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
        return value
    }
    /^[[:space:]]*(#|$)/ { next }
    {
        line = $0
        sub(/#.*/, "", line)
        if (line ~ /=/) {
            sub(/^[^=]*=/, "", line)
            value = trim(line)
            if (value == "\"true\"" || value == "\"false\"") {
                found = 1
                exit
            }
        }
    }
    END { exit found == 1 ? 0 : 1 }
' .lychee.toml; then
    error "Boolean values should not be quoted"
    error "Use: field = true (not field = \"true\")"
fi

# Check that arrays use brackets
for array_field in exclude accept; do
    if value=$(toml_value_for_key "$array_field"); then
        if [[ ! "$value" =~ ^\[ ]]; then
            error "$array_field must be an array with brackets []"
        fi
    fi
done

# Check for sensible timeout
if timeout_value=$(toml_value_for_key "timeout"); then
    if parse_positive_integer "timeout" "$timeout_value"; then
        timeout="${timeout_value#+}"
        timeout="${timeout//_/}"
        if [ "$timeout" -lt 5 ]; then
            warn "Timeout is very short ($timeout seconds)"
            warn "Consider increasing to at least 10 seconds"
        elif [ "$timeout" -gt 60 ]; then
            warn "Timeout is very long ($timeout seconds)"
            warn "Consider reducing to 30-60 seconds"
        else
            success "Timeout is reasonable ($timeout seconds)"
        fi
    fi
fi

# Check for max_concurrency
if concurrency_value=$(toml_value_for_key "max_concurrency"); then
    if parse_positive_integer "max_concurrency" "$concurrency_value"; then
        concurrency="${concurrency_value#+}"
        concurrency="${concurrency//_/}"
        if [ "$concurrency" -lt 1 ]; then
            error "max_concurrency must be at least 1"
        elif [ "$concurrency" -gt 100 ]; then
            warn "max_concurrency is very high ($concurrency)"
            warn "Consider reducing to 10-50 to avoid rate limiting"
        else
            success "max_concurrency is reasonable ($concurrency)"
        fi
    fi
fi

# Validate exclude_path entries
info "Checking exclude_path entries..."
if toml_key_exists "exclude_path"; then
    success "Found exclude_path configuration"

    # Check for common paths that should be excluded
    common_excludes=("target/" ".git/" "third_party/" "node_modules/")
    for path in "${common_excludes[@]}"; do
        if toml_array_values "exclude_path" | grep -Fxq "$path"; then
            success "Excludes: $path"
        else
            warn "Consider excluding: $path"
        fi
    done
else
    warn "No exclude_path configuration found"
    warn "Consider adding exclude_path for target/, .git/, etc."
fi

# Summary
echo ""
echo "========================================="
echo "Validation Summary"
echo "========================================="

if [ $ERRORS -gt 0 ]; then
    echo -e "${RED}✗ Validation failed with $ERRORS error(s) and $WARNINGS warning(s)${NC}"
    exit 1
elif [ $WARNINGS -gt 0 ]; then
    echo -e "${YELLOW}⚠ Validation passed with $WARNINGS warning(s)${NC}"
    exit 0
else
    echo -e "${GREEN}✓ All validations passed${NC}"
    exit 0
fi

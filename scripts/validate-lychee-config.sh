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
    if grep -q "^${field}" .lychee.toml; then
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
    if grep -q "$pattern" .lychee.toml; then
        success "Excludes: $pattern"
    else
        warn "Missing exclusion for: $pattern"
    fi
done

# Check for common configuration mistakes
info "Checking for common configuration mistakes..."

# Check for quoted booleans (should be unquoted)
if grep -qE '= "(true|false)"' .lychee.toml; then
    error "Boolean values should not be quoted"
    error "Use: field = true (not field = \"true\")"
fi

# Check that arrays use brackets
if grep -qE '^(exclude|accept) = [^[]' .lychee.toml; then
    error "exclude/accept must be arrays with brackets []"
fi

# Check for sensible timeout
if grep -qE 'timeout = [0-9]+' .lychee.toml; then
    timeout=$(grep -oP 'timeout = \K[0-9]+' .lychee.toml || echo "0")
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

# Check for max_concurrency
if grep -qE 'max_concurrency = [0-9]+' .lychee.toml; then
    concurrency=$(grep -oP 'max_concurrency = \K[0-9]+' .lychee.toml || echo "0")
    if [ "$concurrency" -lt 1 ]; then
        error "max_concurrency must be at least 1"
    elif [ "$concurrency" -gt 100 ]; then
        warn "max_concurrency is very high ($concurrency)"
        warn "Consider reducing to 10-50 to avoid rate limiting"
    else
        success "max_concurrency is reasonable ($concurrency)"
    fi
fi

# Validate exclude_path entries
info "Checking exclude_path entries..."
if grep -q "^exclude_path" .lychee.toml; then
    success "Found exclude_path configuration"

    # Check for common paths that should be excluded
    common_excludes=("target/" ".git/" "third_party/" "node_modules/")
    for path in "${common_excludes[@]}"; do
        if grep -q "\"$path\"" .lychee.toml; then
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

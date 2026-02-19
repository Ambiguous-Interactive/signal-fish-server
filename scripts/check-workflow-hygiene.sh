#!/usr/bin/env bash
# Workflow Hygiene Checker
#
# Validates GitHub Actions workflow files for common issues and misconfigurations.
# Catches problems before they cause CI failures.
#
# This script was created to prevent recurrence of three actual CI issues:
#   1. Python cache setup on non-Python project (yaml-lint.yml)
#   2. Nightly toolchain becoming stale (>360 days old)
#   3. Dependencies not actually used in code
#
# Usage:
#   ./scripts/check-workflow-hygiene.sh
#
# Exit codes:
#   0 = All checks passed or warnings only
#   1 = Critical errors found
#   2 = Invalid usage or missing files
#
# shellcheck disable=SC2094  # False positive: we read workflow files but never write them

set -euo pipefail

# Color output (disable if not a TTY)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m' # No Color
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

ERRORS=0
WARNINGS=0

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    ERRORS=$((ERRORS + 1))
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
    WARNINGS=$((WARNINGS + 1))
}

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

# Find repository root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

echo -e "${BLUE}Workflow Hygiene Checker${NC}"
echo "Repository: $REPO_ROOT"
echo ""

# ---------------------------------------------------------------------------
# 1. Check for language-specific caching on wrong project types
# ---------------------------------------------------------------------------
info "Checking for language-specific caching mismatches..."

# Determine project type
IS_RUST_PROJECT=false
IS_PYTHON_PROJECT=false
IS_NODE_PROJECT=false

[ -f "Cargo.toml" ] && IS_RUST_PROJECT=true
[ -f "requirements.txt" ] || [ -f "requirements-docs.txt" ] || [ -f "Pipfile" ] || [ -f "pyproject.toml" ] && IS_PYTHON_PROJECT=true
[ -f "package.json" ] && IS_NODE_PROJECT=true

info "Project type detection:"
info "  Rust: $IS_RUST_PROJECT"
info "  Python: $IS_PYTHON_PROJECT"
info "  Node: $IS_NODE_PROJECT"
echo ""

# Check all workflow files
for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    # Check for Python caching on non-Python projects
    if [ "$IS_PYTHON_PROJECT" = "false" ]; then
        if grep -q "cache: 'pip'" "$workflow" 2>/dev/null || \
           grep -q "cache: pip" "$workflow" 2>/dev/null; then
            error "$(basename "$workflow"): Uses Python pip cache but no Python project files found"
            error "  Remove 'cache: pip' or add comment explaining why it's needed"
        fi
    fi

    # Check for Node caching on non-Node projects
    if [ "$IS_NODE_PROJECT" = "false" ]; then
        if grep -q "cache: 'npm'" "$workflow" 2>/dev/null || \
           grep -q "cache: npm" "$workflow" 2>/dev/null || \
           grep -q "cache: 'yarn'" "$workflow" 2>/dev/null; then
            error "$(basename "$workflow"): Uses Node cache but no package.json found"
            error "  Remove cache configuration or add comment explaining why it's needed"
        fi
    fi
done

if [ "$ERRORS" -eq 0 ]; then
    success "No language-specific caching mismatches found"
fi
echo ""

# ---------------------------------------------------------------------------
# 2. Check for stale nightly toolchains
# ---------------------------------------------------------------------------
info "Checking for stale nightly Rust toolchains..."

NIGHTLY_STALENESS_WARN_DAYS=180  # 6 months
NIGHTLY_STALENESS_ERROR_DAYS=365 # 1 year

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    # Extract nightly toolchain versions
    while IFS= read -r line; do
        if [[ "$line" =~ toolchain:[[:space:]]*nightly-([0-9]{4})-([0-9]{2})-([0-9]{2}) ]]; then
            NIGHTLY_DATE="${BASH_REMATCH[1]}-${BASH_REMATCH[2]}-${BASH_REMATCH[3]}"
            WORKFLOW_NAME=$(basename "$workflow")

            # Calculate age in days
            NIGHTLY_EPOCH=$(date -d "$NIGHTLY_DATE" +%s 2>/dev/null || echo 0)
            CURRENT_EPOCH=$(date +%s)
            AGE_DAYS=$(( (CURRENT_EPOCH - NIGHTLY_EPOCH) / 86400 ))

            if [ "$NIGHTLY_EPOCH" -eq 0 ]; then
                warn "$WORKFLOW_NAME: Could not parse nightly date: $NIGHTLY_DATE"
                continue
            fi

            info "$WORKFLOW_NAME: nightly-$NIGHTLY_DATE is $AGE_DAYS days old"

            if [ "$AGE_DAYS" -gt "$NIGHTLY_STALENESS_ERROR_DAYS" ]; then
                error "$WORKFLOW_NAME: Nightly toolchain is over 1 year old ($AGE_DAYS days)"
                error "  Update toolchain to nightly-$(date +%Y-%m-%d -d '1 month ago')"
                error "  See .llm/skills/msrv-and-toolchain-management.md for update procedure"
            elif [ "$AGE_DAYS" -gt "$NIGHTLY_STALENESS_WARN_DAYS" ]; then
                warn "$WORKFLOW_NAME: Nightly toolchain is over 6 months old ($AGE_DAYS days)"
                warn "  Consider updating to nightly-$(date +%Y-%m-%d -d '1 month ago')"
            else
                success "$WORKFLOW_NAME: Nightly toolchain is recent (< 6 months old)"
            fi
        fi
    done < "$workflow"
done
echo ""

# ---------------------------------------------------------------------------
# 3. Check for commented or documented nightly versions
# ---------------------------------------------------------------------------
info "Checking for nightly toolchain documentation..."

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    if grep -q "toolchain: nightly" "$workflow" 2>/dev/null; then
        WORKFLOW_NAME=$(basename "$workflow")

        # Check if there's documentation about the nightly version
        # Look for: Nightly Version, Last Updated, Update Criteria, or substantial header comment
        # Use case-insensitive search and look within 50 lines before toolchain declaration
        if head -n 80 "$workflow" | grep -qi "nightly.*version\|last updated\|update criteria\|nightly toolchain strategy" 2>/dev/null; then
            success "$WORKFLOW_NAME: Nightly toolchain is documented"
        else
            warn "$WORKFLOW_NAME: Uses nightly toolchain but lacks documentation"
            warn "  Add comment explaining why nightly is needed and when to update it"
            warn "  See .github/workflows/unused-deps.yml for example documentation"
        fi
    fi
done
echo ""

# ---------------------------------------------------------------------------
# 4. Check for workflow self-validation
# ---------------------------------------------------------------------------
info "Checking for workflow self-validation..."

HAS_ACTIONLINT=false
HAS_YAML_LINT=false
HAS_SHELLCHECK=false

[ -f ".github/workflows/actionlint.yml" ] && HAS_ACTIONLINT=true
[ -f ".github/workflows/yaml-lint.yml" ] && HAS_YAML_LINT=true

# Check if any workflow has shellcheck
for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue
    if grep -q "shellcheck" "$workflow" 2>/dev/null; then
        HAS_SHELLCHECK=true
        break
    fi
done

if [ "$HAS_ACTIONLINT" = "true" ]; then
    success "actionlint workflow found (.github/workflows/actionlint.yml)"
else
    warn "No actionlint workflow found"
    warn "  Consider adding actionlint to validate GitHub Actions syntax"
fi

if [ "$HAS_YAML_LINT" = "true" ]; then
    success "YAML lint workflow found (.github/workflows/yaml-lint.yml)"
else
    warn "No YAML lint workflow found"
    warn "  Consider adding yamllint to validate YAML syntax"
fi

if [ "$HAS_SHELLCHECK" = "true" ]; then
    success "Shellcheck found in workflows"
else
    warn "No shellcheck found in workflows"
    warn "  Consider adding shellcheck to validate inline shell scripts"
fi
echo ""

# ---------------------------------------------------------------------------
# 5. Check for dependency audit workflows
# ---------------------------------------------------------------------------
info "Checking for dependency audit workflows..."

HAS_CARGO_DENY=false
HAS_CARGO_MACHETE=false
HAS_CARGO_UDEPS=false

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    grep -q "cargo-deny" "$workflow" 2>/dev/null && HAS_CARGO_DENY=true
    grep -q "cargo-machete" "$workflow" 2>/dev/null && HAS_CARGO_MACHETE=true
    grep -q "cargo-udeps" "$workflow" 2>/dev/null && HAS_CARGO_UDEPS=true
done

if [ "$HAS_CARGO_DENY" = "true" ]; then
    success "cargo-deny workflow found (security/license auditing)"
else
    warn "No cargo-deny workflow found"
    warn "  Consider adding cargo-deny for security and license auditing"
fi

if [ "$HAS_CARGO_MACHETE" = "true" ]; then
    success "cargo-machete workflow found (unused dependency detection)"
else
    warn "No cargo-machete workflow found"
    warn "  Consider adding cargo-machete to detect unused dependencies"
fi

if [ "$HAS_CARGO_UDEPS" = "true" ]; then
    success "cargo-udeps workflow found (advanced unused dependency detection)"
else
    info "No cargo-udeps workflow found (optional)"
fi
echo ""

# ---------------------------------------------------------------------------
# 6. Check for timeout configurations
# ---------------------------------------------------------------------------
info "Checking for job timeouts..."

WORKFLOWS_WITHOUT_TIMEOUT=0

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    WORKFLOW_NAME=$(basename "$workflow")

    # Count timeout-minutes occurrences (grep -c returns empty string on no match, default to 0)
    TIMEOUT_COUNT=$(grep -c "timeout-minutes:" "$workflow" 2>/dev/null) || TIMEOUT_COUNT=0

    # Check if workflow has jobs section (indicating it's an actual workflow, not a config file)
    if grep -q "^jobs:" "$workflow" 2>/dev/null; then
        # If any job has timeout, assume best practice is followed
        # (checking each job individually would require YAML parsing)
        if [ "$TIMEOUT_COUNT" -eq 0 ]; then
            warn "$WORKFLOW_NAME: No timeout-minutes found (consider adding to prevent hung jobs)"
            WORKFLOWS_WITHOUT_TIMEOUT=$((WORKFLOWS_WITHOUT_TIMEOUT + 1))
        fi
    fi
done

if [ "$WORKFLOWS_WITHOUT_TIMEOUT" -eq 0 ]; then
    success "All workflows have timeout configurations"
fi
echo ""

# ---------------------------------------------------------------------------
# 7. Check for pinned action versions
# ---------------------------------------------------------------------------
info "Checking for pinned GitHub Actions versions..."

UNPINNED_COUNT=0
PINNED_COUNT=0

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    WORKFLOW_NAME=$(basename "$workflow")

    # Look for uses: that don't have SHA pins
    while IFS= read -r line; do
        if [[ "$line" =~ uses:[[:space:]]*[^@]+@([^#[:space:]]+) ]]; then
            VERSION="${BASH_REMATCH[1]}"

            # Check if version is a SHA (40 hex characters)
            if [[ "$VERSION" =~ ^[0-9a-f]{40}$ ]]; then
                PINNED_COUNT=$((PINNED_COUNT + 1))
            else
                UNPINNED_COUNT=$((UNPINNED_COUNT + 1))
            fi
        fi
    done < "$workflow"
done

if [ "$UNPINNED_COUNT" -gt 0 ]; then
    # This is informational - pinning to SHA is best practice but not required
    info "Found $UNPINNED_COUNT actions not pinned to SHA (consider pinning for supply chain security)"
    info "Found $PINNED_COUNT actions properly pinned to SHA"
elif [ "$PINNED_COUNT" -gt 0 ]; then
    success "All $PINNED_COUNT actions are pinned to SHA hashes"
else
    info "No GitHub Actions found in workflows"
fi
echo ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "=========================================="
if [ "$ERRORS" -gt 0 ]; then
    error "Workflow hygiene check found $ERRORS error(s) and $WARNINGS warning(s)"
    echo ""
    echo "Critical issues must be fixed before merging."
    echo "See error messages above for remediation steps."
    exit 1
elif [ "$WARNINGS" -gt 0 ]; then
    warn "Workflow hygiene check completed with $WARNINGS warning(s)"
    echo ""
    echo "Warnings are recommendations to improve CI/CD robustness."
    echo "Consider addressing them to prevent future issues."
    exit 0
else
    success "All workflow hygiene checks passed!"
    exit 0
fi

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
                error "  See .llm/skills/msrv-management.md for update procedure"
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
# 8. Check for cargo commands missing --locked
# ---------------------------------------------------------------------------
info "Checking for cargo commands missing --locked..."

# Extract run blocks from a workflow YAML file.
# Output format: NUL-delimited records: "<start_line>\t<run_content>".
extract_run_blocks_for_workflow() {
    local workflow_file="$1"

    awk '
        function indent_level(s,    i, c, n) {
            n = 0
            for (i = 1; i <= length(s); i++) {
                c = substr(s, i, 1)
                if (c == " " || c == "\t") {
                    n++
                } else {
                    break
                }
            }
            return n
        }

        function ltrim(s) {
            sub(/^[ \t]+/, "", s)
            return s
        }

        function flush_run_block() {
            if (in_run_block) {
                printf "%d\t%s%c", run_start_line, run_content, 0
                in_run_block = 0
                run_content = ""
                run_block_indent = -1
            }
        }

        {
            line = $0
            trimmed = ltrim(line)
            current_indent = indent_level(line)

            # Continue collecting a run: | / run: > block until we hit a
            # non-empty line at the same or lower indentation as the run key.
            if (in_run_block) {
                if (trimmed != "" && current_indent <= run_key_indent) {
                    flush_run_block()
                    # Fall through: this line may start a new run key.
                } else {
                    content_line = line
                    if (trimmed != "") {
                        if (run_block_indent < 0) {
                            run_block_indent = current_indent
                        }
                        remove = run_block_indent
                        while (remove > 0 && length(content_line) > 0) {
                            first = substr(content_line, 1, 1)
                            if (first == " " || first == "\t") {
                                content_line = substr(content_line, 2)
                                remove--
                            } else {
                                break
                            }
                        }
                    } else {
                        content_line = ""
                    }

                    if (run_content == "") {
                        run_content = content_line
                    } else {
                        run_content = run_content "\n" content_line
                    }
                    next
                }
            }

            # Multiline YAML run block (literal/folded scalars).
            if (trimmed ~ /^run:[ \t]*[|>]/) {
                in_run_block = 1
                run_start_line = NR
                run_key_indent = current_indent
                run_block_indent = -1
                run_content = ""
                next
            }

            # Single-line run key.
            if (trimmed ~ /^run:[ \t]*[^|>]/) {
                inline_cmd = trimmed
                sub(/^run:[ \t]*/, "", inline_cmd)
                printf "%d\t%s%c", NR, inline_cmd, 0
            }
        }

        END {
            flush_run_block()
        }
    ' "$workflow_file"
}

# Normalize shell continuation lines from a run block into logical commands.
# Example:
#   cargo test \
#     --all-features \
#     --locked
# becomes one logical line.
normalize_run_block_commands() {
    local run_block="$1"

    printf '%s\n' "$run_block" | awk '
        {
            line = $0

            if (continued) {
                sub(/^[ \t]+/, "", line)
                logical = logical " " line
            } else {
                logical = line
            }

            if (line ~ /\\[ \t]*$/) {
                sub(/\\[ \t]*$/, "", logical)
                continued = 1
            } else {
                print logical
                continued = 0
                logical = ""
            }
        }

        END {
            if (continued && logical != "") {
                print logical
            }
        }
    '
}

# Commands that are exempt from the --locked requirement:
#   - cargo fmt: Formatter only, does not resolve dependencies
#   - cargo publish: Intentionally resolves from registry for crates.io compatibility
#   - cargo install: Installing tools, not building the project
#   - cargo machete: Static analysis of Cargo.toml, does not compile
#   - cargo sbom: cargo-sbom does not support --locked
#   - cargo clean: Output-only, does not affect reproducibility of build results
#   - cargo init/new/search/login/owner/yank: Registry or scaffolding commands,
#     not project builds (unlikely in CI but listed for completeness)
#   - cargo bench: Benchmarking, not a correctness gate
LOCKED_EXEMPT_PATTERNS="fmt|publish|install|machete|sbom|clean|init|new|search|login|owner|yank|bench"

MISSING_LOCKED=0
for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue
    WORKFLOW_NAME=$(basename "$workflow")

    # Parse full run blocks (single-line and multiline) and then scan logical
    # shell command lines. This avoids false positives/negatives where command
    # flags are split across YAML lines.
    while IFS= read -r -d '' run_record; do
        RUN_START_LINE=${run_record%%$'\t'*}
        RUN_BLOCK=${run_record#*$'\t'}

        while IFS= read -r logical_cmd; do
            # Split chained commands (&&, ||, ;) into individual statements.
            while IFS= read -r statement; do
                # Trim leading/trailing whitespace.
                statement=$(echo "$statement" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
                [ -z "$statement" ] && continue
                [[ "$statement" =~ ^# ]] && continue

                # Match cargo command invocations with optional +toolchain.
                if [[ "$statement" =~ (^|[[:space:]\(\)\{\}\|&;])cargo[[:space:]](\+[^[:space:]]+[[:space:]]+)?([a-z-]+) ]]; then
                    CARGO_SUBCMD="${BASH_REMATCH[3]}"

                    # Known incompatible command/flag combinations (real CI regressions).
                    if [ "$CARGO_SUBCMD" = "sbom" ] && echo "$statement" | grep -q -- "--locked"; then
                        error "$WORKFLOW_NAME:$RUN_START_LINE: cargo sbom does not support --locked"
                        error "  Command: $statement"
                        continue
                    fi
                    if [ "$CARGO_SUBCMD" = "llvm-cov" ] &&
                       echo "$statement" | grep -q "llvm-cov[[:space:]]\+report" &&
                       echo "$statement" | grep -qE -- "--all-features|--workspace"; then
                        error "$WORKFLOW_NAME:$RUN_START_LINE: cargo llvm-cov report does not accept --all-features/--workspace"
                        error "  Command: $statement"
                        continue
                    fi

                    # Skip exempt commands
                    if echo "$CARGO_SUBCMD" | grep -qE "^($LOCKED_EXEMPT_PATTERNS)$"; then
                        continue
                    fi

                    # cargo miri setup is exempt (tool setup, not a project build),
                    # but cargo miri test should use --locked like any test command.
                    if [ "$CARGO_SUBCMD" = "miri" ] && echo "$statement" | grep -q "miri[[:space:]]\+setup"; then
                        continue
                    fi

                    if ! echo "$statement" | grep -q -- "--locked"; then
                        warn "$WORKFLOW_NAME:$RUN_START_LINE: 'cargo $CARGO_SUBCMD' missing --locked flag"
                        warn "  Command: $statement"
                        MISSING_LOCKED=$((MISSING_LOCKED + 1))
                    fi
                fi
            done < <(printf '%s\n' "$logical_cmd" | sed -E 's/(&&|\|\||;)/\n/g')
        done < <(normalize_run_block_commands "$RUN_BLOCK")
    done < <(extract_run_blocks_for_workflow "$workflow")
done

if [ "$MISSING_LOCKED" -eq 0 ]; then
    success "All cargo build/test/check commands use --locked"
fi
echo ""

# ---------------------------------------------------------------------------
# 9. Check for unpinned external tooling execution
# ---------------------------------------------------------------------------
info "Checking automation files for unpinned external tooling execution..."

TOOLING_PIN_VIOLATIONS=0

for candidate in scripts/*.sh .githooks/* .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$candidate" ] || continue
    [ "$candidate" = "scripts/check-workflow-hygiene.sh" ] && continue

    # Avoid on-demand npx execution in automation.
    if grep -nE '^[[:space:]]*npx([[:space:]]|$)|[;&|][[:space:]]*npx([[:space:]]|$)' "$candidate" >/dev/null 2>&1; then
        error "$candidate: Uses npx invocation in automation (on-demand package execution is disallowed)"
        grep -nE '^[[:space:]]*npx([[:space:]]|$)|[;&|][[:space:]]*npx([[:space:]]|$)' "$candidate" | sed 's/^/  /'
        TOOLING_PIN_VIOLATIONS=$((TOOLING_PIN_VIOLATIONS + 1))
    fi

    # Require immutable image tags for third-party images in automation.
    while IFS= read -r match; do
        line_no=${match%%:*}
        line_body=${match#*:}
        image_ref=$(echo "$line_body" | sed -nE 's/.*([A-Za-z0-9._-]+\/[A-Za-z0-9._\/-]+):[Ll][Aa][Tt][Ee][Ss][Tt].*/\1/p')
        [ -n "$image_ref" ] || continue

        case "$image_ref" in
            ghcr.io/ambiguousinteractive/signal-fish-server|ambiguousinteractive/signal-fish-server)
                continue
                ;;
        esac

        error "$candidate:$line_no: Uses mutable Docker tag ':latest' for external image '$image_ref'"
        TOOLING_PIN_VIOLATIONS=$((TOOLING_PIN_VIOLATIONS + 1))
    done < <(grep -nE '[A-Za-z0-9._-]+/[A-Za-z0-9._/-]+:[Ll][Aa][Tt][Ee][Ss][Tt]' "$candidate" || true)
done

if [ "$TOOLING_PIN_VIOLATIONS" -eq 0 ]; then
    success "No unpinned external tooling execution patterns found"
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

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
# 7. Check workflow files end with newline
# ---------------------------------------------------------------------------
info "Checking workflow files for trailing newline..."

WORKFLOWS_MISSING_EOF_NEWLINE=0

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    if [ ! -s "$workflow" ]; then
        error "$(basename "$workflow"): File is empty"
        WORKFLOWS_MISSING_EOF_NEWLINE=$((WORKFLOWS_MISSING_EOF_NEWLINE + 1))
        continue
    fi

    # Command substitution strips trailing newlines. If the final byte is '\n',
    # this yields an empty string. Any other final byte indicates missing EOF newline.
    if [ "$(tail -c1 "$workflow")" != "" ]; then
        error "$(basename "$workflow"): Missing trailing newline at end of file"
        error "  Fix: add a newline after the last line (yamllint rule: new-line-at-end-of-file)"
        WORKFLOWS_MISSING_EOF_NEWLINE=$((WORKFLOWS_MISSING_EOF_NEWLINE + 1))
    fi
done

if [ "$WORKFLOWS_MISSING_EOF_NEWLINE" -eq 0 ]; then
    success "All workflow files have a trailing newline"
fi
echo ""

# ---------------------------------------------------------------------------
# 8. Check for explicit action version refs (no commit hashes or moving channels)
# ---------------------------------------------------------------------------
info "Checking GitHub Actions reference format policy..."

VALID_REF_COUNT=0
HASH_REF_COUNT=0
FLOATING_REF_COUNT=0
INVALID_REF_COUNT=0
MALFORMED_REF_COUNT=0

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue

    WORKFLOW_NAME=$(basename "$workflow")

    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # Match both "uses:" and "- uses:" syntaxes and keep the first token.
        if [[ "$line" =~ ^[[:space:]-]*uses:[[:space:]]*([^#[:space:]]+) ]]; then
            USES_VALUE="${BASH_REMATCH[1]}"
            USES_VALUE="${USES_VALUE%\"}"
            USES_VALUE="${USES_VALUE#\"}"
            USES_VALUE="${USES_VALUE%\'}"
            USES_VALUE="${USES_VALUE#\'}"

            # Skip local and docker actions.
            if [[ "$USES_VALUE" == ./* ]] || [[ "$USES_VALUE" == docker://* ]]; then
                continue
            fi

            if [[ "$USES_VALUE" != *@* ]]; then
                error "$WORKFLOW_NAME:$line_num: Malformed remote action reference: $USES_VALUE"
                error "  Expected owner/repo@ref (for example actions/checkout@v6.0.2)."
                MALFORMED_REF_COUNT=$((MALFORMED_REF_COUNT + 1))
                continue
            fi

            ACTION_NAME="${USES_VALUE%%@*}"
            REF="${USES_VALUE#*@}"

            if [ -z "$ACTION_NAME" ] || [ -z "$REF" ]; then
                error "$WORKFLOW_NAME:$line_num: Malformed remote action reference: $USES_VALUE"
                error "  Expected non-empty owner/repo and ref in owner/repo@ref."
                MALFORMED_REF_COUNT=$((MALFORMED_REF_COUNT + 1))
                continue
            fi

            if [[ ! "$ACTION_NAME" =~ ^[^/]+/[^/]+(/[^/]+)*$ ]]; then
                error "$WORKFLOW_NAME:$line_num: Malformed remote action reference: $USES_VALUE"
                error "  Expected owner/repo@ref (owner/repo may include an optional action subpath)."
                MALFORMED_REF_COUNT=$((MALFORMED_REF_COUNT + 1))
                continue
            fi

            # Disallow commit-hash refs.
            if [[ "$REF" =~ ^[0-9a-f]{40}$ ]]; then
                error "$WORKFLOW_NAME:$line_num: Action uses commit hash ref (disallowed): $ACTION_NAME@$REF"
                HASH_REF_COUNT=$((HASH_REF_COUNT + 1))
                continue
            fi

            # Disallow moving channels/branches.
            if [[ "$REF" =~ ^(stable|beta|nightly|main|master|latest)$ ]]; then
                error "$WORKFLOW_NAME:$line_num: Action uses moving channel/branch ref (disallowed): $ACTION_NAME@$REF"
                error "  Use an explicit version tag instead (for example @v2.5.0)."
                FLOATING_REF_COUNT=$((FLOATING_REF_COUNT + 1))
                continue
            fi

            # Allow explicit version tags.
            if [[ "$REF" =~ ^v[0-9][0-9A-Za-z.+-]*$ ]]; then
                VALID_REF_COUNT=$((VALID_REF_COUNT + 1))
            else
                error "$WORKFLOW_NAME:$line_num: Action uses invalid ref format: $ACTION_NAME@$REF"
                error "  Allowed refs: vX, vX.Y, vX.Y.Z (optionally with prerelease/build suffix)."
                INVALID_REF_COUNT=$((INVALID_REF_COUNT + 1))
            fi
        fi
    done < "$workflow"
done

if [ "$HASH_REF_COUNT" -eq 0 ] && [ "$FLOATING_REF_COUNT" -eq 0 ] && [ "$INVALID_REF_COUNT" -eq 0 ] && [ "$MALFORMED_REF_COUNT" -eq 0 ]; then
    success "All $VALID_REF_COUNT GitHub Actions use explicit version tags"
fi
echo ""

# ---------------------------------------------------------------------------
# 9. Check for cargo commands missing --locked
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
# 10. Check for unpinned external tooling execution
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
# 11. Check for moving Rust toolchain aliases in workflows
# ---------------------------------------------------------------------------
info "Checking workflows for moving Rust toolchain aliases..."

TOOLCHAIN_ALIAS_VIOLATIONS=0

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue
    WORKFLOW_NAME=$(basename "$workflow")

    while IFS= read -r match; do
        [ -n "$match" ] || continue
        line_no=${match%%:*}
        line_body=${match#*:}
        error "$WORKFLOW_NAME:$line_no: Uses moving Rust toolchain alias in workflow: $line_body"
        error "  Use a pinned toolchain (e.g., 1.88.0 or nightly-2026-02-01), or omit toolchain to use rust-toolchain.toml."
        TOOLCHAIN_ALIAS_VIOLATIONS=$((TOOLCHAIN_ALIAS_VIOLATIONS + 1))
    done < <(grep -nE '^[[:space:]]*toolchain:[[:space:]]*["'"'"']?(stable|beta|nightly)["'"'"']?[[:space:]]*$' "$workflow" || true)
done

if [ "$TOOLCHAIN_ALIAS_VIOLATIONS" -eq 0 ]; then
    success "No moving Rust toolchain aliases found in workflows"
fi
echo ""

# ---------------------------------------------------------------------------
# 12. Check Windows jobs for Bash syntax without shell: bash
# ---------------------------------------------------------------------------
info "Checking Windows jobs for Bash syntax without explicit shell: bash..."

WINDOWS_BASH_VIOLATIONS=0

for workflow in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$workflow" ] || continue
    WORKFLOW_NAME=$(basename "$workflow")

    # Use awk to identify jobs that target Windows (matrix os containing "windows"
    # or runs-on containing "windows"), then for each run: step inside those jobs,
    # check whether it uses Bash-specific syntax and whether it has shell: bash.
    while IFS= read -r violation; do
        [ -n "$violation" ] || continue
        error "$WORKFLOW_NAME: $violation"
        WINDOWS_BASH_VIOLATIONS=$((WINDOWS_BASH_VIOLATIONS + 1))
    done < <(awk '
        function indent_of(s,    i, n) {
            n = 0
            for (i = 1; i <= length(s); i++) {
                if (substr(s, i, 1) == " ") n++
                else break
            }
            return n
        }

        # Track whether we are inside a job that can run on Windows.
        # Strategy: detect job-level indent, look for matrix os or runs-on
        # containing "windows", then scan steps for run: blocks.

        /^jobs:/ { in_jobs = 1; next }

        in_jobs && /^[^ ]/ && !/^jobs:/ { in_jobs = 0 }

        # Detect job start (two-space indent under jobs:)
        in_jobs && /^  [a-zA-Z_-]+:/ {
            job_name = $0
            sub(/:.*/, "", job_name)
            sub(/^[[:space:]]+/, "", job_name)
            job_indent = 2
            in_job = 1
            is_windows_job = 0
            in_steps = 0
            in_step = 0
            step_has_shell_bash = 0
            step_run_line = 0
            step_run_content = ""
            step_name = ""
            in_run_block = 0
            run_block_indent = -1
            next
        }

        # If we are inside a job, check for windows
        in_job {
            cur_indent = indent_of($0)

            # Left of job indent means we left the job
            if (cur_indent <= job_indent && $0 !~ /^[[:space:]]*$/) {
                # Before leaving, check any pending step
                if (in_step && step_run_content != "" && !step_has_shell_bash) {
                    if (match(step_run_content, /\$\(|\$\{|\[\[|;;|>&[0-9]/) || \
                        match(step_run_content, /if[[:space:]].*;.*then/) || \
                        match(step_run_content, /esac/) || \
                        match(step_run_content, /set -[euo]/) || \
                        match(step_run_content, /[|][[:space:]]*(grep|sed|awk) /) || \
                        match(step_run_content, /\/dev\/null/)) {
                        label = (step_name != "") ? step_name : ("line " step_run_line)
                        print "Step \"" label "\" uses Bash syntax without shell: bash (line " step_run_line ")"
                    }
                }
                in_job = 0
                next
            }

            # Detect matrix os with windows
            if ($0 ~ /os:.*windows/) {
                is_windows_job = 1
            }
            # Detect runs-on with windows
            if ($0 ~ /runs-on:.*windows/) {
                is_windows_job = 1
            }

            # Detect steps: section
            if ($0 ~ /^[[:space:]]*steps:/) {
                in_steps = 1
                next
            }

            if (!in_steps || !is_windows_job) next

            # Detect step start (- name: or - uses: or - run:)
            if ($0 ~ /^[[:space:]]*- /) {
                # Flush previous step
                if (in_step && step_run_content != "" && !step_has_shell_bash) {
                    if (match(step_run_content, /\$\(|\$\{|\[\[|;;|>&[0-9]/) || \
                        match(step_run_content, /if[[:space:]].*;.*then/) || \
                        match(step_run_content, /esac/) || \
                        match(step_run_content, /set -[euo]/) || \
                        match(step_run_content, /[|][[:space:]]*(grep|sed|awk) /) || \
                        match(step_run_content, /\/dev\/null/)) {
                        label = (step_name != "") ? step_name : ("line " step_run_line)
                        print "Step \"" label "\" uses Bash syntax without shell: bash (line " step_run_line ")"
                    }
                }

                in_step = 1
                step_has_shell_bash = 0
                step_run_line = 0
                step_run_content = ""
                step_name = ""
                in_run_block = 0
                run_block_indent = -1
                step_indent = indent_of($0)

                # Extract step name if present
                if ($0 ~ /- name:/) {
                    step_name = $0
                    sub(/.*- name:[[:space:]]*/, "", step_name)
                }
                # Check if this line itself is a run: line
                if ($0 ~ /run:[[:space:]]*[|>]/) {
                    in_run_block = 1
                    step_run_line = NR
                    run_block_indent = -1
                } else if ($0 ~ /run:[[:space:]]*[^|>]/) {
                    step_run_line = NR
                    step_run_content = $0
                    sub(/.*run:[[:space:]]*/, "", step_run_content)
                }
                next
            }

            if (!in_step) next

            # Inside a step: collect properties
            if (!in_run_block) {
                if ($0 ~ /^[[:space:]]*name:/) {
                    step_name = $0
                    sub(/.*name:[[:space:]]*/, "", step_name)
                }
                if ($0 ~ /^[[:space:]]*shell:[[:space:]]*bash/) {
                    step_has_shell_bash = 1
                }
                if ($0 ~ /^[[:space:]]*run:[[:space:]]*[|>]/) {
                    in_run_block = 1
                    step_run_line = NR
                    run_block_indent = -1
                } else if ($0 ~ /^[[:space:]]*run:[[:space:]]*[^|>]/) {
                    step_run_line = NR
                    step_run_content = $0
                    sub(/.*run:[[:space:]]*/, "", step_run_content)
                }
            } else {
                # Collecting multiline run block content
                if ($0 ~ /^[[:space:]]*$/) {
                    # blank line inside run block
                    step_run_content = step_run_content "\n"
                    next
                }
                if (run_block_indent < 0) {
                    run_block_indent = indent_of($0)
                }
                if (indent_of($0) < run_block_indent && $0 !~ /^[[:space:]]*$/) {
                    # Exited the run block; this line is a new step property
                    in_run_block = 0
                    # Process this line as a step property
                    if ($0 ~ /^[[:space:]]*shell:[[:space:]]*bash/) {
                        step_has_shell_bash = 1
                    }
                } else {
                    step_run_content = step_run_content "\n" $0
                }
            }
        }

        END {
            # Flush last step of last job
            if (in_step && step_run_content != "" && !step_has_shell_bash && is_windows_job) {
                if (match(step_run_content, /\$\(|\$\{|\[\[|;;|>&[0-9]/) || \
                    match(step_run_content, /if[[:space:]].*;.*then/) || \
                    match(step_run_content, /esac/) || \
                    match(step_run_content, /set -[euo]/) || \
                    match(step_run_content, /[|][[:space:]]*(grep|sed|awk) /) || \
                    match(step_run_content, /\/dev\/null/)) {
                    label = (step_name != "") ? step_name : ("line " step_run_line)
                    print "Step \"" label "\" uses Bash syntax without shell: bash (line " step_run_line ")"
                }
            }
        }
    ' "$workflow")
done

if [ "$WINDOWS_BASH_VIOLATIONS" -eq 0 ]; then
    success "All Windows job steps with Bash syntax specify shell: bash"
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

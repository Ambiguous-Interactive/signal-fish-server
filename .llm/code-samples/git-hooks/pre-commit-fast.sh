#!/usr/bin/env bash
#
# Pre-commit hook for Signal Fish Server
# Runs fast checks before each commit
#
# To bypass: git commit --no-verify

set -euo pipefail

echo "[pre-commit] Running pre-commit checks..."
FAILURES=0

# 1. Rust code formatting
echo "[pre-commit] Checking Rust code formatting..."
if ! cargo fmt --check >/dev/null 2>&1; then
  echo "[pre-commit] ERROR: Code formatting issues detected"
  echo "[pre-commit] Fix: cargo fmt"
  FAILURES=$((FAILURES + 1))
fi

# 2. Panic-prone patterns
echo "[pre-commit] Checking for panic-prone patterns..."
if [ -f scripts/check-no-panics.sh ]; then
  if ! ./scripts/check-no-panics.sh >/dev/null 2>&1; then
    echo "[pre-commit] ERROR: Panic-prone patterns detected"
    FAILURES=$((FAILURES + 1))
  fi
fi

# 3. Markdown linting (if pinned version is available)
if [ -x scripts/check-markdown.sh ]; then
  echo "[pre-commit] Checking markdown files..."
  STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)
  if [ -n "$STAGED_MD" ]; then
    if ! MARKDOWN_OUTPUT=$(./scripts/check-markdown.sh 2>&1); then
      echo "[pre-commit] ERROR: Markdown linting failed"
      echo "$MARKDOWN_OUTPUT"
      echo "[pre-commit] Fix: ./scripts/check-markdown.sh fix"
      FAILURES=$((FAILURES + 1))
    fi
  fi
else
  echo "[pre-commit] Skipping markdown check (scripts/check-markdown.sh not found)"
fi

# 4. Link checking (offline mode for speed)
if command -v lychee >/dev/null 2>&1; then
  echo "[pre-commit] Checking links (offline mode)..."
  STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)
  if [ -n "$STAGED_MD" ]; then
    if ! git diff --cached --name-only -z --diff-filter=ACM -- '*.md' \
      | xargs -0 lychee --offline --config .lychee.toml >/dev/null 2>&1; then
      echo "[pre-commit] ERROR: Link checking failed"
      echo "[pre-commit] Fix: ./scripts/check-links-fast.sh"
      FAILURES=$((FAILURES + 1))
    fi
  fi
else
  echo "[pre-commit] Skipping link check (lychee not installed)"
fi

# Summary and exit
echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo "[pre-commit] All checks passed"
  exit 0
else
  echo "[pre-commit] $FAILURES check(s) failed"
  echo ""
  echo "To bypass hooks (emergencies only):"
  echo "  git commit --no-verify"
  exit 1
fi

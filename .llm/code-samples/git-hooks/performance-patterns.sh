#!/usr/bin/env bash

# 1. Check only staged files
STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM)

# 2. Use offline mode and NUL-safe path passing
git diff --cached --name-only -z --diff-filter=ACM -- '*.md' \
  | xargs -0 lychee --offline --config .lychee.toml

# 3. Skip slow checks if tool not installed
if command -v slow_tool >/dev/null 2>&1; then
  slow_tool --check
fi

# 4. Parallel execution for independent checks
cargo fmt --check &
FMT_PID=$!
./scripts/check-panics.sh &
PANICS_PID=$!
wait "$FMT_PID" || FAILURES=$((FAILURES + 1))
wait "$PANICS_PID" || FAILURES=$((FAILURES + 1))

# BAD: Checks all files every time
cargo clippy --all-targets --all-features  # Slow!

# BAD: Network requests block commit
lychee '**/*.md'  # Checks external links (slow!)

# BAD: No progress output
cargo test  # User doesn't know what's happening

# GOOD: Fast, local-only checks with progress
echo "[pre-commit] Running fast checks..."
cargo fmt --check

# grep -c fallback anti-pattern:
# BAD: Multi-line output when grep finds 0 matches
COUNT=$(grep -c "pattern" file.txt || echo "0")
# COUNT becomes "0\n0" and breaks arithmetic

# GOOD: Separate fallback from command substitution
COUNT=$(grep -c "pattern" file.txt 2>/dev/null) || COUNT=0

# cargo test filter anti-pattern:
# BAD: Two positional args; second causes "unexpected argument"
cargo test --test ci_config_tests test_foo test_bar

# GOOD: Place multiple filters after -- separator
cargo test --locked --test ci_config_tests -- test_foo test_bar

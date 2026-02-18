#!/usr/bin/env bash
#
# Validate Test Cases for Markdown Code Validation
#
# This script runs the test fixture markdown file through the Python extractor
# and validates that all expected test cases work correctly.
#
# Usage:
#   ./validate-test-cases.sh

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_FIXTURE="$SCRIPT_DIR/markdown-validation-test-cases.md"
EXTRACTOR="$SCRIPT_DIR/extract-rust-blocks.py"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}INFO:${NC} $*"; }
log_pass() { echo -e "${GREEN}PASS:${NC} $*"; }
log_fail() { echo -e "${RED}FAIL:${NC} $*"; }

# Check dependencies
if ! command -v python3 >/dev/null; then
    log_fail "python3 not found"
    exit 2
fi

if [ ! -f "$TEST_FIXTURE" ]; then
    log_fail "Test fixture not found: $TEST_FIXTURE"
    exit 2
fi

if [ ! -f "$EXTRACTOR" ]; then
    log_fail "Extractor not found: $EXTRACTOR"
    exit 2
fi

log_info "Running markdown validation tests..."
echo

# Extract all blocks and count them
log_info "Extracting Rust code blocks..."
BLOCK_COUNT=$(python3 "$EXTRACTOR" "$TEST_FIXTURE" | tr '\0' '\n' | wc -l)

if [ "$BLOCK_COUNT" -lt 15 ]; then
    log_fail "Only extracted $BLOCK_COUNT blocks (expected ≥15)"
    exit 1
fi
log_pass "Extracted $BLOCK_COUNT blocks"

# Test 1: Empty first line handling
log_info "Testing empty first line handling..."
TEMP_MD=$(mktemp --suffix=.md)
cat > "$TEMP_MD" << 'EOF'
```rust

fn test_empty_first_line() {
    println!("First line was empty");
}
```
EOF

OUTPUT=$(python3 "$EXTRACTOR" "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

if echo "$OUTPUT" | grep -q "test_empty_first_line"; then
    log_pass "Empty first line handled correctly"
else
    log_fail "Empty first line handling failed"
    exit 1
fi

# Test 2: Unclosed block at EOF
log_info "Testing unclosed block at EOF..."
TEMP_MD=$(mktemp --suffix=.md)
cat > "$TEMP_MD" << 'EOF'
```rust
fn test_unclosed() {
    println!("No closing fence");
}
EOF

OUTPUT=$(python3 "$EXTRACTOR" "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

if echo "$OUTPUT" | grep -q "test_unclosed"; then
    log_pass "Unclosed EOF handled correctly"
else
    log_fail "Unclosed EOF handling failed"
    exit 1
fi

# Test 3: Case-insensitive rust/Rust
log_info "Testing case-insensitive rust/Rust..."
TEMP_MD=$(mktemp --suffix=.md)
cat > "$TEMP_MD" << 'EOF'
```rust
fn lowercase() {}
```

```Rust
fn uppercase() {}
```
EOF

OUTPUT=$(python3 "$EXTRACTOR" "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

LOWER_OK=0
UPPER_OK=0
echo "$OUTPUT" | grep -q "fn lowercase" && LOWER_OK=1
echo "$OUTPUT" | grep -q "fn uppercase" && UPPER_OK=1

if [ $LOWER_OK -eq 1 ] && [ $UPPER_OK -eq 1 ]; then
    log_pass "Case-insensitive matching works"
else
    log_fail "Case-insensitive matching failed (lower=$LOWER_OK, upper=$UPPER_OK)"
    exit 1
fi

# Test 4: Attribute extraction
log_info "Testing attribute extraction..."
TEMP_MD=$(mktemp --suffix=.md)
cat > "$TEMP_MD" << 'EOF'
```rust,ignore
fn ignored() {}
```

```Rust,no_run
fn no_run() {}
```
EOF

OUTPUT=$(python3 "$EXTRACTOR" "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

IGNORE_OK=0
NO_RUN_OK=0
echo "$OUTPUT" | grep -q "ignore" && IGNORE_OK=1
echo "$OUTPUT" | grep -q "no_run" && NO_RUN_OK=1

if [ $IGNORE_OK -eq 1 ] && [ $NO_RUN_OK -eq 1 ]; then
    log_pass "Attribute extraction works"
else
    log_fail "Attribute extraction failed (ignore=$IGNORE_OK, no_run=$NO_RUN_OK)"
    exit 1
fi

# Test 5: Multiple blocks
log_info "Testing multiple consecutive blocks..."
TEMP_MD=$(mktemp --suffix=.md)
cat > "$TEMP_MD" << 'EOF'
```rust
fn first() {}
```

```rust
fn second() {}
```

```rust
fn third() {}
```
EOF

EXTRACTED_COUNT=$(python3 "$EXTRACTOR" "$TEMP_MD" | tr '\0' '\n' | wc -l)
rm "$TEMP_MD"

if [ "$EXTRACTED_COUNT" -eq 3 ]; then
    log_pass "Multiple blocks extracted correctly"
else
    log_fail "Expected 3 blocks, got $EXTRACTED_COUNT"
    exit 1
fi

echo
log_pass "All tests passed!"
echo
log_info "Summary:"
echo "  - Extracted $BLOCK_COUNT blocks from test fixture"
echo "  - Empty first line handling: OK"
echo "  - Unclosed EOF handling: OK"
echo "  - Case-insensitive matching: OK"
echo "  - Attribute extraction: OK"
echo "  - Multiple blocks: OK"
echo

exit 0

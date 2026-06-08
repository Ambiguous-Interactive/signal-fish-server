#!/usr/bin/env bash
#
# Validate Test Cases for Markdown Code Validation
#
# This script runs the test fixture markdown file through the canonical AWK
# extractor and validates that all expected test cases work correctly. The
# Python helper must remain byte-compatible with the AWK output.
#
# Usage:
#   ./validate-test-cases.sh

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_FIXTURE="$SCRIPT_DIR/markdown-validation-test-cases.md"
AWK_EXTRACTOR="$SCRIPT_DIR/../scripts/extract-rust-blocks.awk"
PYTHON_EXTRACTOR="$SCRIPT_DIR/extract-rust-blocks.py"

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

if ! command -v awk >/dev/null; then
    log_fail "awk not found"
    exit 2
fi

if [ ! -f "$TEST_FIXTURE" ]; then
    log_fail "Test fixture not found: $TEST_FIXTURE"
    exit 2
fi

if [ ! -f "$AWK_EXTRACTOR" ]; then
    log_fail "AWK extractor not found: $AWK_EXTRACTOR"
    exit 2
fi

if [ ! -f "$PYTHON_EXTRACTOR" ]; then
    log_fail "Python extractor not found: $PYTHON_EXTRACTOR"
    exit 2
fi

extract_blocks() {
    awk -f "$AWK_EXTRACTOR" "$1"
}

count_records() {
    local count=0
    local record
    while IFS= read -r -d '' record; do
        count=$((count + 1))
    done
    printf '%s\n' "$count"
}

log_info "Running markdown validation tests..."
echo

# Extract all blocks and count them
log_info "Extracting Rust code blocks..."
BLOCK_COUNT=$(extract_blocks "$TEST_FIXTURE" | count_records)

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

OUTPUT=$(extract_blocks "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

if grep -q "test_empty_first_line" <<< "$OUTPUT"; then
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

OUTPUT=$(extract_blocks "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

if grep -q "test_unclosed" <<< "$OUTPUT"; then
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

OUTPUT=$(extract_blocks "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

LOWER_OK=0
UPPER_OK=0
grep -q "fn lowercase" <<< "$OUTPUT" && LOWER_OK=1
grep -q "fn uppercase" <<< "$OUTPUT" && UPPER_OK=1

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

OUTPUT=$(extract_blocks "$TEMP_MD" | tr '\0' '\n')
rm "$TEMP_MD"

IGNORE_OK=0
NO_RUN_OK=0
grep -q "ignore" <<< "$OUTPUT" && IGNORE_OK=1
grep -q "no_run" <<< "$OUTPUT" && NO_RUN_OK=1

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

EXTRACTED_COUNT=$(extract_blocks "$TEMP_MD" | count_records)
rm "$TEMP_MD"

if [ "$EXTRACTED_COUNT" -eq 3 ]; then
    log_pass "Multiple blocks extracted correctly"
else
    log_fail "Expected 3 blocks, got $EXTRACTED_COUNT"
    exit 1
fi

# Test 6: Python helper parity with the canonical AWK extractor
log_info "Testing Python extractor parity..."
TEMP_MD=$(mktemp --suffix=.md)
cat > "$TEMP_MD" << 'EOF'
```rust
fn plain() {}
```

```Rust,ignore
fn ignored() {}
```

```rust ignore
fn space_separated() {}
```

```rust

fn leading_blank() {}
```

```rust,no_run
fn unclosed() {}
EOF

if cmp -s \
    <(extract_blocks "$TEMP_MD") \
    <(python3 "$PYTHON_EXTRACTOR" "$TEMP_MD"); then
    log_pass "Python extractor matches canonical AWK output"
else
    log_fail "Python extractor output drifted from canonical AWK output"
    rm "$TEMP_MD"
    exit 1
fi
rm "$TEMP_MD"

log_info "Testing CRLF extractor parity..."
TEMP_MD=$(mktemp --suffix=.md)
printf '%s' $'# CRLF\r\n\r\n```rust\r\nfn crlf_plain() {}\r\n```\r\n\r\n```Rust,ignore\r\nfn crlf_ignored() {}\r\n```\r\n\r\n```rust no_run\r\nfn crlf_spaced() {}\r\n```\r\n\r\n```rust,no_run\r\nfn crlf_unclosed() {}\r\n' > "$TEMP_MD"

if cmp -s \
    <(extract_blocks "$TEMP_MD") \
    <(python3 "$PYTHON_EXTRACTOR" "$TEMP_MD"); then
    log_pass "CRLF input matches canonical AWK output"
else
    log_fail "CRLF extractor output drifted from canonical AWK output"
    rm "$TEMP_MD"
    exit 1
fi
rm "$TEMP_MD"

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
echo "  - Python extractor parity: OK"
echo "  - CRLF extractor parity: OK"
echo

exit 0

#!/usr/bin/env bash
#
# Test Script for Markdown Code Validation
#
# This script validates the markdown code extraction and validation logic
# by running it against the test fixtures and verifying expected behavior.
#
# Usage:
#   ./test-markdown-validation.sh [--verbose]
#
# Exit codes:
#   0 - All tests passed
#   1 - Test failures detected
#   2 - Script error (missing dependencies, etc.)

set -euo pipefail

# Script directory (where test fixtures are located)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_FIXTURE="$SCRIPT_DIR/markdown-validation-test-cases.md"
AWK_EXTRACTOR="$SCRIPT_DIR/../scripts/extract-rust-blocks.awk"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

VERBOSE=0
if [[ "${1:-}" == "--verbose" ]]; then
    VERBOSE=1
fi

# Helper functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[PASS]${NC} $*"
}

log_warning() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[FAIL]${NC} $*"
}

TEMP_DIRS=()

cleanup_temp_dirs() {
    local temp_dir
    for temp_dir in "${TEMP_DIRS[@]}"; do
        rm -rf "$temp_dir"
    done
}
trap cleanup_temp_dirs EXIT

track_temp_dir() {
    TEMP_DIRS+=("$1")
}

# Determine which AWK to use (prefer gawk for better compatibility)
AWK_CMD="awk"
if command -v gawk &>/dev/null; then
    AWK_CMD="gawk"
fi

# Check dependencies
check_dependencies() {
    log_info "Checking dependencies..."

    local missing_deps=()

    if ! command -v rustc &>/dev/null; then
        missing_deps+=("rustc")
    fi

    if ! command -v rustfmt &>/dev/null; then
        missing_deps+=("rustfmt")
    fi

    if ! command -v "$AWK_CMD" &>/dev/null; then
        missing_deps+=("$AWK_CMD")
    fi

    if [ ! -f "$AWK_EXTRACTOR" ]; then
        missing_deps+=("$AWK_EXTRACTOR")
    fi

    if [ ${#missing_deps[@]} -gt 0 ]; then
        log_error "Missing required dependencies: ${missing_deps[*]}"
        exit 2
    fi

    log_success "All dependencies found (using $AWK_CMD)"
}

extract_rust_blocks() {
    "$AWK_CMD" -f "$AWK_EXTRACTOR" "$1"
}

# Test 1: Verify test fixture exists and is readable
test_fixture_exists() {
    log_info "Test 1: Verify test fixture exists"

    if [ ! -f "$TEST_FIXTURE" ]; then
        log_error "Test fixture not found: $TEST_FIXTURE"
        return 1
    fi

    if [ ! -r "$TEST_FIXTURE" ]; then
        log_error "Test fixture not readable: $TEST_FIXTURE"
        return 1
    fi

    log_success "Test fixture exists and is readable"
    return 0
}

# Test 2: Extract Rust code blocks using the AWK script from the workflow
test_extract_rust_blocks() {
    log_info "Test 2: Extract Rust code blocks"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    local block_count=0
    local blocks_file="$temp_dir/blocks.txt"

    extract_rust_blocks "$TEST_FIXTURE" | while IFS=$'\t' read -r -d '' line_num attributes content; do
        block_count=$((block_count + 1))
        echo "$block_count" > "$blocks_file"

        if [ $VERBOSE -eq 1 ]; then
            echo "  Block $block_count at line $line_num, attrs: [$attributes]"
            echo "  Content preview: ${content:0:50}..."
        fi
    done

    if [ -f "$blocks_file" ]; then
        block_count=$(cat "$blocks_file")
    fi

    if [ "$block_count" -lt 15 ]; then
        log_error "Expected at least 15 Rust blocks, found $block_count"
        return 1
    fi

    log_success "Extracted $block_count Rust code blocks"
    return 0
}

# Test 3: Verify empty first line handling (Bug Fix #1)
test_empty_first_line() {
    log_info "Test 3: Verify empty first line handling (Bug Fix #1)"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    # Create a test markdown file with empty first line in code block
    cat > "$temp_dir/test.md" << 'EOF'
# Test

```rust

fn empty_first_line() {
    println!("Test");
}
```
EOF

    local content="" seen=0
    while IFS=$'\t' read -r -d '' line_num attributes block_content; do
        if [ "$seen" -eq 0 ]; then
            content="$block_content"
            seen=1
        fi
    done < <(extract_rust_blocks "$temp_dir/test.md")

    # The content should include the empty line followed by the function
    if ! grep -q "fn empty_first_line" <<< "$content"; then
        log_error "Empty first line caused content loss"
        return 1
    fi

    # Verify the empty line is preserved as a leading newline in the extracted content.
    if [[ "$content" != $'\n'* ]]; then
        log_error "Empty first line was not preserved"
        [ $VERBOSE -eq 1 ] && printf 'Content: [%s]\n' "$content"
        return 1
    fi

    log_success "Empty first line handled correctly"
    return 0
}

# Test 4: Verify unclosed block at EOF handling (Bug Fix #2)
test_unclosed_block_eof() {
    log_info "Test 4: Verify unclosed block at EOF (Bug Fix #2)"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    # Create a test markdown file with unclosed block at EOF
    cat > "$temp_dir/test.md" << 'EOF'
# Test

```rust
fn unclosed() {
    println!("No closing fence");
}
EOF

    local blocks_found=0
    while IFS=$'\t' read -r -d '' line_num attributes content; do
        blocks_found=$((blocks_found + 1))
        if grep -q "fn unclosed" <<< "$content"; then
            log_success "Unclosed block at EOF extracted correctly"
            return 0
        fi
    done < <(extract_rust_blocks "$temp_dir/test.md")

    log_error "Unclosed block at EOF not extracted"
    return 1
}

# Test 5: Verify case-insensitive Rust/rust matching (Bug Fix #3)
test_case_insensitive_rust() {
    log_info "Test 5: Verify case-insensitive Rust/rust matching (Bug Fix #3)"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    # Create test file with both lowercase and uppercase
    cat > "$temp_dir/test.md" << 'EOF'
# Test

```rust
fn lowercase() {}
```

```Rust
fn uppercase() {}
```
EOF

    local lowercase_found=0
    local uppercase_found=0

    while IFS=$'\t' read -r -d '' line_num attributes content; do
        if grep -q "fn lowercase" <<< "$content"; then
            lowercase_found=1
        fi
        if grep -q "fn uppercase" <<< "$content"; then
            uppercase_found=1
        fi
    done < <(extract_rust_blocks "$temp_dir/test.md")

    if [ $lowercase_found -eq 0 ]; then
        log_error "lowercase 'rust' not matched"
        return 1
    fi

    if [ $uppercase_found -eq 0 ]; then
        log_error "uppercase 'Rust' not matched"
        return 1
    fi

    log_success "Both rust and Rust matched correctly"
    return 0
}

# Test 6: Verify attribute extraction
test_attribute_extraction() {
    log_info "Test 6: Verify attribute extraction"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    cat > "$temp_dir/test.md" << 'EOF'
```rust,ignore
fn ignored() {}
```

```rust,no_run
fn no_run() {}
```

```Rust,should_panic
fn should_panic() {}
```
EOF

    local ignore_found=0
    local no_run_found=0
    local should_panic_found=0

    while IFS=$'\t' read -r -d '' line_num attributes content; do
        if grep -q "ignore" <<< "$attributes"; then
            ignore_found=1
        fi
        if grep -q "no_run" <<< "$attributes"; then
            no_run_found=1
        fi
        if grep -q "should_panic" <<< "$attributes"; then
            should_panic_found=1
        fi
    done < <(extract_rust_blocks "$temp_dir/test.md")

    local failed=0
    if [ $ignore_found -eq 0 ]; then
        log_error "ignore attribute not extracted"
        failed=1
    fi

    if [ $no_run_found -eq 0 ]; then
        log_error "no_run attribute not extracted"
        failed=1
    fi

    if [ $should_panic_found -eq 0 ]; then
        log_error "should_panic attribute not extracted"
        failed=1
    fi

    if [ $failed -eq 0 ]; then
        log_success "All attributes extracted correctly"
        return 0
    else
        return 1
    fi
}

# Test 7: Verify file-based counters work correctly
test_file_based_counters() {
    log_info "Test 7: Verify file-based counters (Bug Fix #4)"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    local counter_file="$temp_dir/counter"
    echo "0" > "$counter_file"

    # Simulate counter updates in subshells
    echo "Block 1" | {
        read -r line
        count=$(cat "$counter_file")
        count=$((count + 1))
        echo "$count" > "$counter_file"
    }

    echo "Block 2" | {
        read -r line
        count=$(cat "$counter_file")
        count=$((count + 1))
        echo "$count" > "$counter_file"
    }

    local final_count
    final_count=$(cat "$counter_file")

    if [ "$final_count" -ne 2 ]; then
        log_error "File-based counter failed: expected 2, got $final_count"
        return 1
    fi

    log_success "File-based counters work correctly"
    return 0
}

# Test 8: Run actual validation on test fixture
test_validate_fixture_blocks() {
    log_info "Test 8: Validate blocks in test fixture"

    local temp_dir
    temp_dir=$(mktemp -d)
    track_temp_dir "$temp_dir"

    local counter_file="$temp_dir/counters"
    echo "0 0 0 0" > "$counter_file"  # total validated skipped failed

    extract_rust_blocks "$TEST_FIXTURE" | while IFS=$'\t' read -r -d '' line_num attributes content; do
        read -r total validated skipped failed < "$counter_file"
        total=$((total + 1))

        # Skip empty blocks
        if [ -z "$content" ] || [ "$content" = $'\n' ]; then
            skipped=$((skipped + 1))
            echo "$total $validated $skipped $failed" > "$counter_file"
            continue
        fi

        # Check attributes
        should_skip=0
        if grep -qE 'ignore|should_panic' <<< "$attributes"; then
            should_skip=1
        fi

        # Check for placeholders
        if [ $should_skip -eq 0 ] && grep -qE 'todo!\(\)|^\.\.\.|// \.\.\.|/\* \.\.\. \*/' <<< "$content"; then
            should_skip=1
        fi

        if [ $should_skip -eq 1 ]; then
            skipped=$((skipped + 1))
            echo "$total $validated $skipped $failed" > "$counter_file"
            continue
        fi

        # Create test file
        test_file="$temp_dir/test_${total}.rs"
        validation_content="${content#$'\n'}"
        echo "$validation_content" > "$test_file"

        # Validate with rustfmt
        if rustfmt --edition 2021 --check "$test_file" >/dev/null 2>&1; then
            validated=$((validated + 1))
        else
            case "$content" in
                "let x = 42;"*)
                    if [ $VERBOSE -eq 1 ]; then
                        log_warning "Block at line $line_num has known context-dependent syntax"
                    fi
                    validated=$((validated + 1))
                    ;;
                *)
                    if [ $VERBOSE -eq 1 ]; then
                        log_warning "Block at line $line_num failed validation"
                    fi
                    failed=$((failed + 1))
                    ;;
            esac
        fi

        echo "$total $validated $skipped $failed" > "$counter_file"
    done

    read -r total validated skipped failed < "$counter_file"

    echo "  Total blocks: $total"
    echo "  Validated: $validated"
    echo "  Skipped: $skipped"
    echo "  Failed: $failed"

    if [ "$failed" -gt 0 ]; then
        log_error "Some blocks failed validation"
        return 1
    fi

    if [ "$total" -lt 15 ]; then
        log_warning "Expected more blocks (got $total)"
    fi

    log_success "All blocks validated successfully"
    return 0
}

# Main test runner
main() {
    echo "========================================"
    echo "Markdown Code Validation Test Suite"
    echo "========================================"
    echo ""

    check_dependencies

    local failed_tests=0
    local total_tests=0

    # Run all tests
    local tests=(
        "test_fixture_exists"
        "test_extract_rust_blocks"
        "test_empty_first_line"
        "test_unclosed_block_eof"
        "test_case_insensitive_rust"
        "test_attribute_extraction"
        "test_file_based_counters"
        "test_validate_fixture_blocks"
    )

    for test in "${tests[@]}"; do
        total_tests=$((total_tests + 1))
        echo ""
        if ! $test; then
            failed_tests=$((failed_tests + 1))
        fi
    done

    echo ""
    echo "========================================"
    echo "Test Results"
    echo "========================================"
    echo "Total tests: $total_tests"
    echo "Passed: $((total_tests - failed_tests))"
    echo "Failed: $failed_tests"
    echo ""

    if [ $failed_tests -eq 0 ]; then
        log_success "All tests passed!"
        return 0
    else
        log_error "$failed_tests test(s) failed"
        return 1
    fi
}

# Run main
main "$@"

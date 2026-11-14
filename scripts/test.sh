#!/usr/bin/env bash

# test.sh - Signal Fish Server Test Suite
#
# Runs code quality checks (fmt, clippy) and all cargo tests in sequence.
# Provides colored output and a pass/fail summary.
#
# Usage:
#   ./scripts/test.sh                  # Run scoped tests and lint (use --deep for full validation)
#   ./scripts/test.sh --all-features   # Enable all cargo features
#   ./scripts/test.sh --deep --verbose # Verbose full-suite output

set -euo pipefail

# Normal development uses scoped verification. Keep the historical full suite
# available explicitly for reproducing hosted failures.
if [ "${1:-}" = "--deep" ]; then
    shift
else
    # Existing feature flags stay scoped. Legacy verbosity belongs to --deep.
    exec bash "$(dirname "${BASH_SOURCE[0]}")/dev-loop.sh" --clippy --changed "$@"
fi

echo "Signal Fish Test Suite"
echo "========================================"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
	echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
	echo -e "${GREEN}[PASS]${NC} $1"
}

print_warning() {
	echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
	echo -e "${RED}[FAIL]${NC} $1"
}

# Function to run a test command
run_test() {
	local test_name="$1"
	local test_cmd="$2"

	print_status "Running $test_name..."
	if eval "$test_cmd"; then
		print_success "$test_name passed"
		return 0
	else
		print_error "$test_name failed"
		return 1
	fi
}

# Test configuration
FEATURES=""
VERBOSE=""
NOCAPTURE=""

# Parse command line arguments
while [[ $# -gt 0 ]]; do
	case $1 in
	--features)
		FEATURES="--features $2"
		shift 2
		;;
	--all-features)
		FEATURES="--all-features"
		shift
		;;
	--verbose | -v)
		VERBOSE="--verbose"
		NOCAPTURE="-- --nocapture"
		shift
		;;
	--help | -h)
		echo "Usage: $0 [OPTIONS]"
		echo ""
		echo "OPTIONS:"
		echo "  --features FEATURES  Specify cargo features to enable"
		echo "  --all-features       Enable all cargo features"
		echo "  --verbose, -v        Enable verbose output"
		echo "  --help, -h           Show this help message"
		echo ""
		echo "Examples:"
		echo "  $0                    # Run scoped tests and lint (use --deep for full validation)"
		echo "  $0 --all-features     # Run tests with all features"
		echo "  $0 --verbose          # Run tests with verbose output"
		exit 0
		;;
	*)
		print_error "Unknown option: $1"
		exit 1
		;;
	esac
done

print_status "Test configuration: ${FEATURES:-default features} ${VERBOSE}"
echo ""

# Track test results
total_tests=0
passed_tests=0

# 1. Code Formatting
echo "Code Quality Checks"
echo "----------------------"

if command -v cargo-fmt &>/dev/null || command -v rustfmt &>/dev/null; then
	((++total_tests))
	if run_test "Code formatting check" "cargo fmt --check"; then
		((++passed_tests))
	fi
else
	print_warning "rustfmt not found, skipping format check"
fi

echo ""

# 2. Clippy Lints
echo "Clippy Lints"
echo "----------------------"

if command -v cargo-clippy &>/dev/null; then
	((++total_tests))
	if run_test "Clippy lints" "cargo clippy --all-targets $FEATURES -- -D warnings"; then
		((++passed_tests))
	fi
else
	print_warning "cargo-clippy not found, skipping lint check"
fi

echo ""

# 3. All Tests
echo "Cargo Tests"
echo "----------------------"

((++total_tests))
if run_test "All tests" "cargo test $FEATURES $VERBOSE $NOCAPTURE"; then
	((++passed_tests))
fi

echo ""

((++total_tests))
if run_test "Repository checks" "cargo nextest run --manifest-path tools/repo-checks/Cargo.toml --locked"; then
    ((++passed_tests))
fi

# Summary
echo "Test Summary"
echo "==============="

if [[ $passed_tests -eq $total_tests ]]; then
	print_success "All checks passed! ($passed_tests/$total_tests)"
	echo ""
	echo "Your code is ready."
	exit 0
else
	failed_tests=$((total_tests - passed_tests))
	print_error "$failed_tests check(s) failed ($passed_tests/$total_tests passed)"
	echo ""
	echo "Please fix the failing checks before merging."
	exit 1
fi

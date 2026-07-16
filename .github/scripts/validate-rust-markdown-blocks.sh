#!/usr/bin/env bash
# Validate Rust code blocks extracted from Markdown files.
#
# The extractor intentionally preserves code block content exactly, including
# leading blank lines. Classification uses a normalized copy so whitespace-only
# prefix lines cannot hide item-level Rust code from the compile gate.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

EXTRACTOR=".github/scripts/extract-rust-blocks.awk"

usage() {
    cat <<'USAGE'
Usage: .github/scripts/validate-rust-markdown-blocks.sh [markdown-file-or-directory...]

With no arguments, validates tracked and untracked non-ignored repository Markdown files.
USAGE
}

for arg in "$@"; do
    case "$arg" in
        --help|-h)
            usage
            exit 0
            ;;
    esac
done

for tool in awk rustfmt rustc grep sed find sort; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "ERROR: Required tool not found: $tool" >&2
        exit 2
    fi
done

if [ ! -f "$EXTRACTOR" ]; then
    echo "ERROR: Rust block extractor not found: $EXTRACTOR" >&2
    exit 2
fi

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

COUNTER_FILE="$TEMP_DIR/counters"
# Counter file format: total validated skipped warned failed
echo "0 0 0 0 0" > "$COUNTER_FILE"

echo "========================================="
echo "Extracting Rust code blocks from markdown"
echo "========================================="

normalize_repo_path() {
    local path="$1"

    case "$path" in
        "$REPO_ROOT"/*)
            printf './%s\n' "${path#"$REPO_ROOT"/}"
            ;;
        ./*|/*)
            printf '%s\n' "$path"
            ;;
        *)
            printf './%s\n' "$path"
            ;;
    esac
}

is_excluded_markdown_file() {
    local normalized
    normalized=$(normalize_repo_path "$1")

    if git rev-parse --is-inside-work-tree >/dev/null 2>&1 && git check-ignore -q -- "$1" 2>/dev/null; then
        return 0
    fi

    case "$normalized" in
        ./target/*|./third_party/*|./node_modules/*|./.git/*|./.github/test-fixtures/*|./test-fixtures/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_reference_documentation_file() {
    local normalized
    normalized=$(normalize_repo_path "$1")

    case "$normalized" in
        ./.agents/skills/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

find_markdown_files() {
    if [ "$#" -eq 0 ]; then
        if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
            git ls-files --cached --others --exclude-standard '*.md' | while IFS= read -r md_file; do
                if ! is_excluded_markdown_file "$md_file"; then
                    printf '%s\n' "$md_file"
                fi
            done | sort -u
            return 0
        fi

        set -- .
    fi

    local target
    for target in "$@"; do
        if [ ! -e "$target" ]; then
            echo "ERROR: Markdown validation target does not exist: $target" >&2
            exit 2
        fi
    done

    for target in "$@"; do
        if [ -f "$target" ]; then
            case "$target" in
                *.md)
                    printf '%s\n' "$target"
                    ;;
            esac
        elif [ -d "$target" ]; then
            find "$target" -type f -name "*.md" -print | while IFS= read -r md_file; do
                if ! is_excluded_markdown_file "$md_file"; then
                    printf '%s\n' "$md_file"
                fi
            done
        fi
    done | sort -u
}

rust_attribute_present() {
    local attributes="$1"
    local attribute="$2"

    grep -Eq "(^|[[:space:],])${attribute}([[:space:],]|$)" <<< "$attributes"
}

content_has_non_whitespace() {
    grep -q '[^[:space:]]' <<< "$1"
}

content_without_leading_blank_lines() {
    sed '/[^[:space:]]/,$!d' <<< "$1"
}

has_item_level_rust() {
    local content="$1"
    local item_re='^[[:space:]]*(#\[|pub([[:space:]]|\(|$)|crate[[:space:]]|use[[:space:]]|mod[[:space:]]|extern([[:space:]]|$)|fn[[:space:]]|async[[:space:]]+fn[[:space:]]|const[[:space:]]+(fn[[:space:]]|[[:alpha:]_])|unsafe[[:space:]]+(fn|impl|trait|extern)[[:space:]]|struct[[:space:]]|enum[[:space:]]|union[[:space:]]|impl([[:space:]<]|$)|trait[[:space:]]|type[[:space:]]|static[[:space:]]|macro_rules!)'

    grep -Eq "$item_re" <<< "$content"
}

is_external_context_compile_error() {
    local error_file="$1"
    local external_error_re='^(error\[E(0412|0422|0425|0432|0433|0463)\]|error): (can.t find crate|unresolved import|cannot find|failed to resolve|use of undeclared|no external crate|.*not found in this scope|.*not found)'

    if ! grep -qE '^error(\[[A-Z][0-9]{4}\])?:' "$error_file"; then
        return 1
    fi

    if grep -E '^error(\[[A-Z][0-9]{4}\])?:' "$error_file" \
        | grep -vE '^error: aborting due to [0-9]+ previous errors?' \
        | grep -qEv "$external_error_re"; then
        return 1
    fi

    grep -qE "$external_error_re" "$error_file"
}

is_top_level_statement_compile_error() {
    local error_file="$1"

    grep -qE "expected item, found|non-item macro in item position|let cannot be used for global variables" "$error_file"
}

MARKDOWN_LIST="$TEMP_DIR/markdown-files"
find_markdown_files "$@" > "$MARKDOWN_LIST"

while IFS= read -r md_file; do
    [ -f "$md_file" ] || continue

    echo ""
    echo "Processing: $(normalize_repo_path "$md_file")"

    awk -f "$EXTRACTOR" "$md_file" | while IFS=$'\t' read -r -d '' line_num attributes content; do
        read -r total validated skipped warned failed < "$COUNTER_FILE"
        total=$((total + 1))

        check_content=$(content_without_leading_blank_lines "$content")

        if ! content_has_non_whitespace "$content"; then
            echo "  - Skipping empty block at line $line_num"
            skipped=$((skipped + 1))
            echo "$total $validated $skipped $warned $failed" > "$COUNTER_FILE"
            continue
        fi

        should_skip=0
        should_compile=1

        if rust_attribute_present "$attributes" "ignore"; then
            echo "  - Skipping block at line $line_num (marked ignore)"
            should_skip=1
        elif rust_attribute_present "$attributes" "no_run"; then
            echo "  - Compiling but not running block at line $line_num (marked no_run)"
            should_compile=1
        elif rust_attribute_present "$attributes" "should_panic"; then
            echo "  - Skipping block at line $line_num (marked should_panic)"
            should_skip=1
        fi

        if [ "$should_skip" -eq 0 ] && is_reference_documentation_file "$md_file"; then
            echo "  - Skipping block at line $line_num (reference documentation file)"
            should_skip=1
        fi

        has_rust_items=0
        if has_item_level_rust "$check_content"; then
            has_rust_items=1
        fi

        if [ "$should_skip" -eq 0 ] && [ "$has_rust_items" -eq 0 ] && grep -qE 'todo!\(\)|\.\.\.|\.\. |// omitted|/\* \.\.\. \*/|unimplemented!\(\)' <<< "$check_content"; then
            echo "  - Skipping block at line $line_num (incomplete/placeholder code)"
            should_skip=1
        fi

        if [ "$should_skip" -eq 0 ] && [ "$has_rust_items" -eq 0 ] && grep -qE '// Note:|// Example:|/\* config \*/|<your_|YOUR_|PLACEHOLDER' <<< "$check_content"; then
            echo "  - Skipping block at line $line_num (documentation snippet)"
            should_skip=1
        fi

        if [ "$should_skip" -eq 0 ] && [ "$has_rust_items" -eq 0 ]; then
            echo "  - Skipping block at line $line_num (partial snippet, no item-level keywords)"
            should_skip=1
        fi

        if [ "$should_skip" -eq 1 ]; then
            skipped=$((skipped + 1))
            echo "$total $validated $skipped $warned $failed" > "$COUNTER_FILE"
            continue
        fi

        test_file="$TEMP_DIR/test_${total}.rs"
        printf '%s\n' "$content" > "$test_file"

        rustfmt_ok=0
        if rustfmt --edition 2021 --check "$test_file" >/dev/null 2>&1; then
            rustfmt_ok=1
        else
            echo "  - rustfmt warning: Block at line $line_num has formatting issues (non-fatal)"
        fi

        if [ "$should_compile" -eq 1 ]; then
            if [ "$has_rust_items" -eq 1 ]; then
                compile_file="$TEMP_DIR/compile_${total}.rs"
                {
                    echo "// Auto-generated test harness for markdown code block"
                    echo "#![allow(unused)]"
                    printf '%s\n' "$content"
                } > "$compile_file"

                if ! rustc --edition 2021 --crate-type lib "$compile_file" -o "$TEMP_DIR/test_${total}.rlib" 2>"$TEMP_DIR/compile_err_${total}.txt"; then
                    compile_error_file="$TEMP_DIR/compile_err_${total}.txt"

                    if is_external_context_compile_error "$compile_error_file"; then
                        echo "  - Block at line $line_num requires external context (syntax valid)"
                        warned=$((warned + 1))
                    elif is_top_level_statement_compile_error "$compile_error_file"; then
                        wrapped_file="$TEMP_DIR/wrapped_${total}.rs"
                        {
                            echo "// Auto-generated test harness for markdown code block"
                            echo "#![allow(unused)]"
                            echo "fn main() -> Result<(), Box<dyn std::error::Error>> {"
                            printf '%s\n' "$content"
                            echo "Ok(())"
                            echo "}"
                        } > "$wrapped_file"

                        if rustc --edition 2021 "$wrapped_file" -o "$TEMP_DIR/test_${total}" 2>"$TEMP_DIR/wrapped_err_${total}.txt"; then
                            echo "  - Compiled and validated block at line $line_num (wrapped Rustdoc-style snippet)"
                            validated=$((validated + 1))
                        elif is_external_context_compile_error "$TEMP_DIR/wrapped_err_${total}.txt"; then
                            echo "  - Block at line $line_num requires external context (syntax valid)"
                            warned=$((warned + 1))
                        else
                            echo "  - FAILED: Block at line $line_num failed compilation"
                            echo "--- Compilation errors ---"
                            cat "$TEMP_DIR/wrapped_err_${total}.txt"
                            echo "--- End errors ---"
                            failed=$((failed + 1))
                            echo "$total $validated $skipped $warned $failed" > "$COUNTER_FILE"
                            continue
                        fi
                    else
                        echo "  - FAILED: Block at line $line_num failed compilation"
                        echo "--- Compilation errors ---"
                        cat "$compile_error_file"
                        echo "--- End errors ---"
                        failed=$((failed + 1))
                        echo "$total $validated $skipped $warned $failed" > "$COUNTER_FILE"
                        continue
                    fi
                else
                    echo "  - Compiled and validated block at line $line_num"
                    validated=$((validated + 1))
                fi
            elif [ "$rustfmt_ok" -eq 1 ]; then
                echo "  - Syntax validated block at line $line_num (snippet, no compilation)"
                validated=$((validated + 1))
            else
                echo "  - Syntax-checked block at line $line_num (best effort)"
                warned=$((warned + 1))
            fi
        elif [ "$rustfmt_ok" -eq 1 ]; then
            echo "  - Syntax validated block at line $line_num"
            validated=$((validated + 1))
        else
            echo "  - Syntax-checked block at line $line_num (best effort)"
            warned=$((warned + 1))
        fi

        echo "$total $validated $skipped $warned $failed" > "$COUNTER_FILE"
    done
done < "$MARKDOWN_LIST"

read -r total validated skipped warned failed < "$COUNTER_FILE"

echo ""
echo "========================================="
echo "Documentation validation complete!"
echo "Total blocks: $total"
echo "Validated: $validated"
echo "Skipped: $skipped"
echo "Warned: $warned (informational, non-blocking)"
echo "Failed: $failed"
echo "========================================="

if [ "$failed" -gt 0 ]; then
    exit 1
fi

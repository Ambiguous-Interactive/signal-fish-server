#!/usr/bin/env python3
"""
Extract Rust code blocks from Markdown files.

This script mirrors the canonical AWK extractor used by doc-validation.yml.
Keep its output byte-compatible with .github/scripts/extract-rust-blocks.awk.

Output format: line_number\tattributes\tcontent (NUL-separated records)
"""

import sys
from typing import Iterator, Optional, Tuple


POSIX_SPACE = " \t\r\n\v\f"


def _fence_start(line: str) -> Optional[int]:
    """Return the zero-based index of a CommonMark backtick fence, if present."""
    index = 0
    while index < min(3, len(line)) and line[index] == " ":
        index += 1

    if index >= len(line) or line[index] != "`":
        return None

    return index


def _opening_fence_count(line: str) -> int:
    start = _fence_start(line)
    if start is None:
        return 0

    count = 0
    while start + count < len(line) and line[start + count] == "`":
        count += 1

    return count if count >= 3 else 0


def _bare_closing_fence_count(line: str) -> int:
    count = _opening_fence_count(line)
    if count == 0:
        return 0

    start = _fence_start(line)
    assert start is not None
    rest = line[start + count :]
    return count if all(char in POSIX_SPACE for char in rest) else 0


def _fence_rest(line: str, count: int) -> str:
    start = _fence_start(line)
    assert start is not None
    return line[start + count :]


def _rust_attributes(rest: str) -> str:
    attrs = rest[4:]
    if attrs.startswith(","):
        attrs = attrs[1:]
    attrs = attrs.lstrip(POSIX_SPACE)
    return attrs or "none"


def _is_rust_info_string(rest: str) -> bool:
    if len(rest) < 4 or not (rest.startswith("rust") or rest.startswith("Rust")):
        return False
    return len(rest) == 4 or rest[4] == "," or rest[4] in POSIX_SPACE


def extract_rust_blocks(content: str) -> Iterator[Tuple[int, str, str]]:
    """
    Extract Rust code blocks from markdown content.

    Yields:
        (line_number, attributes, content) tuples
    """
    lines = content.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    in_block = False
    in_other_block = False
    block_start = 0
    block_content = ""
    seen_content = False
    attributes = ""
    rust_fence_len = 0
    other_fence_len = 0

    for i, line in enumerate(lines, 1):
        if line.endswith("\r"):
            line = line[:-1]

        fence_count = _opening_fence_count(line)
        closing_count = _bare_closing_fence_count(line)

        if in_other_block:
            if closing_count >= other_fence_len:
                in_other_block = False
            continue

        if in_block:
            if closing_count >= rust_fence_len:
                yield (block_start, attributes, block_content)
                in_block = False
                continue

            if seen_content:
                block_content = block_content + "\n" + line
            else:
                block_content = line
                seen_content = True
            continue

        if fence_count >= 3:
            rest = _fence_rest(line, fence_count)
            if _is_rust_info_string(rest):
                in_block = True
                block_start = i
                block_content = ""
                seen_content = False
                attributes = _rust_attributes(rest)
                rust_fence_len = fence_count
                continue

            in_other_block = True
            other_fence_len = fence_count
            continue

    # Handle unclosed block at EOF
    if in_block:
        yield (block_start, attributes, block_content)


def main():
    """Main entry point."""
    if len(sys.argv) != 2:
        print("Usage: extract-rust-blocks.py <markdown-file>", file=sys.stderr)
        sys.exit(1)

    markdown_file = sys.argv[1]

    try:
        with open(markdown_file, 'r', encoding='utf-8') as f:
            content = f.read()
    except FileNotFoundError:
        print(f"Error: File not found: {markdown_file}", file=sys.stderr)
        sys.exit(1)
    except IOError as e:
        print(f"Error reading file: {e}", file=sys.stderr)
        sys.exit(1)

    # Extract and output blocks
    # Use tab as delimiter (easier to parse in bash than :::)
    for line_num, attrs, block_content in extract_rust_blocks(content):
        # Output format: line\tattrs\tcontent\0
        sys.stdout.write(f"{line_num}\t{attrs}\t{block_content}\0")


if __name__ == '__main__':
    main()

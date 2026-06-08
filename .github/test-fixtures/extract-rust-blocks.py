#!/usr/bin/env python3
"""
Extract Rust code blocks from Markdown files.

This script mirrors the canonical AWK extractor used by doc-validation.yml.
Keep its output byte-compatible with .github/scripts/extract-rust-blocks.awk.

Output format: line_number\tattributes\tcontent (NUL-separated records)
"""

import re
import sys
from typing import Iterator, Tuple


def extract_rust_blocks(content: str) -> Iterator[Tuple[int, str, str]]:
    """
    Extract Rust code blocks from markdown content.

    Yields:
        (line_number, attributes, content) tuples
    """
    lines = content.splitlines()
    in_block = False
    block_start = 0
    block_content = ""
    seen_content = False
    attributes = ""

    for i, line in enumerate(lines, 1):
        # Match the canonical AWK prefix pattern with case-insensitive rust.
        if re.match(r'^```[Rr]ust', line):
            in_block = True
            block_start = i
            block_content = ""
            seen_content = False
            attributes = re.sub(r'^```[Rr]ust,?', '', line, count=1)
            if attributes == "":
                attributes = "none"
            continue

        # Match closing fence
        if line == '```' and in_block:
            # Yield the block
            yield (block_start, attributes, block_content)
            in_block = False
            continue

        # Accumulate content while in block
        if in_block:
            if seen_content:
                block_content = block_content + '\n' + line
            else:
                block_content = line
                seen_content = True

        # Handle nested/malformed blocks (another opening fence while in block)
        if line.startswith('```') and in_block and not line == '```':
            in_block = False

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

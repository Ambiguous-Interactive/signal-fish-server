#!/usr/bin/env python3
"""
Extract Rust code blocks from Markdown files.

This script implements the same extraction logic as the AWK script in the
doc-validation.yml workflow, but in Python for better portability and testing.

Output format: line_number:::attributes:::content (NUL-separated records)
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
    lines = content.split('\n')
    in_block = False
    block_start = 0
    block_content = []
    attributes = ""

    for i, line in enumerate(lines, 1):
        # Match opening fence with case-insensitive rust
        if re.match(r'^```[Rr]ust(,.*)?$', line):
            in_block = True
            block_start = i
            block_content = []
            # Extract attributes
            match = re.match(r'^```[Rr]ust,(.*)$', line)
            attributes = match.group(1) if match else ""
            continue

        # Match closing fence
        if line == '```' and in_block:
            # Yield the block
            content_str = '\n'.join(block_content)
            yield (block_start, attributes, content_str)
            in_block = False
            continue

        # Accumulate content while in block
        if in_block:
            block_content.append(line)

        # Handle nested/malformed blocks (another opening fence while in block)
        if line.startswith('```') and in_block and not line == '```':
            in_block = False

    # Handle unclosed block at EOF
    if in_block:
        content_str = '\n'.join(block_content)
        yield (block_start, attributes, content_str)


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

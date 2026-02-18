# extract-rust-blocks.awk
#
# Extracts Rust code blocks from markdown files for validation.
# Outputs NUL-delimited records in the format: line_number<TAB>attributes<TAB>content
#
# AWK state variables (uninitialized variables start at 0/""):
#   in_block     - 1 if currently parsing inside a code block, 0 otherwise
#   block_start  - line number (NR) where the current block started
#   content      - accumulated content of the current code block
#   attrs        - extracted attributes from fence (e.g., "ignore", "no_run"); "none" if no attributes

# Initialize state variables explicitly to silence AWK --lint warnings
# about uninitialized variables (particularly in_block used in the END block)
BEGIN { in_block = 0 }

# Match opening fence with optional attributes (case-insensitive)
/^```[Rr]ust/ {
  in_block = 1          # Enter code block state
  block_start = NR      # Record starting line number
  content = ""          # Reset content accumulator
  attributes = $0       # Save full fence line for reference
  # POSIX-compatible: use sub() instead of match() for mawk compatibility
  # Extract attributes after rust (case-insensitive)
  # Uses prefix match pattern instead of exact match to handle multiple fence formats:
  # - ```rust           (plain)
  # - ```rust,ignore    (with attribute)
  # - ```Rust           (capitalized)
  # Prefix match /^```[Rr]ust/ catches all these variants, then sub() strips the prefix
  attrs = $0
  sub(/^```[Rr]ust,?/, "", attrs)  # Remove ```rust or ```Rust and optional comma
  if (attrs == "") attrs = "none"  # Sentinel value prevents bash IFS tab-collapsing
  next                  # Skip to next line (do not include fence in content)
}
# Match closing fence only if in a block
/^```$/ && in_block {
  # Output with NUL byte separator to preserve multi-line content
  # Format: line_number<TAB>attributes<TAB>content<NUL>
  # Tab is used as field separator, NUL as record separator (matches JSON/YAML/TOML/Bash validators)
  # POSIX-compatible: Use printf "%c", 0 instead of "\0" for mawk compatibility
  # (mawk does not support "\0" in printf format strings, but does support %c with value 0)
  printf "%s\t%s\t%s%c", block_start, attrs, content, 0
  in_block = 0          # Exit code block state
  next                  # Skip to next line
}
# Accumulate content while in block
# Always append lines with newline separator, handling empty first lines
in_block {
  if (content == "") {
    content = $0        # First line: no leading newline
  } else {
    content = content "\n" $0  # Subsequent lines: add newline separator
  }
}
# Reset if we hit another opening fence while already in a block (nested/malformed)
/^```/ && in_block {
  in_block = 0          # Reset state on malformed/nested blocks
}
# Handle unclosed blocks at end of file
END {
  if (in_block) {
    # Output whatever we accumulated, even if block was not closed
    # POSIX-compatible: Use printf "%c", 0 instead of "\0" for mawk compatibility
    printf "%s\t%s\t%s%c", block_start, attrs, content, 0
  }
}

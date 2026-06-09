# extract-rust-blocks.awk
#
# Extracts Rust code blocks from markdown files for validation.
# Outputs NUL-delimited records in the format: line_number<TAB>attributes<TAB>content
#
# AWK state variables:
#   in_block        - 1 if currently parsing inside a Rust code block, 0 otherwise
#   in_other_block  - 1 if currently parsing inside a non-Rust code block, 0 otherwise
#   block_start     - line number (NR) where the current Rust block started
#   rust_fence_len  - opening backtick count for the current Rust block
#   other_fence_len - opening backtick count for the current non-Rust block
#   content         - accumulated content of the current Rust code block
#   attrs           - extracted attributes from fence (e.g., "ignore", "no_run"); "none" if no attributes

BEGIN {
  in_block = 0
  in_other_block = 0
  cr = sprintf("%c", 13)
}

# Normalize CRLF markdown input so fixture helpers and CI parse the same
# logical fence/content lines on Windows-created files.
{ sub(cr "$", "") }

function fence_start(line,   pos) {
  pos = 1
  while (pos <= 4 && substr(line, pos, 1) == " ") {
    pos++
  }

  # CommonMark permits at most three leading spaces before a fenced code block.
  if (pos > 4 || substr(line, pos, 1) != "`") {
    return 0
  }

  return pos
}

function fence_backtick_count(line, start,   count) {
  count = 0
  while (substr(line, start + count, 1) == "`") {
    count++
  }
  return count
}

function opening_fence_count(line,   start, count) {
  start = fence_start(line)
  if (!start) {
    return 0
  }

  count = fence_backtick_count(line, start)
  if (count < 3) {
    return 0
  }

  return count
}

function bare_closing_fence_count(line,   start, count, rest) {
  count = opening_fence_count(line)
  if (!count) {
    return 0
  }

  start = fence_start(line)
  rest = substr(line, start + count)
  if (rest ~ /^[[:space:]]*$/) {
    return count
  }

  return 0
}

function fence_rest(line, count,   start) {
  start = fence_start(line)
  return substr(line, start + count)
}

function append_content(line) {
  if (seen_content) {
    content = content "\n" line
  } else {
    content = line
    seen_content = 1
  }
}

function emit_block() {
  # Format: line_number<TAB>attributes<TAB>content<NUL>
  # POSIX-compatible: Use printf "%c", 0 instead of "\0" for mawk compatibility.
  printf "%s\t%s\t%s%c", block_start, attrs, content, 0
}

{
  fence_count = opening_fence_count($0)
  closing_count = bare_closing_fence_count($0)

  if (in_other_block) {
    if (closing_count >= other_fence_len) {
      in_other_block = 0
    }
    next
  }

  if (in_block) {
    if (closing_count >= rust_fence_len) {
      emit_block()
      in_block = 0
      next
    }

    append_content($0)
    next
  }

  if (fence_count >= 3) {
    rest = fence_rest($0, fence_count)

    if (rest ~ /^[Rr]ust([[:space:],]|$)/) {
      in_block = 1
      block_start = NR
      rust_fence_len = fence_count
      content = ""
      seen_content = 0
      attrs = rest
      sub(/^[Rr]ust,?/, "", attrs)
      sub(/^[[:space:]]+/, "", attrs)
      if (attrs == "") attrs = "none"
      next
    }

    in_other_block = 1
    other_fence_len = fence_count
    next
  }
}

END {
  if (in_block) {
    # Output whatever we accumulated, even if the Rust block was not closed.
    emit_block()
  }
}

# Skill: AWK in CI/CD Pipelines

<!--
  trigger: awk, mawk, gawk, multi-line, NUL byte, pipeline, record separator, code block extraction, markdown parsing
  | Portable AWK patterns for multi-line content processing in CI scripts | Infrastructure
-->

**Trigger**: When writing AWK scripts for CI pipelines, extracting code blocks from Markdown,
or debugging AWK failures on Ubuntu runners.

---

## When to Use

- Extracting multi-line code blocks from Markdown files in CI
- Writing AWK scripts that must run on Ubuntu (mawk) and macOS (gawk)
- Debugging AWK failures that only appear in CI but not locally
- Processing pipeline output where content spans multiple lines

## When NOT to Use

- Simple single-line field extraction (use `cut` or `grep` instead)
- Non-AWK shell scripting issues (see [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md))

## TL;DR

- NUL byte delimiters (`printf "%s%c", content, 0`) preserve multi-line blocks through pipelines
- Use `printf "%c", 0` not `"\0"` — mawk (Ubuntu default) does not support `"\0"` escape
- Use POSIX `sub()` not gawk's `match()` with capture groups — mawk incompatible
- Use prefix patterns (`/^```rust/`) not exact matches (`/^```rust(,.*)?$/`) for flexibility
- Always test AWK scripts on Ubuntu/mawk before pushing

---

## 1. AWK Multi-Line Content Processing

### The Problem

AWK record separators default to newlines. When extracting multi-line code blocks (e.g., from Markdown),
using newline-separated output causes each line to become a separate record in the downstream pipeline,
breaking validation logic.

### The Solution: NUL Byte Delimiters

Use NUL bytes as record separators to preserve multi-line content through pipelines.

```bash
# ❌ WRONG: Newline separator breaks multi-line blocks
awk '/^```rust/ {in_block=1; next} /^```$/ && in_block {
  print content; in_block=0; next
} in_block {content = content "\n" $0}' file.md | while read -r block; do
  # Each LINE of the block arrives as a separate record — validation fails
  validate "$block"
done

# ✅ CORRECT: NUL byte separator preserves entire block
awk '
  /^```rust/ {in_block=1; content=""; next}
  /^```$/ && in_block {
    printf "%s%c", content, 0  # NUL byte separator (POSIX-compatible)
    in_block=0
    next
  }
  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }
' file.md | while IFS= read -r -d '' block; do
  # Entire block arrives as one record
  validate "$block"
done
```

### Multi-Field AWK Output with NUL Delimiters

When you need multiple fields (e.g., line number, attributes, content), use a custom field separator:

```bash
awk '
  /^```[Rr]ust/ {
    in_block=1
    block_start=NR
    content=""
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)  # Extract attributes (POSIX-compatible)
    next
  }
  /^```$/ && in_block {
    # Output: line_number:::attributes:::content\0
    printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    in_block=0
    next
  }
  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }
  END {
    # CRITICAL: Handle unclosed blocks at EOF
    if (in_block) {
      printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    }
  }
' file.md | while IFS=':::' read -r -d '' line_num attributes content; do
  echo "Processing block at line $line_num with attributes: $attributes"
  echo "$content" | validate_code
done
```

---

## 2. AWK Portability: gawk vs mawk

**Critical Issue**: Ubuntu CI runners use **mawk** by default, not gawk. Many gawk-specific features are not portable.

```awk
# ❌ WRONG: gawk-specific (fails on mawk)
printf "%s\0", content        # mawk doesn't support "\0" escape
if (match($0, /pattern/, arr)) # mawk's match() doesn't support capture groups

# ✅ CORRECT: POSIX-compatible (works on both gawk and mawk)
printf "%s%c", content, 0    # Use %c with value 0 for NUL byte
sub(/pattern/, "", var)       # Use sub() instead of match() for extraction
```

**Why This Matters:**

- Local development often uses gawk (GNU awk)
- CI/CD runners (Ubuntu) default to mawk (Mike's awk)
- Scripts that work locally can fail in CI due to these differences
- **Always test AWK scripts on Ubuntu/mawk before committing**

---

## 3. AWK Pattern Portability: Prefix vs Exact Matching

### The Problem: Fragile Exact Patterns

```awk
# ❌ FRAGILE: Alternation with optional suffix
/^```[Rr]ust(,.*)?$/ {
  # Matches: ```rust, ```Rust, ```rust,ignore
  # FAILS on: ```rust ignore (space instead of comma)
  # FAILS on: ```rust,no_run or other valid fence formats
}
```

**Issues with exact pattern matching:**

1. **Brittle fence format assumptions** — Assumes comma separator, fails on spaces
2. **Maintenance burden** — Adding new fence formats requires pattern updates
3. **Portability concerns** — Complex regex behaves differently across AWK versions

### The Solution: Prefix Patterns

```awk
# ✅ ROBUST: Prefix pattern (matches any fence format)
/^```[Rr]ust/ {
  in_block = 1
  block_start = NR
  content = ""
  attrs = $0
  sub(/^```[Rr]ust,?/, "", attrs)  # Remove prefix, keep attributes
  # Now attrs contains: "ignore", "no_run", "", "ignore no_run", etc.
  next
}
```

**Benefits:**

1. **Flexible** — Works with: `rust,ignore`, `rust ignore`, `rust,no_run`, `rust,edition2021`
2. **Future-proof** — New attribute formats automatically supported
3. **Portable** — Uses POSIX `sub()` instead of gawk-specific `match()`

### Pattern Selection Guide

| Scenario                       | Pattern Type | Example                | Rationale                      |
|--------------------------------|--------------|------------------------|--------------------------------|
| Code fence detection           | Prefix       | `/^```[Rr]ust/`        | Flexible attribute handling    |
| Closing fence                  | Exact        | `/^```$/`              | Must match exactly (no prefix) |
| Language detection (no attrs)  | Exact        | `/^```Rust$/`          | Only plain code blocks         |
| Strict validation              | Exact        | `/^```Rust,ignore$/`   | Enforce specific format        |
| General extraction             | Prefix       | `/^```python/`         | Handle any Python fence        |

---

## 4. Testing AWK Patterns for Portability

```bash
# Test with both gawk and mawk
echo '```rust ignore' | gawk '/^```[Rr]ust/ {print "match"}'
echo '```rust ignore' | mawk '/^```[Rr]ust/ {print "match"}'

# Test attribute extraction
echo '```rust,ignore' | awk '
  /^```[Rr]ust/ {
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    print "attrs: [" attrs "]"
  }
'
# Output: attrs: [ignore]
```

**Test all fence format variations:**

```bash
test_fences=(
  '```rust'
  '```Rust'
  '```rust,ignore'
  '```Rust,ignore'
  '```rust ignore'
  '```rust,no_run'
  '```rust ignore no_run'
  '```rust,edition2021'
)

for fence in "${test_fences[@]}"; do
  result=$(echo "$fence" | awk '/^```[Rr]ust/ {print "MATCH"}')
  if [ "$result" = "MATCH" ]; then
    echo "PASS: $fence"
  else
    echo "FAIL: $fence"
  fi
done
```

---

## 5. Key AWK Patterns Reference

```awk
# Empty first line handling — ALWAYS check if content is empty before appending
in_block {
  if (content == "") content = $0
  else content = content "\n" $0
}

# END block for unclosed blocks at EOF
END {
  if (in_block) {
    printf "%s%c", content, 0  # POSIX-compatible NUL byte
  }
}

# Case-insensitive matching (handles both "rust" and "Rust")
/^```[Rr]ust/ { in_block = 1 }

# Extract attributes: POSIX-compatible sub() instead of gawk match()
# ❌ WRONG (gawk-only):
# if (match($0, /```rust,(.*)/, arr)) { attrs = arr[1] }

# ✅ CORRECT (POSIX-compatible):
attrs = $0
sub(/^```[Rr]ust,?/, "", attrs)  # Remove prefix, leaving only attributes
```

---

## 6. Extracting Inline Scripts to External Files

### When AWK in YAML Breaks Shellcheck

Inline AWK programs in YAML `run: |` blocks cause shellcheck failures when the AWK code contains
apostrophes or single quotes — shellcheck parses the entire block as bash and misinterprets AWK
quoting boundaries.

```yaml
# ❌ WRONG: Inline AWK with apostrophes breaks shellcheck
- name: Extract blocks
  run: |
    awk '/pattern/ { gsub(/'\''/,"") }' file.md

# ✅ CORRECT: External AWK file avoids quoting conflicts
- name: Extract blocks
  run: |
    awk -f .github/scripts/extract-rust-blocks.awk file.md
```

### When to Extract vs Inline

| AWK Program Size             | Recommendation   | Rationale                       |
|------------------------------|------------------|---------------------------------|
| 1-5 lines, no quotes         | Inline OK        | Simple enough to keep inline    |
| 5-10 lines                   | Consider extract | Readability benefit             |
| > 10 lines                   | Always extract   | Maintainability and testability |
| Any size with apostrophes    | Always extract   | Shellcheck compatibility        |

**Validate external AWK files:** `awk -f .github/scripts/extract-rust-blocks.awk /dev/null`

---

## Related Skills

- [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md) — Shellcheck, variable quoting, subshells
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Workflow structure and configuration
- [ci-cd-troubleshooting](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures

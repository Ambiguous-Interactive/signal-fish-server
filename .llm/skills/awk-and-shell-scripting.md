# Skill: AWK and Shell Scripting in CI/CD

<!-- trigger: awk, shell script, bash, posix, mawk, gawk, nul delimiter, multi-line processing | POSIX-compatible AWK and shell scripting for CI/CD pipelines | Infrastructure -->

**Trigger**: When writing AWK scripts for CI/CD workflows, processing multi-line content, or ensuring shell script portability across environments.

---

## When to Use

- Writing AWK scripts in GitHub Actions workflows
- Processing multi-line content (code blocks, logs, etc.)
- Ensuring POSIX compatibility for CI environments
- Extracting structured data from files
- Writing portable shell scripts for CI/CD
- Validating configuration files with AWK

## When NOT to Use

- Complex data processing (use dedicated tools: jq, yq, etc.)
- Application logic (AWK is for CI automation only)
- Performance-critical paths (AWK is for validation, not hot paths)

---

## TL;DR

**AWK Portability:**

- Ubuntu CI uses `mawk` (not `gawk`) - test portability locally
- Use `printf "%c", 0` for NUL bytes (not `"\0"` - mawk incompatible)
- Use POSIX `sub()` instead of gawk's `match()` with capture groups
- Use prefix patterns (`/^```rust/`) for flexibility, not exact matches (`/^```rust(,.*)?$/`)

**Multi-line Content:**

- Use NUL byte delimiters (`\0`) to preserve multi-line blocks through pipelines
- Custom field separator (`:::`) prevents conflicts with content
- Always handle unclosed blocks at EOF in AWK `END` block

**Shell Scripts:**

- Always use `set -euo pipefail` for strict error handling
- Quote all variables: `"$var"` prevents word splitting
- Use `trap` for cleanup (runs even on error)
- Bash subshells lose variable modifications - use file-based counters

---

## AWK Multi-Line Content Processing

### The Problem: Newline Separators Break Multi-Line Blocks

**Default AWK behavior:**

```bash
# ❌ WRONG: Each line becomes a separate record
awk '/^```rust/ {in_block=1; next}
     /^```$/ && in_block {print content; content=""; in_block=0; next}
     in_block {content = content "\n" $0}' file.md | while read -r block; do
  # Problem: Each LINE of the block arrives as separate record
  validate "$block"  # Only gets first line!
done
```

**Why this fails:**

- AWK's default record separator is newline (`RS="\n"`)
- Pipeline's `while read` also splits on newlines
- Multi-line code block becomes multiple records
- Validation sees only first line of each block

### The Solution: NUL Byte Delimiters

**Use NUL bytes (`\0`) as record separators:**

```bash
# ✅ CORRECT: Entire block arrives as one record
awk '
  /^```rust/ {
    in_block = 1
    content = ""
    next
  }
  /^```$/ && in_block {
    # CRITICAL: Use printf "%c", 0 (POSIX compatible)
    printf "%s%c", content, 0
    in_block = 0
    next
  }
  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }
  END {
    # CRITICAL: Handle unclosed blocks at EOF
    if (in_block) {
      printf "%s%c", content, 0
    }
  }
' file.md | while IFS= read -r -d '' block; do
  # Entire block arrives as one record
  validate "$block"
done
```

**Key patterns:**

1. **NUL byte output**: `printf "%s%c", content, 0` (POSIX compatible)
2. **NUL byte input**: `while IFS= read -r -d '' block` (read with NUL delimiter)
3. **Empty first line**: Check `if (content == "")` before appending
4. **EOF handling**: `END` block handles unclosed blocks

---

## AWK Portability: gawk vs mawk

### The Problem: Ubuntu CI Uses mawk

**Local development:** Often uses `gawk` (GNU AWK)
**CI environments:** Ubuntu defaults to `mawk` (Mike's AWK)

**Incompatibilities:**

```awk
# ❌ WRONG: gawk-specific (fails on mawk)

# 1. String escape "\0" for NUL byte
printf "%s\0", content  # mawk doesn't support "\0" escape

# 2. match() with capture groups
if (match($0, /pattern (group)/, arr)) {
  value = arr[1]  # mawk's match() doesn't support capture groups
}

# 3. POSIX character classes (varies by version)
/[[:space:]]/  # Behavior differs between gawk and mawk
```

```awk
# ✅ CORRECT: POSIX-compatible (works on both)

# 1. NUL byte using %c format
printf "%s%c", content, 0

# 2. sub() for extraction instead of match()
attrs = $0
sub(/^prefix/, "", attrs)  # Remove prefix, keep rest

# 3. Simple patterns instead of character classes
/[ \t\n]/  # Explicit whitespace matching
```

### Portability Checklist

**Before committing AWK scripts:**

- [ ] Test with `mawk` (Ubuntu default): `mawk 'script' file.txt`
- [ ] Use `printf "%c", 0` for NUL bytes (not `"\0"`)
- [ ] Use `sub()` for extraction (not `match()` with capture groups)
- [ ] Avoid gawk-specific features (BEGINFILE, ENDFILE, etc.)
- [ ] Test both locally (gawk) and in Docker (mawk)

### Testing AWK Portability

```bash
# Test with both AWK implementations
echo '```rust ignore' | gawk '/^```[Rr]ust/ {print "MATCH"}'
echo '```rust ignore' | mawk '/^```[Rr]ust/ {print "MATCH"}'

# If outputs differ, script is not portable
```

---

## AWK Pattern Design

### The Problem: Exact Patterns are Brittle

**Brittle pattern (exact matching):**

```awk
# ❌ FRAGILE: Only matches specific formats
/^```[Rr]ust(,.*)?$/ {
  # Matches: ```rust, ```Rust, ```rust,ignore
  # Fails: ```rust ignore (space instead of comma)
  # Fails: ```rust,no_run ignore (multiple attributes)
}
```

**Issues:**

- Assumes comma separator (breaks on spaces)
- Doesn't handle multiple attributes flexibly
- Requires pattern updates for new fence formats
- Hard to test all variations

### The Solution: Prefix Patterns

**Flexible pattern (prefix matching):**

```awk
# ✅ ROBUST: Matches any fence format
/^```[Rr]ust/ {
  in_block = 1
  block_start = NR
  content = ""

  # Extract attributes using POSIX sub()
  attrs = $0
  sub(/^```[Rr]ust,?/, "", attrs)  # Remove prefix, keep attributes
  # Now attrs contains: "ignore", "no_run", "", "ignore no_run", etc.

  next
}
```

**Benefits:**

- Works with any attribute format: `rust,ignore`, `rust ignore`, `rust,no_run`
- Future-proof: new attribute styles automatically supported
- Portable: uses POSIX `sub()` instead of gawk-specific `match()`
- Single pattern handles all variations

### When to Use Prefix vs Exact Matching

| Scenario | Pattern Type | Example | Rationale |
|----------|--------------|---------|-----------|
| Code fence detection | Prefix | `/^```[Rr]ust/` | Flexible attribute handling |
| Closing fence | Exact | `/^```$/` | Must match exactly (no prefix) |
| Language-only detection | Exact | `/^```rust$/` | Only plain code blocks |
| Strict validation | Exact | `/^```rust,ignore$/` | Enforce specific format |
| General extraction | Prefix | `/^```python/` | Handle any Python fence |

---

## Multi-Field AWK Output

### Pattern: Multiple Fields with NUL Delimiters

**When you need to output multiple fields (e.g., line number, attributes, content):**

```awk
awk '
  /^```rust/ {
    in_block = 1
    block_start = NR
    content = ""

    # Extract attributes
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    next
  }

  /^```$/ && in_block {
    # Output: line_number:::attributes:::content\0
    # Custom separator (:::) unlikely to appear in content
    printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    in_block = 0
    next
  }

  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }

  END {
    # Handle unclosed blocks at EOF
    if (in_block) {
      printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    }
  }
' file.md | while IFS=':::' read -r -d '' line_num attributes content; do
  echo "Block at line $line_num with attributes: $attributes"
  echo "$content" | validate_code
done
```

**Key features:**

1. **Custom separator** (`:::`) - unlikely to appear in content
2. **NUL delimiter** (`%c`, 0) - preserves multi-line content
3. **Three fields** - line number, attributes, content
4. **EOF handling** - `END` block handles unclosed blocks

---

## Shell Script Best Practices

### 1. Strict Error Handling

**Always use strict mode:**

```bash
#!/usr/bin/env bash
set -euo pipefail

# set -e: Exit on error
# set -u: Exit on undefined variable
# set -o pipefail: Pipeline fails if any stage fails
```

**Why this matters:**

```bash
# Without set -e:
command_that_fails
echo "This runs even though previous command failed"

# Without set -o pipefail:
failing_command | grep pattern  # Grep success hides failure!

# Without set -u:
rm -rf "$TYPO_VARIABLE"/*  # Becomes: rm -rf /*  (DISASTER!)
```

### 2. Variable Quoting

**Always quote variables:**

```bash
# ❌ WRONG: Unquoted variables (shellcheck SC2086)
file=$1
cat $file  # Fails if $file contains spaces
rm $TEMP_DIR/*.txt  # Glob expansion issues

# ✅ CORRECT: Quoted variables
file="$1"
cat "$file"  # Works with spaces in filename
rm "$TEMP_DIR"/*.txt  # Quote variable, not glob

# ✅ CORRECT: Arrays for multiple arguments
files=("file1.txt" "file with spaces.txt")
cat "${files[@]}"  # Proper array expansion
```

### 3. Cleanup with trap

**Always use trap for cleanup:**

```bash
# ❌ WRONG: Cleanup doesn't run on error
TEMP_DIR=$(mktemp -d)
process_files "$TEMP_DIR"
rm -rf "$TEMP_DIR"  # Never runs if process_files fails

# ✅ CORRECT: Cleanup runs even on error
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT
process_files "$TEMP_DIR"
# Cleanup happens automatically
```

### 4. Subshells and Variable Scope

**The problem: Pipeline subshells lose variable modifications:**

```bash
# ❌ WRONG: Counter increments are lost
TOTAL=0
FAILED=0

find . -name "*.md" | while read -r file; do
  TOTAL=$((TOTAL + 1))
  validate "$file" || FAILED=$((FAILED + 1))
done

# TOTAL and FAILED are still 0 here — changes were in subshell!
echo "Failed: $FAILED / $TOTAL"
```

**Solution A: File-based counters (for complex pipelines):**

```bash
# ✅ CORRECT: File-based counters survive subshells
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

COUNTER_FILE="$TEMP_DIR/counters"
echo "0 0" > "$COUNTER_FILE"  # total failed

find . -name "*.md" | while read -r file; do
  # Read current counters from file
  read -r total failed < "$COUNTER_FILE"

  total=$((total + 1))
  validate "$file" || failed=$((failed + 1))

  # Write updated counters back to file
  echo "$total $failed" > "$COUNTER_FILE"
done

# Read final counters (survives pipeline)
read -r total failed < "$COUNTER_FILE"
echo "Failed: $failed / $total"
```

**Solution B: Process substitution (for simple cases):**

```bash
# ✅ CORRECT: No subshell, variables preserved
TOTAL=0
FAILED=0

while read -r file; do
  TOTAL=$((TOTAL + 1))
  validate "$file" || FAILED=$((FAILED + 1))
done < <(find . -name "*.md")

echo "Failed: $FAILED / $TOTAL"
```

---

## Real-World Example: Rust Code Block Extraction

### Problem: Extract Rust code blocks from markdown for validation

**Requirements:**

1. Handle both `rust` and `Rust` (case-insensitive)
2. Handle any attribute format: `rust,ignore`, `rust ignore`, `rust,no_run`
3. Preserve multi-line code content
4. Report line numbers for errors
5. Handle unclosed blocks at EOF
6. Work on both gawk and mawk

**Solution:**

```bash
#!/usr/bin/env bash
set -euo pipefail

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

# Counter file format: total validated skipped failed
echo "0 0 0 0" > "$TEMP_DIR/counters"

awk '
  # Match opening fence (flexible pattern)
  /^```[Rr]ust/ {
    in_block = 1
    block_start = NR
    content = ""

    # Extract attributes using POSIX sub()
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)

    next
  }

  # Match closing fence
  /^```$/ && in_block {
    # Output with NUL delimiter (POSIX compatible)
    printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    in_block = 0
    next
  }

  # Accumulate content
  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }

  # Handle unclosed blocks at EOF
  END {
    if (in_block) {
      printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    }
  }
' "$@" | while IFS=':::' read -r -d '' line_num attrs content; do
  # Read counters
  read -r total validated skipped failed < "$TEMP_DIR/counters"
  total=$((total + 1))

  # Check if should skip (ignore attribute)
  if echo "$attrs" | grep -q "ignore"; then
    skipped=$((skipped + 1))
    echo "Skipped: line $line_num (ignore attribute)"
  else
    # Validate code block
    if echo "$content" | rustfmt --check --edition 2021 >/dev/null 2>&1; then
      validated=$((validated + 1))
    else
      failed=$((failed + 1))
      echo "ERROR: line $line_num: Invalid Rust code"
    fi
  fi

  # Write counters
  echo "$total $validated $skipped $failed" > "$TEMP_DIR/counters"
done

# Read final counts
read -r total validated skipped failed < "$TEMP_DIR/counters"

echo ""
echo "Summary:"
echo "  Total blocks: $total"
echo "  Validated: $validated"
echo "  Skipped: $skipped"
echo "  Failed: $failed"

# Exit with error if any failed
[ "$failed" -eq 0 ]
```

**Key features:**

1. ✅ POSIX-compatible AWK (works on mawk and gawk)
2. ✅ Flexible pattern matching (handles any fence format)
3. ✅ Multi-line preservation (NUL delimiters)
4. ✅ File-based counters (survive pipeline)
5. ✅ Error reporting with line numbers
6. ✅ Handles unclosed blocks at EOF
7. ✅ Clear summary output

---

## Debugging AWK Scripts

### Enable AWK Debugging

```bash
# Print all AWK variables at key points
awk '
  /pattern/ {
    # Debug output to stderr
    print "DEBUG: NR=" NR ", in_block=" in_block, "content=" substr(content, 1, 50) > "/dev/stderr"
  }
' file.md
```

### Test AWK Patterns Interactively

```bash
# Test pattern matching
echo '```rust ignore' | awk '/^```[Rr]ust/ {print "MATCH"}'

# Test attribute extraction
echo '```rust,ignore no_run' | awk '
  /^```[Rr]ust/ {
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    print "Attributes: [" attrs "]"
  }
'
```

### Validate NUL Delimiter Output

```bash
# Visualize NUL delimiters
awk 'BEGIN { printf "field1%cfield2%c", 0, 0 }' | od -c
# Should show: \0 between fields
```

---

## GitHub Actions Integration

### Pattern: Inline AWK Scripts in Workflows

**Always document AWK scripts in workflows:**

```yaml
# .github/workflows/doc-validation.yml

- name: Validate Rust code blocks
  run: |
    set -euo pipefail

    # Extract and validate Rust code blocks from markdown files
    # Uses NUL byte delimiters to preserve multi-line content
    # Pattern /^```[Rr]ust/ matches any attribute format (flexible)
    awk '
      # Match opening fence (case-insensitive, any attributes)
      /^```[Rr]ust/ {
        in_block = 1
        content = ""
        attrs = $0
        sub(/^```[Rr]ust,?/, "", attrs)  # POSIX-compatible extraction
        next
      }

      # Match closing fence
      /^```$/ && in_block {
        printf "%s:::%s%c", attrs, content, 0  # NUL delimiter
        in_block = 0
        next
      }

      # Accumulate content
      in_block {
        if (content == "") content = $0
        else content = content "\n" $0
      }

      # Handle unclosed blocks at EOF
      END {
        if (in_block) printf "%s:::%s%c", attrs, content, 0
      }
    ' **/*.md | while IFS=':::' read -r -d '' attrs content; do
      # Skip blocks with 'ignore' attribute
      if echo "$attrs" | grep -q "ignore"; then
        continue
      fi

      # Validate Rust code
      echo "$content" | rustfmt --check --edition 2021
    done
```

### Pattern: Shellcheck Validation

**Always validate inline scripts with shellcheck:**

```yaml
- name: Validate workflow scripts
  run: |
    set -euo pipefail

    # Extract inline scripts and validate with shellcheck
    # (Simplified example - see github-actions-best-practices.md for full pattern)
    shellcheck -s bash -  <<'EOF'
    set -euo pipefail
    # Inline script content here
    EOF
```

---

## Common Pitfalls

### Pitfall 1: Forgetting END Block

```awk
# ❌ WRONG: Unclosed blocks at EOF are lost
/^```rust/ { in_block=1; content="" }
/^```$/ { printf "%s%c", content, 0; in_block=0 }
in_block { content = content "\n" $0 }
# If file ends without closing ```, content is lost!

# ✅ CORRECT: END block handles unclosed blocks
END {
  if (in_block) printf "%s%c", content, 0
}
```

### Pitfall 2: Empty First Line

```awk
# ❌ WRONG: First line becomes newline
in_block { content = content "\n" $0 }
# If first line in block, content = "\nfirst line"

# ✅ CORRECT: Check if content is empty
in_block {
  if (content == "") content = $0
  else content = content "\n" $0
}
```

### Pitfall 3: Using gawk-Specific Features

```awk
# ❌ WRONG: gawk-specific (fails on mawk)
match($0, /pattern (group)/, arr)
value = arr[1]

# ✅ CORRECT: POSIX-compatible
value = $0
sub(/pattern /, "", value)
```

### Pitfall 4: Unquoted Variables in Shell

```bash
# ❌ WRONG: Word splitting breaks paths with spaces
file="my document.md"
cat $file  # Expands to: cat my document.md (two args!)

# ✅ CORRECT: Quote variables
cat "$file"  # Expands to: cat "my document.md"
```

---

## Prevention Checklist

Before committing AWK/shell scripts:

- [ ] AWK script tested with both `gawk` and `mawk`
- [ ] NUL delimiters use `printf "%c", 0` (not `"\0"`)
- [ ] Attribute extraction uses `sub()` (not `match()` with capture groups)
- [ ] `END` block handles unclosed blocks at EOF
- [ ] Empty first line handled: `if (content == "") content = $0`
- [ ] Shell script uses `set -euo pipefail`
- [ ] All variables quoted: `"$var"`
- [ ] Cleanup uses `trap` for robustness
- [ ] Shellcheck validation passes with no warnings
- [ ] Script documented with comments explaining key patterns
- [ ] Tested locally before pushing to CI

---

## Lessons Learned: Common Pitfalls from Production CI/CD

### AWK Apostrophes in YAML Workflow Inline Scripts

**Never embed AWK containing apostrophes inside single-quoted bash
strings in YAML `run: |` blocks.** Shellcheck parses the YAML-embedded
script as bash, and single quotes inside AWK programs break
shellcheck's quoting analysis, producing false positives or masking
real errors.

- **Symptom:** Shellcheck reports quoting errors (SC1003, SC2016)
  on AWK programs embedded in workflow YAML
- **Root cause:** YAML `run: |` blocks are parsed as bash by
  shellcheck; apostrophes in AWK conflict with bash single-quotes
- **Solution:** Extract AWK programs to external files in
  `.github/scripts/` and invoke with `awk -f .github/scripts/script.awk`
- **Rule of thumb:** Prefer external script files for any AWK
  program longer than ~10 lines

### Bash IFS is a Character Set, Not a String Delimiter

**`IFS=':::'` does NOT split on the string `:::`.** Bash `IFS` is a
set of individual delimiter characters -- `IFS=':::'` is equivalent
to `IFS=':'`, splitting on every single `:`.

- **Symptom:** Fields split incorrectly when using multi-character
  IFS values (e.g., `IFS=':::'` with `read -r -d ''`)
- **Root cause:** `IFS` treats each character independently,
  not as a substring
- **Solution:** Use `IFS=$'\t'` (tab) or another single-character
  delimiter that won't appear in content

### AWK Range Pattern Self-Matching

**When using AWK to extract script blocks from workflow YAML files,
range patterns (e.g., `/start/,/end/`) can match references to the
target block name in other jobs.** For example, extracting a `run:`
block named "Validate Rust code" may also capture lines in other
jobs that reference that step name.

- **Symptom:** AWK extraction captures too many lines or content
  from unrelated workflow jobs
- **Root cause:** Range patterns match any line containing the
  pattern, not just the intended block boundary
- **Solution:** Use flag-based state machines instead of range
  patterns -- set a flag on the start pattern, clear it on the
  end pattern, and process lines only when the flag is set

### Local Validation with `scripts/validate-ci.sh`

Run `scripts/validate-ci.sh` locally before pushing CI/CD changes. It validates:

- AWK file syntax (files in `.github/scripts/`)
- Shell script lint (shellcheck on `scripts/` and `.githooks/`)
- Markdown link integrity (relative paths resolve correctly)

---

## Related Skills

- [`github-actions-best-practices`](./github-actions-best-practices.md) — Workflow patterns and AWK examples
- [ci-cd-troubleshooting](./ci-cd-troubleshooting.md) — Debugging CI failures
- [markdown-best-practices](./markdown-best-practices.md) — Markdown processing patterns
- [defensive-programming](./defensive-programming.md) — Error handling principles

---

## Summary

**AWK and shell scripting in CI/CD requires portability and robustness:**

**AWK Best Practices:**

1. **POSIX compatibility** - Test on both gawk (local) and mawk (CI)
2. **NUL delimiters** - Use `printf "%c", 0` for multi-line content
3. **Prefix patterns** - More flexible than exact matching
4. **END blocks** - Handle unclosed blocks at EOF
5. **sub() for extraction** - More portable than match() with capture groups

**Shell Best Practices:**

1. **Strict mode** - Always `set -euo pipefail`
2. **Quote variables** - Prevent word splitting and glob expansion
3. **Use trap** - Ensure cleanup runs even on error
4. **File-based counters** - Survive pipeline subshells
5. **Shellcheck validation** - Catch issues before CI

**Key insight:** CI environments (Ubuntu/mawk) differ from local development (macOS/gawk). Always test scripts in CI-like environments before committing.

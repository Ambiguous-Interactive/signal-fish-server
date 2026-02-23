# Skill: AWK Text Processing

<!--
  trigger: awk, mawk, gawk, nul delimiter, multi-line processing, field processing, column manipulation, posix awk
  | POSIX-compatible AWK scripting for CI/CD: multi-line blocks, portability, pattern design
  | Infrastructure
-->

**Trigger**: When writing AWK scripts for CI/CD workflows, processing multi-line content,
or ensuring AWK portability across gawk (local) and mawk (Ubuntu CI).

---

## When to Use

- Extracting structured data from files (code blocks, logs, etc.)
- Processing multi-line content with NUL delimiters
- Ensuring POSIX AWK compatibility for CI environments
- Validating configuration files with AWK

## When NOT to Use

- Complex data processing (use dedicated tools: jq, yq, etc.)
- Application logic (AWK is for CI automation only)

---

## TL;DR

- Ubuntu CI uses `mawk` (not `gawk`) — test portability locally
- Use `printf "%c", 0` for NUL bytes (not `"\0"` — mawk incompatible)
- Use POSIX `sub()` instead of gawk's `match()` with capture groups
- Use prefix patterns (`/^```rust/`) for flexibility over exact matches
- Use NUL byte delimiters (`\0`) to preserve multi-line blocks through pipelines

---

## AWK Multi-Line Content Processing

### The Problem: Newline Separators Break Multi-Line Blocks

```bash
# WRONG: Each line becomes a separate record
awk '/^```rust/ {in_block=1; next}
     /^```$/ && in_block {print content; content=""; in_block=0; next}
     in_block {content = content "\n" $0}' file.md | while read -r block; do
  validate "$block"  # Only gets first line!
done
```

**Why this fails:** AWK's default `RS="\n"` and pipeline `while read` both split on newlines.

### The Solution: NUL Byte Delimiters

```bash
# CORRECT: Entire block arrives as one record
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
  validate "$block"
done
```

Key patterns:

1. **NUL byte output**: `printf "%s%c", content, 0` (POSIX compatible)
2. **NUL byte input**: `while IFS= read -r -d '' block`
3. **Empty first line**: Check `if (content == "")` before appending
4. **EOF handling**: `END` block handles unclosed blocks

---

## AWK Portability: gawk vs mawk

### Incompatibilities to Avoid

```awk
# WRONG: gawk-specific (fails on mawk)
printf "%s\0", content           # mawk doesn't support "\0" escape
if (match($0, /pattern (group)/, arr)) { value = arr[1] }  # no capture groups
```

```awk
# CORRECT: POSIX-compatible (works on both)
printf "%s%c", content, 0        # NUL byte using %c format
attrs = $0
sub(/^prefix/, "", attrs)        # Remove prefix, keep rest
```

### Portability Checklist

- [ ] Test with `mawk` (Ubuntu default): `mawk 'script' file.txt`
- [ ] Use `printf "%c", 0` for NUL bytes (not `"\0"`)
- [ ] Use `sub()` for extraction (not `match()` with capture groups)
- [ ] Avoid gawk-specific features (BEGINFILE, ENDFILE, etc.)

---

## AWK Pattern Design

### Prefix vs Exact Matching

```awk
# FRAGILE: Only matches specific formats
/^```[Rr]ust(,.*)?$/ { ... }
# Fails on: ```rust ignore (space), ```rust,no_run ignore (multiple attrs)

# ROBUST: Matches any fence format
/^```[Rr]ust/ {
  attrs = $0
  sub(/^```[Rr]ust,?/, "", attrs)  # Extract attributes using POSIX sub()
}
```

| Scenario | Pattern Type | Example |
|----------|--------------|---------|
| Code fence detection | Prefix | `/^```[Rr]ust/` |
| Closing fence | Exact | `/^```$/` |
| Strict validation | Exact | `/^```Rust,ignore$/` |

### Keep Cross-Language Token Boundaries in Sync

When a shell/AWK script and a Rust test parse the same token (for example, URL extraction),
use the same boundary semantics in both places.

```awk
# ❌ DRIFT: only excludes literal spaces, not tabs
/https:\/\/img\.shields\.io\/[^"' )>]+/

# ✅ PARITY: excludes all whitespace via POSIX class
/https:\/\/img\.shields\.io\/[^"'[:space:])>]+/
```

If Rust uses `char::is_whitespace()`, AWK should normally use `[[:space:]]` for equivalent behavior.

---

## Multi-Field AWK Output

```awk
awk '
  /^```rust/ {
    in_block = 1; block_start = NR; content = ""
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    next
  }
  /^```$/ && in_block {
    # Custom separator (:::) unlikely to appear in content
    printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    in_block = 0; next
  }
  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }
  END {
    if (in_block) { printf "%s:::%s:::%s%c", block_start, attrs, content, 0 }
  }
' file.md | while IFS=$'\t' read -r -d '' record; do
  # NOTE: IFS is a character set, not a string — use single-char delimiter
  echo "$record"
done
```

**Important**: `IFS=':::'` does NOT split on `:::` — bash `IFS` treats each character
independently (`IFS=':'`). Use `IFS=$'\t'` (tab) or another single character.

---

## Debugging AWK Scripts

```bash
# Print all AWK variables at key points
awk '
  /pattern/ {
    print "DEBUG: NR=" NR ", in_block=" in_block > "/dev/stderr"
  }
' file.md

# Test pattern matching interactively
echo '```rust ignore' | awk '/^```[Rr]ust/ {print "MATCH"}'

# Test attribute extraction
echo '```rust,ignore no_run' | awk '
  /^```[Rr]ust/ {
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    print "Attributes: [" attrs "]"
  }
'

# Visualize NUL delimiters
awk 'BEGIN { printf "field1%cfield2%c", 0, 0 }' | od -c
```

---

## Common Pitfalls

### Pitfall 1: Forgetting END Block

```awk
# WRONG: Unclosed blocks at EOF are lost
/^```$/ { printf "%s%c", content, 0; in_block=0 }
# If file ends without closing ```, content is lost!

# CORRECT: END block handles unclosed blocks
END { if (in_block) printf "%s%c", content, 0 }
```

### Pitfall 2: Empty First Line

```awk
# WRONG: First line becomes leading newline
in_block { content = content "\n" $0 }

# CORRECT: Check if content is empty
in_block {
  if (content == "") content = $0
  else content = content "\n" $0
}
```

### Pitfall 3: AWK Apostrophes in YAML Inline Scripts

Never embed AWK containing apostrophes inside single-quoted bash strings in YAML `run: |`
blocks. Shellcheck parses the YAML-embedded script as bash, and single quotes inside AWK
break shellcheck's quoting analysis.

**Solution:** Extract AWK programs to external files in `.github/scripts/` and invoke with
`awk -f .github/scripts/script.awk`. Prefer external files for AWK programs longer than ~10 lines.

### Pitfall 4: AWK Range Pattern Self-Matching

Range patterns (e.g., `/start/,/end/`) can match references to the target block name in other
jobs, capturing too many lines. Use flag-based state machines instead: set a flag on the start
pattern, clear it on the end, and process lines only when the flag is set.

### Pitfall 5: Prefix Pattern Over-Matching

`/^```[Rr]ust/` also matches `` ```rustic `` or `` ```rusty ``. For stricter detection,
anchor the end: `/^```[Rr]ust(,.*| .*)?$/`.

---

## See Also

- [shell-scripting-patterns](./shell-scripting-patterns.md) — bash idioms, error handling, portable scripts
- [GitHub-actions-awk](./github-actions-awk.md) — AWK examples in workflow YAML
- [ci-cd-troubleshooting-scripts](./ci-cd-troubleshooting-scripts.md) — Debugging CI script failures

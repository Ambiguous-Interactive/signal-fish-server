# Skill: Shell Scripting Patterns

<!--
  trigger: bash, shell script, posix, set -euo pipefail, trap, subshell, variable quoting, shellcheck, pipeline
  | Bash idioms, error handling, portable shell scripts for CI/CD pipelines
  | Infrastructure
-->

**Trigger**: When writing shell scripts for CI/CD pipelines, debugging subshell variable scope
issues, or ensuring POSIX-compatible bash scripts.

---

## When to Use

- Writing portable shell scripts for CI/CD
- Handling errors, cleanup, and variable scoping
- Debugging pipeline subshell issues

## When NOT to Use

- Complex data processing (use dedicated tools: jq, yq, etc.)
- Application logic (shell scripts are for CI automation only)

---

## TL;DR

- Always use `set -euo pipefail` for strict error handling
- Quote all variables: `"$var"` prevents word splitting
- Use `trap` for cleanup (runs even on error)
- Bash subshells lose variable modifications — use file-based counters or process substitution

---

## 1. Strict Error Handling

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

---

## 2. Variable Quoting

**Always quote variables:**

```bash
# WRONG: Unquoted variables (shellcheck SC2086)
file=$1
cat $file  # Fails if $file contains spaces
rm $TEMP_DIR/*.txt  # Glob expansion issues

# CORRECT: Quoted variables
file="$1"
cat "$file"  # Works with spaces in filename
rm "$TEMP_DIR"/*.txt  # Quote variable, not glob

# CORRECT: Arrays for multiple arguments
files=("file1.txt" "file with spaces.txt")
cat "${files[@]}"  # Proper array expansion
```

---

## 3. Cleanup with trap

**Always use trap for cleanup:**

```bash
# WRONG: Cleanup doesn't run on error
TEMP_DIR=$(mktemp -d)
process_files "$TEMP_DIR"
rm -rf "$TEMP_DIR"  # Never runs if process_files fails

# CORRECT: Cleanup runs even on error
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT
process_files "$TEMP_DIR"
# Cleanup happens automatically
```

---

## 4. Subshells and Variable Scope

**The problem: Pipeline subshells lose variable modifications:**

```bash
# WRONG: Counter increments are lost
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
# CORRECT: File-based counters survive subshells
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

COUNTER_FILE="$TEMP_DIR/counters"
echo "0 0" > "$COUNTER_FILE"  # total failed

find . -name "*.md" | while read -r file; do
  read -r total failed < "$COUNTER_FILE"
  total=$((total + 1))
  validate "$file" || failed=$((failed + 1))
  echo "$total $failed" > "$COUNTER_FILE"
done

read -r total failed < "$COUNTER_FILE"
echo "Failed: $failed / $total"
```

**Solution B: Process substitution (for simple cases):**

```bash
# CORRECT: No subshell, variables preserved
TOTAL=0
FAILED=0

while read -r file; do
  TOTAL=$((TOTAL + 1))
  validate "$file" || FAILED=$((FAILED + 1))
done < <(find . -name "*.md")

echo "Failed: $FAILED / $TOTAL"
```

---

## 5. Argument Parsing Without Dead Checks

When using `while (($# > 0)); do ... shift; done`, all arguments are consumed by design.
Do not add a post-loop `if (($# > 0)); then ... fi` check; it is unreachable and misleading.

```bash
# ❌ WRONG: unreachable post-loop check
while (($# > 0)); do
  case "$1" in
    --flag) shift ;;
    *) shift ;;
  esac
done
if (($# > 0)); then
  echo "Unexpected arguments"
fi

# ✅ CORRECT: handle unknown/extra args inside the loop
while (($# > 0)); do
  case "$1" in
    --flag) shift ;;
    --) shift; break ;;
    -*) echo "Unknown option: $1"; exit 2 ;;
    *) echo "Too many positional arguments"; exit 2 ;;
  esac
done
```

---

## Real-World Example: Rust Code Block Extraction

```bash
#!/usr/bin/env bash
set -euo pipefail

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT
echo "0 0 0 0" > "$TEMP_DIR/counters"  # total validated skipped failed

# AWK extracts Rust code blocks as NUL-delimited records
awk '
  /^```[Rr]ust/ { in_block=1; start=NR; content=""; next }
  /^```$/ && in_block { printf "%s:::%s%c",start,content,0; in_block=0; next }
  in_block { content = (content=="" ? $0 : content "\n" $0) }
' "$@" | while IFS= read -r -d '' record; do
  block_start="${record%%:::*}"; content="${record#*:::}"
  read -r total validated skipped failed < "$TEMP_DIR/counters"
  total=$((total + 1))
  if echo "$content" | rustfmt --check --edition 2021 >/dev/null 2>&1; then
    validated=$((validated + 1))
  else
    failed=$((failed + 1))
    echo "ERROR: line $block_start: Invalid Rust code"
  fi
  echo "$total $validated $skipped $failed" > "$TEMP_DIR/counters"
done

read -r total validated skipped failed < "$TEMP_DIR/counters"
echo "Summary: total=$total validated=$validated skipped=$skipped failed=$failed"
[ "$failed" -eq 0 ]
```

---

## Prevention Checklist

- [ ] Uses `set -euo pipefail`; all variables quoted: `"$var"`
- [ ] Cleanup uses `trap 'rm -rf "$TEMP_DIR"' EXIT`
- [ ] Pipeline counters use file-based approach or process substitution
- [ ] `IFS` uses single-character delimiter (not multi-char like `:::`)
- [ ] Shellcheck passes; tested locally before pushing to CI

---

## Lessons Learned

### Bash IFS is a Character Set, Not a String

`IFS=':::'` does NOT split on the string `:::`. Bash `IFS` treats each character
independently — `IFS=':::'` is equivalent to `IFS=':'`. Use `IFS=$'\t'` (tab) or
another single character that won't appear in content.

### Use `#!/usr/bin/env bash` Not `#!/bin/bash`

`/bin/bash` may not exist on all systems (e.g., FreeBSD uses `/usr/local/bin/bash`).
`#!/usr/bin/env bash` works on macOS, Linux, and BSD.

### `grep -c` Fallback Produces Multi-Line Output

`grep -c` outputs "0" with exit code 1 when no matches found. Wrapping it as
`$(grep -c ... || echo "0")` produces "0\n0" — grep emits "0", then the
fallback echo also emits "0", both inside the same command substitution.

```bash
# BAD: Multi-line output when grep finds 0 matches
COUNT=$(grep -c "pattern" file.txt || echo "0")

# GOOD: Separate the fallback from command substitution
COUNT=$(grep -c "pattern" file.txt 2>/dev/null) || COUNT=0
```

### Run `scripts/validate-ci.sh` Before Pushing

Validates AWK syntax, shellcheck on `scripts/` and `.githooks/`, and Markdown links.

### `cargo test` Accepts Only One Positional TESTNAME

`cargo test [TESTNAME] [-- [ARGS]]` takes at most one positional filter before `--`.
To run multiple named tests, pass them after the `--` separator. Forgetting `--`
causes the second name to be rejected as an unexpected argument.

```bash
# BAD: Two positional args — second is rejected by cargo
cargo test --test suite test_a test_b

# GOOD: Multiple filters after --
cargo test --locked --test suite -- test_a test_b
```

---

## See Also

- [AWK Text Processing](./awk-text-processing.md) — AWK patterns, NUL delimiters, portability
- [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md) — Shellcheck in CI workflows
- [CI Troubleshooting Scripts](./ci-cd-troubleshooting-scripts.md) — Debugging CI script failures
- [Defensive Programming](./defensive-programming.md) — Error handling principles

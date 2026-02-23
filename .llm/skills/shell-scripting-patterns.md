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

# Counter file format: total validated skipped failed
echo "0 0 0 0" > "$TEMP_DIR/counters"

awk '
  /^```[Rr]ust/ {
    in_block = 1; block_start = NR; content = ""
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    next
  }
  /^```$/ && in_block {
    printf "%s:::%s:::%s%c", block_start, attrs, content, 0
    in_block = 0; next
  }
  in_block {
    if (content == "") content = $0
    else content = content "\n" $0
  }
  END { if (in_block) printf "%s:::%s:::%s%c", block_start, attrs, content, 0 }
' "$@" | while IFS= read -r -d '' record; do
  # Parse fields from record (use single-char delimiter, not :::)
  block_start="${record%%:::*}"
  rest="${record#*:::}"
  attrs="${rest%%:::*}"
  content="${rest#*:::}"

  read -r total validated skipped failed < "$TEMP_DIR/counters"
  total=$((total + 1))

  if echo "$attrs" | grep -q "ignore"; then
    skipped=$((skipped + 1))
  else
    if echo "$content" | rustfmt --check --edition 2021 >/dev/null 2>&1; then
      validated=$((validated + 1))
    else
      failed=$((failed + 1))
      echo "ERROR: line $block_start: Invalid Rust code"
    fi
  fi
  echo "$total $validated $skipped $failed" > "$TEMP_DIR/counters"
done

read -r total validated skipped failed < "$TEMP_DIR/counters"
echo "Summary: total=$total validated=$validated skipped=$skipped failed=$failed"
[ "$failed" -eq 0 ]
```

---

## Prevention Checklist

Before committing shell scripts:

- [ ] Shell script uses `set -euo pipefail`
- [ ] All variables quoted: `"$var"`
- [ ] Cleanup uses `trap 'rm -rf "$TEMP_DIR"' EXIT`
- [ ] Pipeline counter variables use file-based approach or process substitution
- [ ] `IFS` uses single-character delimiter (not multi-char like `:::`)
- [ ] Shellcheck validation passes with no warnings
- [ ] Script documented with comments explaining key patterns
- [ ] Tested locally before pushing to CI

---

## Lessons Learned

### Bash IFS is a Character Set, Not a String

`IFS=':::'` does NOT split on the string `:::`. Bash `IFS` treats each character
independently — `IFS=':::'` is equivalent to `IFS=':'`. Use `IFS=$'\t'` (tab) or
another single character that won't appear in content.

### Use `#!/usr/bin/env bash` Not `#!/bin/bash`

`/bin/bash` may not exist on all systems (e.g., FreeBSD uses `/usr/local/bin/bash`).
`#!/usr/bin/env bash` works on macOS, Linux, and BSD.

### Run `scripts/validate-ci.sh` Before Pushing

Run `scripts/validate-ci.sh` locally before pushing CI/CD changes. It validates:

- AWK file syntax (files in `.github/scripts/`)
- Shell script lint (shellcheck on `scripts/` and `.githooks/`)
- Markdown link integrity

---

## See Also

- [awk-text-processing](./awk-text-processing.md) — AWK patterns, NUL delimiters, portability
- [GitHub-actions-bash-scripts](./github-actions-bash-scripts.md) — Shellcheck in CI workflows
- [ci-cd-troubleshooting-scripts](./ci-cd-troubleshooting-scripts.md) — Debugging CI script failures
- [defensive-programming](./defensive-programming.md) — Error handling principles

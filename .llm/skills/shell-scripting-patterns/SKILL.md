---
name: shell-scripting-patterns
description: >-
  Apply project guidance for shell scripting patterns. Use when writing shell scripts for CI/CD
  pipelines, debugging subshell variable scope issues, or ensuring POSIX-compatible bash
  scripts.
---

# Shell Scripting Patterns

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

## 6. Shell Portability — `/bin/sh` vs `/bin/bash`

**Docker RUN commands and POSIX scripts use `/bin/sh` (dash on Debian), not bash.**
Bash-specific features silently fail or behave differently under `/bin/sh`.

macOS still supplies Bash 3.2. With `set -u`, it can reject `"${items[@]}"`
when `items=()` is empty; cardinality-guard the expansion and keep a macOS CI
lane for scripts that use arrays.

### Brace Expansion Does Not Work in `/bin/sh`

```bash
# ❌ WRONG: Brace expansion is a bash-ism — /bin/sh ignores it silently
rm -rf /path/{cache,src}          # Removes literal "{cache,src}" or nothing

# ✅ CORRECT: Use explicit paths
rm -rf /path/cache /path/src      # Works in all POSIX shells
```

This is especially dangerous in Dockerfiles, where `RUN` uses `/bin/sh` by default:

```dockerfile
# ❌ WRONG: Brace expansion won't work — cargo registry not cleaned
RUN cargo install cargo-deny && rm -rf /usr/local/cargo/registry/{cache,src}

# ✅ CORRECT: Two explicit paths
RUN cargo install cargo-deny \
    && rm -rf /usr/local/cargo/registry/cache /usr/local/cargo/registry/src
```

### Other Bash-isms That Fail in `/bin/sh`

| Feature | Bash | POSIX sh Equivalent |
|---------|------|---------------------|
| `[[ ]]` | Double brackets | `[ ]` (single brackets) |
| `{a,b}` | Brace expansion | Spell out each path |
| `source file` | Source a file | `. file` |
| `<(cmd)` | Process substitution | Temporary file or pipe |
| `function f()` | Function keyword | `f()` (no `function` keyword) |
| `$'...'` | ANSI-C quoting | `printf` |
| `declare -a` | Arrays | Not available in POSIX sh |

### `find` Commands — Always Use `-type f` for Files

```bash
# ❌ WRONG: Matches directories named "*.sh" too (unlikely but possible)
find scripts -name '*.sh' -exec chmod +x {} +

# ✅ CORRECT: Restrict to regular files
find scripts -type f -name '*.sh' -exec chmod +x {} +
```

**When validating `find` commands, check for `-type f` specifically — not just any `-type` flag:**

```bash
# ❌ WRONG: Matches -type d, -type l, etc. — still not restricting to files
if ! echo "$cmd" | grep -qE 'find.*-type[[:space:]]'; then warn "missing -type f"; fi

# ✅ CORRECT: Matches only -type f
if ! echo "$cmd" | grep -qE 'find.*-type[[:space:]]+f([[:space:]]|$)'; then warn "missing -type f"; fi
```

Also ensure log messages match the actual search scope:

```bash
# ❌ WRONG: Log says non-recursive, but find IS recursive
find scripts -type f -name '*.sh' -exec chmod +x {} +
echo "Made scripts/*.sh executable."

# ✅ CORRECT: Log reflects recursive behavior
find scripts -type f -name '*.sh' -exec chmod +x {} +
echo "Made scripts/**/*.sh executable."
```

---

Quiet/silent flag behavior lives in [Validation Script Output Modes](../validation-script-output-modes/SKILL.md);
use that when adding `--quiet` or deciding which messages must still print on warnings or failures.

---

## Prevention Checklist

- [ ] Uses `set -euo pipefail`; all variables quoted: `"$var"`
- [ ] Cleanup uses `trap 'rm -rf "$TEMP_DIR"' EXIT`
- [ ] Pipeline counters use file-based approach or process substitution
- [ ] `IFS` uses single-character delimiter (not multi-char like `:::`)
- [ ] Shellcheck passes; tested locally before pushing to CI
- [ ] No bash-isms in `/bin/sh` scripts or Dockerfile `RUN` commands
- [ ] Empty Bash arrays are guarded before expansion under `set -u`
- [ ] `find` commands use `-type f` when targeting files
- [ ] Log messages match actual search scope (recursive vs. non-recursive)
- [ ] `--quiet` suppresses banner, info, success, and summary — never errors or warnings
- [ ] Best-effort functions (caller uses `if ! f` / `f ||`) `return` non-zero — never `exit`
- [ ] Python packages installed via `python3 -m pip`, not bare `pip` (matches the interpreter)

---

## See Also

- [Awk Text Processing](../awk-text-processing/SKILL.md) — AWK patterns, NUL delimiters, portability
- [GitHub Actions Bash Scripts](../github-actions-bash-scripts/SKILL.md) — Shellcheck in CI workflows
- [CI CD Troubleshooting Scripts](../ci-cd-troubleshooting/references/scripts-and-tests.md) — Debugging CI script failures
- [Validation Script Output Modes](../validation-script-output-modes/SKILL.md) —
  `--quiet` behavior and failure-summary rules
- [Repo Source Hygiene Guards](../repo-source-hygiene-guards/SKILL.md) — `return`-not-`exit` and `python3 -m pip` guards

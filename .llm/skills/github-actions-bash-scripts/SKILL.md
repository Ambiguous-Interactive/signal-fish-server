---
name: github-actions-bash-scripts
description: >-
  Apply project guidance for Bash scripts in CI/CD. Use when writing inline shell scripts in
  GitHub Actions, debugging pipeline variable loss, or fixing shellcheck warnings.
---

# Bash Scripts in CI/CD

---

## When to Use

- Writing `run:` blocks in GitHub Actions workflows
- Debugging variables that reset to zero after a pipeline loop
- Fixing shellcheck SC2086 (unquoted variable) warnings
- Adding `set -euo pipefail` and `trap` cleanup to CI scripts
- Deciding whether to use `find | while` or process substitution

## When NOT to Use

- AWK-specific portability issues (see [GitHub Actions Awk](../github-actions-awk/SKILL.md))
- Workflow-level configuration (see [GitHub Actions Workflow Config](../github-actions-workflow-config/SKILL.md))

## TL;DR

- Always use `set -euo pipefail` and `trap 'rm -rf "$TEMP_DIR"' EXIT`
- Quote all variables: `"$var"` not `$var` (prevents SC2086 and word splitting)
- Pipelines create subshells — use file-based counters or process substitution to propagate state
- Use dynamic `find` discovery, not hardcoded file lists
- Use `set -x` sparingly (targeted sections only — full trace creates massive logs)

---

## 1. Shellcheck Integration in CI/CD

### Self-Validating Workflows

GitHub Actions workflows should validate their own inline bash scripts using shellcheck:

```yaml
jobs:
  shellcheck-workflow:
    name: Shellcheck Workflow Scripts
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6.0.3
      - name: Install shellcheck
        run: sudo apt-get update && sudo apt-get install -y shellcheck
      - name: Validate inline shell scripts
        run: |
          set -euo pipefail
          TEMP_DIR=$(mktemp -d)
          trap 'rm -rf "$TEMP_DIR"' EXIT
          awk '/name: My Script Step/,/^      - name:/ {
            if (/run: \|/) { in_script=1; next }
            if (in_script && /^      - name:/) { exit }
            if (in_script && /^          /) { print substr($0, 11) }
          }' .github/workflows/my-workflow.yml > "$TEMP_DIR/script.sh"
          shellcheck -s bash "$TEMP_DIR/script.sh"
```

### Variable Quoting Best Practices

Always quote variables to prevent word splitting and glob expansion:

```bash
# ❌ WRONG: Unquoted variables (shellcheck SC2086)
cat $file                    # Fails if file has spaces
rm $TEMP_DIR/*.txt           # Glob expansion issues

# ✅ CORRECT: Quoted variables
cat "$file"                  # Works with spaces in filename
rm "$TEMP_DIR"/*.txt         # Quote variable, not glob

# ✅ CORRECT: Arrays for multiple arguments
files=("file1.txt" "file with spaces.txt")
cat "${files[@]}"
```

### Common Shellcheck Warnings

| Code   | Issue                        | Fix                                              |
|--------|------------------------------|--------------------------------------------------|
| SC2086 | Unquoted variable            | `"$var"` instead of `$var`                       |
| SC2034 | Unused variable              | Add `# shellcheck disable=SC2034` comment        |
| SC2046 | Unquoted command substitution| Use `while IFS= read -r f; do ... done < <(cmd)` |

**Note:** Shellcheck validates Bash syntax but does NOT validate AWK syntax in heredocs.
AWK errors are caught at runtime.

### Variable Naming Conventions

```bash
# Constants and cross-script values: UPPERCASE
TEMP_DIR=$(mktemp -d)
COUNTER_FILE="$TEMP_DIR/counters"

# Local variables and loop iterators: lowercase
for file in *.md; do
  total=$((total + 1))
done
```

---

## 2. Bash Subshells & Variable Scope

### The Problem

Pipelines create subshells. Variables modified in a subshell are lost when the subshell exits.

```bash
# ❌ WRONG: Counter increments are lost in subshell
TOTAL=0
FAILED=0
find . -name "*.md" | while read -r file; do
  TOTAL=$((TOTAL + 1))          # Lost when subshell exits
  validate "$file" || FAILED=$((FAILED + 1))
done
echo "Failed: $FAILED / $TOTAL" # Always prints "0 / 0"
```

### Solution A: File-Based Counters

```bash
# ✅ CORRECT: File-based counters survive subshells
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

# Counter file format: 2 space-separated integers (total failed)
COUNTER_FILE="$TEMP_DIR/counters"
echo "0 0" > "$COUNTER_FILE"

find . -name "*.md" | while read -r file; do
  read -r total failed < "$COUNTER_FILE"
  total=$((total + 1))
  validate "$file" || failed=$((failed + 1))
  echo "$total $failed" > "$COUNTER_FILE"
done

read -r total failed < "$COUNTER_FILE"
echo "Failed: $failed / $total"
[ "$failed" -gt 0 ] && exit 1
```

### Solution B: Process Substitution (No Subshell)

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

## 3. Common CI Anti-Patterns

### Incomplete Error Handling

```bash
# ❌ WRONG: Only -e is insufficient; grep failure in pipe ignored
set -e
result=$(command_that_fails | grep foo)

# ✅ CORRECT: Strict error handling catches all failures
set -euo pipefail
result=$(command_that_fails | grep foo)
```

### Missing Cleanup

```bash
# ❌ WRONG: Temp files left behind on error
TEMP_DIR=$(mktemp -d)
process_files "$TEMP_DIR"
rm -rf "$TEMP_DIR"  # Never runs if process_files fails

# ✅ CORRECT: Cleanup runs even on error
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT
process_files "$TEMP_DIR"
```

### Hardcoded File Lists

```bash
# ❌ WRONG: Hardcoded list goes stale as files are added/removed
for file in README.md CONTRIBUTING.md docs/guide.md; do
  validate "$file"
done

# ✅ CORRECT: Dynamic discovery always includes all files
find . -type f -name "*.md" \
  -not -path "./target/*" \
  -not -path "./.git/*" | while read -r file; do
  validate "$file"
done
```

---

## 4. Debugging Workflow Failures

### Enable Debug Logging

Set repository secret or env var: `ACTIONS_STEP_DEBUG=true` / `RUNNER_DEBUG=1`.

```yaml
env:
  ACTIONS_STEP_DEBUG: true
  RUNNER_DEBUG: 1
```

### Targeted Tracing

```bash
# Print variable state at key points
echo "DEBUG: total=$total, failed=$failed, file=$file"

# Enable trace only for problematic sections (full set -x creates massive logs)
set -x
complicated_pipeline | awk '...' | while read -r x; do process "$x"; done
set +x
```

---

## 5. Documenting Magic Numbers

```yaml
# ❌ WRONG: Unexplained timeout
timeout-minutes: 15

# ✅ CORRECT: Documented reasoning
timeout-minutes: 15  # Generous timeout for building docs with all features
```

```bash
# ❌ WRONG: Unexplained file format
echo "0 0 0 0" > "$COUNTER_FILE"

# ✅ CORRECT: Document the schema
# Counter file format: 4 integers (total validated skipped failed)
# Example: "10 7 2 1" = 10 total, 7 validated, 2 skipped, 1 failed
echo "0 0 0 0" > "$COUNTER_FILE"
```

---

## Agent Checklist

- [ ] All variables quoted: `"$var"` not `$var` (prevents SC2086)
- [ ] Scripts use `set -euo pipefail` at the top
- [ ] Temp dirs use `trap 'rm -rf "$TEMP_DIR"' EXIT`
- [ ] Pipeline counters use file-based propagation or process substitution
- [ ] File discovery is dynamic (`find`), not hardcoded lists
- [ ] Variable naming: UPPERCASE for constants, lowercase for locals
- [ ] Timeout values and counter formats have explanatory comments
- [ ] `set -x` used only on targeted sections, not entire scripts

---

## Related Skills

- [GitHub Actions Awk](../github-actions-awk/SKILL.md) — AWK portability, multi-line content, NUL byte delimiters
- [GitHub Actions Workflow Config](../github-actions-workflow-config/SKILL.md) — Workflow structure and configuration
- [CI CD Troubleshooting Categories](../ci-cd-troubleshooting/references/diagnostic-workflow.md) — Diagnosing CI failures

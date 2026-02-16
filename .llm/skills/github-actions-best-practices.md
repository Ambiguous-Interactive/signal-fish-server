# Skill: GitHub Actions & CI/CD Best Practices

<!-- trigger: github actions, workflow, ci, cd, pipeline, bash, awk, shell script, continuous integration | Patterns for writing robust CI/CD workflows and avoiding common pitfalls | Infrastructure -->

**Trigger**: When writing GitHub Actions workflows, Bash scripts in CI, or debugging pipeline failures.

---

## When to Use

- Creating or modifying GitHub Actions workflows
- Writing Bash scripts for CI/CD pipelines
- Processing multi-line content in AWK or shell scripts
- Using loops and counters in shell pipelines
- Configuring lychee, actionlint, or other CI tools
- Debugging workflow failures or flaky tests
- Handling file path case sensitivity issues

## When NOT to Use

- Container-specific configuration (see [container-and-deployment](./container-and-deployment.md))
- Rust-specific testing (see [testing-strategies](./testing-strategies.md))
- Application code (see [rust-idioms-and-patterns](./rust-idioms-and-patterns.md))

---

## TL;DR

- AWK multi-line content needs NUL byte delimiters, not newlines
- Bash subshells lose variable modifications — use file-based counters in pipelines
- Lychee `include` is for URL regex filtering, not file glob patterns
- Always verify case-sensitive filesystem assumptions on Linux
- Documentation links must match actual filenames exactly
- Use retry loops with `docker logs` dumps for smoke tests
- Pin all action versions with SHA256 digests

---

## 1. AWK Multi-Line Content Processing

### The Problem

AWK record separators default to newlines. When extracting multi-line code blocks (e.g., from Markdown), using newline-separated output causes each line to become a separate record in the downstream pipeline, breaking validation logic.

### The Solution: NUL Byte Delimiters

Use NUL bytes (`\0`) as record separators to preserve multi-line content through pipelines.

```bash
# ❌ WRONG: Newline separator breaks multi-line blocks
awk '/^```rust/ {in_block=1; next} /^```$/ && in_block {print content; in_block=0; next} in_block {content = content "\n" $0}' file.md | while read -r block; do
  # Each LINE of the block arrives as a separate record — validation fails
  validate "$block"
done

# ✅ CORRECT: NUL byte separator preserves entire block
awk '
  /^```rust/ {in_block=1; content=""; next}
  /^```$/ && in_block {
    printf "%s\0", content  # NUL byte separator
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

When you need multiple fields (e.g., line number, attributes, content), use a custom field separator that won't appear in content:

```bash
awk '
  /^```rust(,.*)?$/ {
    in_block=1
    block_start=NR
    content=""
    # Extract attributes after "rust"
    if (match($0, /```rust,(.*)/, arr)) attrs = arr[1]
    else attrs = ""
    next
  }
  /^```$/ && in_block {
    # Output: line_number:::attributes:::content\0
    printf "%s:::%s:::%s\0", block_start, attrs, content
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
      printf "%s:::%s:::%s\0", block_start, attrs, content
    }
  }
' file.md | while IFS=':::' read -r -d '' line_num attributes content; do
  echo "Processing block at line $line_num with attributes: $attributes"
  echo "$content" | validate_code
done
```

### Key AWK Patterns

```awk
# Empty first line handling — ALWAYS check if content is empty
in_block {
  if (content == "") content = $0
  else content = content "\n" $0
}

# END block for unclosed blocks at EOF
END {
  if (in_block) {
    printf "%s\0", content
  }
}

# Case-insensitive matching
/^```[Rr]ust(,.*)?$/ {  # Matches both "rust" and "Rust"
  in_block = 1
}

# Extract attributes with regex capture groups
if (match($0, /```rust,(.*)/, arr)) {
  attrs = arr[1]  # Everything after "rust,"
}
```

---

## 2. Bash Subshells & Variable Scope

### The Problem

Pipelines create subshells. Variables modified in a subshell are lost when the subshell exits.

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

### The Solution: File-Based Counters

Use temporary files to propagate state across pipeline stages.

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

if [ $failed -gt 0 ]; then
  exit 1
fi
```

### Alternative: Process Substitution (No Subshell)

For simple cases, use process substitution to avoid subshells:

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

## 3. Lychee Link Checker Configuration

### The Problem

Lychee's `include` field in `.lychee.toml` is for **URL regex filtering**, not file glob patterns. Using file globs in `include` silently fails to filter anything.

```toml
# ❌ WRONG: include is for URL patterns, not file paths
include = [
    "**/*.md",
    "src/**/*.rs",
]
```

### The Solution: Use Command-Line Args for File Selection

Specify file patterns as CLI arguments in the workflow, not in the config file:

```yaml
# ✅ CORRECT: File patterns in workflow args
- name: Link Checker
  uses: lycheeverse/lychee-action@v2.7.0
  with:
    # File patterns are CLI args, NOT in .lychee.toml
    args: --verbose --no-progress --cache --max-cache-age 7d './**/*.md' './**/*.rs' './**/*.toml' --config .lychee.toml
```

### Lychee Config (.lychee.toml) Best Practices

```toml
# Lychee configuration — for link validation rules, NOT file selection

# Accept status codes
accept = [
    "100..=103",
    "200..=299",
    "429",  # Rate limiting
]

# Retry settings
max_retries = 3
retry_wait_time = 2
timeout = 20

# Exclude specific URL patterns (regex)
exclude = [
    "http://localhost",
    "http://127.0.0.1",
    "ws://localhost",
    "mailto:*",
]

# Exclude file paths (for internal link checking)
exclude_path = [
    "target/",
    ".git/",
]

# Exclude local file:// links
exclude_link_local = true
```

### When Lychee Fails: Case-Sensitive Paths

Lychee follows filesystem case sensitivity. On Linux, `Skills/foo.md` ≠ `skills/foo.md`.

```markdown
<!-- ❌ WRONG: Case mismatch breaks on Linux -->
See [testing guide](Skills/testing-strategies.md)

<!-- ✅ CORRECT: Exact case match -->
See [testing guide](skills/testing-strategies.md)
```

**Prevention:**
- Verify link case matches actual filename case exactly
- Use tab completion when creating links
- Test on Linux before pushing (WSL, Docker, or CI)

---

## 4. Case-Sensitive Filesystem Issues

### The Problem

Windows and macOS default to case-insensitive filesystems, but Linux (including CI runners) is case-sensitive. Links and imports that work locally may break in CI.

### Common Failures

```bash
# Local (Windows/macOS): Works
ls Skills/testing.md    # Finds skills/testing.md

# CI (Linux): Fails
ls Skills/testing.md    # No such file or directory
```

### Prevention Checklist

- [ ] All file paths use consistent casing (prefer lowercase)
- [ ] All Markdown links match actual filename case exactly
- [ ] All `mod` statements in Rust match file case
- [ ] All `#include` directives (if any) match file case
- [ ] Test on Linux before pushing (WSL, Docker, or CI)

### Fix Script: Case Audit

```bash
# Find all Markdown links and verify targets exist (case-sensitive)
find . -name "*.md" -not -path "./target/*" | while read -r md_file; do
  grep -oE '\[([^]]+)\]\(([^)]+)\)' "$md_file" | while read -r link; do
    url=$(echo "$link" | sed -E 's/.*\(([^)]+)\).*/\1/')

    # Skip external URLs
    [[ "$url" =~ ^https?:// ]] && continue

    # Resolve relative path
    file_part="${url%%#*}"
    [ -z "$file_part" ] && continue

    base_dir=$(dirname "$md_file")
    full_path=$(realpath -m "$base_dir/$file_part")

    # Check existence (case-sensitive)
    if [ ! -f "$full_path" ]; then
      echo "✗ Broken link in $md_file: $url"
      echo "  Resolved to: $full_path"
    fi
  done
done
```

---

## 5. Docker Smoke Test Patterns

### The Problem

Bare `sleep` followed by `curl` is unreliable — the server may not be ready, causing false failures.

```bash
# ❌ WRONG: Fixed sleep is unreliable
docker run -d --name test-server -p 3536:3536 myapp:ci
sleep 3
curl -f http://localhost:3536/health  # May fail if server takes >3s
```

### The Solution: Retry Loop with Diagnostics

```bash
# ✅ CORRECT: Retry loop with docker logs on failure
docker run -d --name test-server -p 3536:3536 myapp:ci

# Retry health check with exponential backoff (up to ~30s)
for i in $(seq 1 15); do
  if curl -sf http://localhost:3536/health; then
    echo ""
    echo "Health check passed on attempt $i/15"
    exit 0
  fi
  echo "Attempt $i/15: server not ready, retrying in 2s..."
  sleep 2
done

# If we get here, server failed to start
echo "ERROR: Server failed to become healthy after 30s"
echo "=== Docker logs ==="
docker logs test-server
exit 1
```

### Always Include Cleanup

```yaml
- name: Cleanup smoke test
  if: always()
  run: docker stop test-server && docker rm test-server || true
```

---

## 6. Action Version Pinning

### The Problem

Using mutable tags (`@v2`, `@main`) allows actions to change behavior between runs, breaking reproducibility.

### The Solution: SHA256 Digest Pinning

```yaml
# ❌ WRONG: Mutable tags can change
- uses: actions/checkout@v4
- uses: lycheeverse/lychee-action@v2.7.0

# ✅ CORRECT: SHA256 digest is cryptographically immutable
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
- uses: lycheeverse/lychee-action@a8c4c7cb88f0c7386610c35eb25108e448569cb0 # v2.7.0
```

**How to get SHA256 digests:**

```bash
# GitHub Actions: Go to releases, find commit SHA
# Or use gh CLI:
gh api repos/actions/checkout/commits/v4.2.2 --jq .sha
```

**Benefits:**
- Reproducible builds (same digest = same code)
- Security (prevents tag hijacking)
- Auditable (comment shows human-readable version)

---

## 7. Workflow Path Filtering Best Practices

### Trigger on Relevant Changes Only

```yaml
on:
  push:
    branches: [main]
    paths:
      - '**/*.md'
      - '**/*.rs'
      - 'Cargo.toml'
      - 'Cargo.lock'
      - '.github/workflows/this-workflow.yml'
  pull_request:
    branches: [main]
    paths:
      - '**/*.md'
      - '**/*.rs'
      - 'Cargo.toml'
      - 'Cargo.lock'
      - '.github/workflows/this-workflow.yml'
```

**Always include the workflow file itself** — changes to the workflow should trigger a run to validate them.

### Concurrency Control

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.head_ref || github.run_id }}
  cancel-in-progress: true
```

Prevents duplicate runs on rapid pushes.

---

## 8. Minimal Permissions (Security)

### Default to Read-Only

```yaml
# NEVER omit permissions — defaults to full write access
permissions:
  contents: read
```

### Grant Only What's Needed

```yaml
# If workflow creates issues or comments
permissions:
  contents: read
  issues: write
  pull-requests: write
```

---

## 9. Common CI Anti-Patterns

### Using `set -e` Without `set -u` or `set -o pipefail`

```bash
# ❌ WRONG: Only -e is insufficient
set -e
result=$(command_that_fails | grep foo)  # Grep failure ignored!

# ✅ CORRECT: Strict error handling
set -euo pipefail
result=$(command_that_fails | grep foo)  # Pipeline fails if any stage fails
```

### Not Using `trap` for Cleanup

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
# ❌ WRONG: Hardcoded list goes stale
for file in README.md CONTRIBUTING.md docs/guide.md; do
  validate "$file"
done

# ✅ CORRECT: Dynamic discovery
find . -type f -name "*.md" \
  -not -path "./target/*" \
  -not -path "./.git/*" | while read -r file; do
  validate "$file"
done
```

---

## 10. Debugging Workflow Failures

### Enable Debug Logging

```yaml
env:
  ACTIONS_STEP_DEBUG: true
  RUNNER_DEBUG: 1
```

Or set repository secret `ACTIONS_STEP_DEBUG=true`.

### Print Variable State

```bash
# At key points, dump state for debugging
echo "DEBUG: total=$total, failed=$failed, file=$file"
echo "DEBUG: block content:"
echo "$content" | head -20
```

### Use `set -x` Selectively

```bash
# Enable trace for problematic sections only
set -x
complicated_pipeline | awk '...' | while read -r x; do
  process "$x"
done
set +x
```

Full `set -x` in CI creates massive logs — use sparingly.

---

## Agent Checklist

- [ ] AWK multi-line extraction uses NUL byte delimiters (`printf "%s\0", content`)
- [ ] Bash pipeline counters use file-based propagation, not subshell variables
- [ ] AWK END block handles unclosed blocks at EOF
- [ ] Lychee file patterns in CLI args, not `.lychee.toml` `include` field
- [ ] All Markdown links use exact case matching actual filenames
- [ ] Docker smoke tests use retry loops with `docker logs` on failure
- [ ] Action versions pinned with SHA256 digests (not mutable tags)
- [ ] Workflows use `set -euo pipefail` and `trap` for cleanup
- [ ] File discovery is dynamic (`find`), not hardcoded lists
- [ ] Permissions are minimal (`contents: read` by default)
- [ ] Workflow path filters include the workflow file itself
- [ ] Concurrency control prevents duplicate runs

---

## Related Skills

- [container-and-deployment](./container-and-deployment.md) — Docker builds, Kubernetes config, health checks
- [testing-strategies](./testing-strategies.md) — Test design, regression tests, CI integration
- [supply-chain-security](./supply-chain-security.md) — Dependency auditing, SBOM, reproducible builds
- [mandatory-workflow](./mandatory-workflow.md) — Required checks before commit/push

# Skill: GitHub Actions & CI/CD Best Practices

<!-- trigger: GitHub actions, workflow, ci, cd, pipeline, bash, awk, shell script, continuous integration
     | Patterns for writing robust CI/CD workflows and avoiding common pitfalls | Infrastructure -->

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
- Application code (see [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md))

## Quick Reference: Preventative Measures

**Before pushing workflow changes:**

1. Run `./scripts/check-workflow-hygiene.sh` to validate workflow configuration
2. Run `cargo test --test ci_config_tests` to validate CI consistency
3. Run `./scripts/check-markdown.sh` to validate markdown documentation
4. Review `/docs/adr/ci-cd-preventative-measures.md` for systematic issue prevention

**Common issues automatically detected:**

- Language-specific caching on wrong project type (Python cache on Rust project)
- Stale nightly toolchains (>180 days old)
- Missing required CI validation workflows
- MSRV inconsistency across configuration files
- Markdown linting issues (MD040, MD060, MD013)
- Spell checking issues (technical terms not whitelisted)
- Git hook permission issues (missing executable bit)
- YAML indentation inconsistency (4-space vs 2-space)

---

## TL;DR

**Docker & Version Formats:**

- Docker Hub official images use X.Y tags (e.g., `rust:1.88`), not X.Y.Z
- Normalize versions (1.88.0 → 1.88) when comparing Dockerfile vs Cargo.toml
- X.Y format provides automatic security patches; X.Y.Z requires manual updates

**AWK Portability:**

- AWK multi-line content needs NUL byte delimiters with `printf "%c", 0` (not `"\0"` - mawk incompatible)
- Use POSIX `sub()` instead of gawk's `match()` with capture groups (mawk compatibility)
- Use prefix patterns (`/^```rust/`) instead of exact matches (`/^```rust(,.*)?$/`) for flexibility
- Test AWK scripts on Ubuntu/mawk, not just local gawk

**Bash Best Practices:**

- Always quote variables: `"$var"` prevents word splitting (shellcheck SC2086)
- Bash subshells lose variable modifications - use file-based counters in pipelines
- Add shellcheck validation job to workflows to catch inline script issues

**CI/CD Configuration:**

- Lychee `include` is for URL regex filtering, not file glob patterns
- Always verify case-sensitive filesystem assumptions on Linux
- Documentation links must match actual filenames exactly
- Use retry loops with `Docker logs` dumps for smoke tests
- Pin all action versions with SHA256 digests
- Document magic numbers: timeout values, AWK field offsets, counter file formats

---

## 1. Language-Specific Caching & Configuration Matching

### The Problem: Ecosystem Mismatch

Using caching or tooling from the wrong language ecosystem causes silent failures, cache misses, and cryptic errors.
This is surprisingly common when copying workflow templates.

**Critical Rule:** Workflow configuration MUST match the project's primary language.

### Common Mismatches

#### Python Caching on Rust Project (WRONG)

```yaml
# ❌ WRONG: Python caching for a Rust project
- uses: actions/cache@v4

  with:
    path: ~/.cache/pip           # Python cache directory
    key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements.txt') }}  # Python dependency file

- name: Build Rust project

  run: cargo build               # ← Rust, not Python!

```

**Symptoms:**

- `ERROR: Cache entry deserialization failed, entry ignored`
- `ERROR: Unable to locate executable file: pip`
- Cache always misses (slower CI)
- Workflow succeeds but caching is broken

#### Rust Caching on Node Project (WRONG)

```yaml
# ❌ WRONG: Rust caching for a Node project
- uses: Swatinem/rust-cache@v2

  # Looks for Cargo.toml, finds nothing, silently does nothing

- name: Build Node project

  run: npm run build             # ← Node, not Rust!

```

### Solution: Match Configuration to Project Language

#### Rust Projects (CORRECT)

```yaml
# ✅ CORRECT: Rust-specific caching and tools
- name: Cache Rust dependencies

  uses: Swatinem/rust-cache@5cb072d7354962be830356aa6b146f7612846014 # v2.7.5
  with:
    prefix-key: "rust"

- name: Build Rust project

  run: cargo build --locked

```

**Rust indicators:**

- Has `Cargo.toml` and `Cargo.lock`
- Uses `cargo` commands (`cargo build`, `cargo test`)
- Caches `~/.cargo/`, `target/`
- Dependencies in `Cargo.toml`, not `requirements.txt` or `package.json`

#### Python Projects (CORRECT)

```yaml
# ✅ CORRECT: Python-specific caching
- uses: actions/setup-python@v5

  with:
    python-version: '3.11'
    cache: 'pip'  # Automatically caches pip dependencies

- name: Install dependencies

  run: pip install -r requirements.txt

- name: Build Python project

  run: python setup.py build

```

#### Node Projects (CORRECT)

```yaml
# ✅ CORRECT: Node-specific caching
- uses: actions/setup-node@v4

  with:
    node-version: '20'
    cache: 'npm'  # Automatically caches npm dependencies

- name: Install dependencies

  run: npm ci

- name: Build Node project

  run: npm run build

```

### Detection: Identifying Ecosystem Mismatches

Run these checks on workflow files:

```bash
# Check for language-specific patterns
cd .github/workflows

# For Rust projects, these should NOT appear:
grep -r "pip\|requirements\.txt\|setup\.py" .       # Python
grep -r "npm\|yarn\|package\.json\|node_modules" .  # Node
grep -r "bundle\|Gemfile\|gem install" .            # Ruby
grep -r "mvn\|gradle\|pom\.xml" .                   # Java/Maven

# For Rust projects, these SHOULD appear:
grep -r "cargo\|Cargo\.toml\|rust-cache" .          # Rust

```

**Red flags:**

| Indicator        | Rust                        | Python                              | Node                                  | Java                          |
|------------------|-----------------------------|-------------------------------------|---------------------------------------|-------------------------------|
| Cache paths      | `~/.cargo/`, `target/`      | `~/.cache/pip`                      | `node_modules/`, `.npm/`              | `.m2/`, `.gradle/`            |
| Dependency files | `Cargo.toml`, `Cargo.lock`  | `requirements.txt`, `Pipfile.lock`  | `package.json`, `package-lock.json`   | `pom.xml`, `build.gradle`     |
| Build commands   | `cargo build`               | `pip install`, `python setup.py`    | `npm install`, `npm run build`        | `mvn package`, `gradle build` |
| Test commands    | `cargo test`                | `pytest`, `python -m unittest`      | `npm test`, `jest`                    | `mvn test`, `gradle test`     |

### Avoiding Configuration Drift

**Problem:** Copying workflow templates from other projects introduces wrong-ecosystem configuration.

**Prevention checklist:**

Before committing a new or modified workflow:

- [ ] **Identify project language**: Check repository for `Cargo.toml` (Rust),
  `package.json` (Node), `requirements.txt` (Python), etc.
- [ ] **Verify cache configuration**: Cache paths must match project language (see table above)
- [ ] **Check hash files in cache keys**: Files referenced in `hashFiles()` must exist
- [ ] **Validate tool/action selection**: Use language-appropriate actions
  (`rust-cache` for Rust, `setup-python` for Python, etc.)
- [ ] **Review dependency install commands**: Must match project language
  (`cargo build`, not `pip install`)

- [ ] **Test workflow with cold cache**: Ensure workflow works even when cache misses

**Workflow template validation:**

```bash
# Run this before committing workflow changes
./scripts/validate-workflow-ecosystem.sh

# Sample implementation:
#!/bin/bash
set -euo pipefail

PROJECT_LANG="rust"  # Detected from Cargo.toml presence

# Check workflows for wrong-ecosystem patterns
WRONG_PATTERNS=()
if [ "$PROJECT_LANG" = "rust" ]; then
  WRONG_PATTERNS=("pip" "npm" "bundle" "mvn" "gradle")
fi

for pattern in "${WRONG_PATTERNS[@]}"; do
  if grep -r "$pattern" .github/workflows/ 2>/dev/null; then
    echo "ERROR: Found $pattern in workflows (Rust project, should not have this)"
    exit 1
  fi
done

echo "✓ Workflow ecosystem configuration validated"

```

### SHA Pinning for Actions

**Always pin actions with SHA256 digests**, not mutable tags:

```yaml
# ❌ WRONG: Mutable tags
- uses: actions/checkout@v4
- uses: Swatinem/rust-cache@v2

# ✅ CORRECT: SHA256 digest pinning
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
- uses: Swatinem/rust-cache@5cb072d7354962be830356aa6b146f7612846014 # v2.7.5


```

**Benefits:**

- Reproducible builds (same SHA = same code)
- Security (prevents tag hijacking)
- Stability (no surprise breaking changes)

**How to get SHA digests:**

```bash
# GitHub Actions: Go to releases, find commit SHA
# Or use gh CLI:
gh api repos/actions/checkout/commits/v4.2.2 --jq .sha

```

### Enforcing SHA Pinning with Tests

**Problem:** Developers may forget to pin actions with SHA hashes, introducing
security risks.

**Solution:** Add automated test to enforce SHA pinning:

```rust

// tests/ci_config_tests.rs

#[test]
fn test_all_github_actions_are_sha_pinned() {
    let workflows_dir = std::path::Path::new(".github/workflows");
    let mut unpinned_actions = Vec::new();

    for entry in std::fs::read_dir(workflows_dir).unwrap() {
        let path = entry.unwrap().path();
        let is_workflow = path.extension()
            .map(|ext| ext == "yml" || ext == "yaml")
            .unwrap_or(false);

        if is_workflow {
            let content = std::fs::read_to_string(&path).unwrap();

            for (line_num, line) in content.lines().enumerate() {
                // Match "uses: owner/repo@ref" pattern
                if line.trim().starts_with("uses:") {
                    let action_ref = line.split('@').nth(1);

                    if let Some(ref_part) = action_ref {
                        let ref_value = ref_part
                            .split_whitespace()
                            .next()
                            .unwrap_or("");

                        // Check if it's a SHA (40 hex characters)
                        let is_sha = ref_value.len() == 40
                            && ref_value.chars().all(|c| c.is_ascii_hexdigit());

                        if !is_sha {
                            unpinned_actions.push(format!(
                                "{}:{}: {}",
                                path.display(),
                                line_num + 1,
                                line.trim()
                            ));
                        }
                    }
                }
            }
        }
    }

    assert!(
        unpinned_actions.is_empty(),
        "All GitHub Actions must be pinned to SHA hashes for security.\n\n\
         Unpinned actions found:\n{}\n\n\
         Example fix:\n\
         ❌ WRONG: uses: actions/checkout@v4\n\
         ✅ CORRECT: uses: actions/checkout@11bd7190... # v4.2.2\n\n\
         Get SHA: gh api repos/OWNER/REPO/commits/TAG --jq .sha",
        unpinned_actions.join("\n")
    );
}

```

**Benefits:**

- Catches unpinned actions during `cargo test`
- Prevents security risks from reaching production
- Clear error messages with fix instructions
- Self-documenting security requirement

### SHA Pinning Comments Best Practice

**Always include version comment after SHA:**

```yaml
# ✅ GOOD: SHA with version comment
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
- uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14 # v2.0.15

# ❌ BAD: SHA without context
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683
- uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14


```

**Why version comments matter:**

1. **Human readability** - Know what version the SHA represents
2. **Update tracking** - Easy to see which actions need updating
3. **Dependency auditing** - Tools can parse version from comment
4. **Documentation** - Self-documenting workflow configuration

---

## 2. Docker Version Format Conventions

### The Problem: Docker Hub Tag Format vs Semantic Versioning

Docker Hub uses a shortened version format for official images (e.g., `rust:1.88` instead of `rust:1.88.0`),
which can cause MSRV consistency validation to fail when comparing against `Cargo.toml` versions.

**Critical Rule:** When using Docker Hub official images, use the X.Y format (not X.Y.Z) to match Docker Hub conventions.

### Docker Hub Versioning Patterns

**Official Rust images on Docker Hub:**

```dockerfile
# ✅ CORRECT: Docker Hub format (X.Y)
FROM rust:1.88-bookworm      # Works - official Docker Hub tag
FROM rust:1.87-alpine        # Works - official Docker Hub tag

# ❌ WRONG: Full semantic version (X.Y.Z)
FROM rust:1.88.0-bookworm    # Tag doesn't exist on Docker Hub
FROM rust:1.87.0-alpine      # Tag doesn't exist on Docker Hub

```

**Why Docker Hub uses X.Y tags:**

1. **Automatic security patches**: `rust:1.88` automatically includes `1.88.1`, `1.88.2`, etc.
2. **Simplified maintenance**: No need to update Dockerfiles for patch releases
3. **Convention consistency**: Most official images follow this pattern
4. **Reduced tag proliferation**: Fewer tags to maintain

### Solution: Version Format Normalization

**A. Use shortened format in Dockerfile:**

```dockerfile
# Dockerfile (line 7)
FROM rust:1.88-bookworm AS chef
#          ^^^^
#          X.Y format (not 1.88.0)
```

**B. Normalize versions in validation scripts:**

```bash
# Extract and normalize versions for comparison
DOCKERFILE_VERSION=$(grep '^FROM rust:' Dockerfile | head -1 | sed -E 's/FROM rust:([0-9]+\.[0-9]+).*/\1/')
CARGO_VERSION=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "([0-9]+\.[0-9]+).*/\1/')

# Compare normalized versions (X.Y format)
if [ "$DOCKERFILE_VERSION" != "$CARGO_VERSION" ]; then
  echo "ERROR: Dockerfile Rust version ($DOCKERFILE_VERSION) doesn't match Cargo.toml ($CARGO_VERSION)"
  exit 1
fi

```

**C. CI validation that handles both formats:**

```rust

// tests/ci_config_tests.rs

fn normalize_version(version: &str) -> String {
    // Normalize "1.88.0" -> "1.88" or "1.88" -> "1.88"
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        version.to_string()
    }
}

#[test]
fn test_dockerfile_rust_version_matches_msrv() {
    let dockerfile = read_file("Dockerfile");
    let cargo_toml = read_file("Cargo.toml");

    // Extract versions
    let dockerfile_version = extract_dockerfile_rust_version(&dockerfile);
    let cargo_version = extract_cargo_rust_version(&cargo_toml);

    // Normalize to X.Y format for comparison
    let normalized_dockerfile = normalize_version(&dockerfile_version);
    let normalized_cargo = normalize_version(&cargo_version);

    assert_eq!(
        normalized_dockerfile, normalized_cargo,
        "Dockerfile Rust version must match Cargo.toml rust-version.\n\
         Expected: {} (from Cargo.toml)\n\
         Found: {} (from Dockerfile)\n\
         Note: Docker Hub uses X.Y format (e.g., 1.88, not 1.88.0)\n\
         Fix: Update Dockerfile to use rust:{normalized_cargo}-bookworm",
        normalized_cargo, normalized_dockerfile
    );
}

```

### When to Use Full Versions vs Shortened Versions

| Context                       | Version Format | Example  | Rationale                            |
|-------------------------------|----------------|----------|--------------------------------------|
| `Cargo.toml`                  | Full (X.Y.Z)   | `1.88.0` | Semantic versioning, MSRV spec       |
| `rust-toolchain.toml`         | Full (X.Y.Z)   | `1.88.0` | Exact toolchain pinning              |
| `clippy.toml`                 | Full (X.Y.Z)   | `1.88.0` | MSRV consistency                     |
| Dockerfile (official images)  | Short (X.Y)    | `1.88`   | Docker Hub convention                |
| Custom Docker images          | Full (X.Y.Z)   | `1.88.0` | Explicit version control             |
| GitHub Actions                | Full (X.Y.Z)   | `1.88.0` | Explicit version control             |

### Docker Hub Official Image Patterns

**Rust official image tags:**

```dockerfile
# Primary patterns (CORRECT for Docker Hub)
FROM rust:1.88-bookworm        # Debian 12 base
FROM rust:1.88-alpine          # Alpine base
FROM rust:1.88-slim            # Slim Debian
FROM rust:1.88                 # Default (Debian bookworm)

# NOT available on Docker Hub
FROM rust:1.88.0-bookworm      # ❌ Won't work
FROM rust:1.88.0               # ❌ Won't work

```

**Check available tags:**

```bash
# List available tags for rust image
docker search rust --limit 5
docker pull rust:1.88-bookworm  # Works
docker pull rust:1.88.0-bookworm  # Error: manifest unknown

```

### Benefits of Docker Hub Shortened Format

**Automatic security updates:**

```dockerfile
# Using rust:1.88 automatically pulls latest patch
FROM rust:1.88-bookworm
# Today: Gets 1.88.0
# Tomorrow: Automatically gets 1.88.1 (with security fixes)
# Next week: Automatically gets 1.88.2 (with bug fixes)
```

**vs explicit pinning:**

```dockerfile
# Using full version requires manual updates
FROM rust:1.88.0-bookworm
# Stuck on 1.88.0 forever
# Must manually update Dockerfile to get 1.88.1
```

### When to Pin Exact Versions

**Use exact versions (X.Y.Z) when:**

1. **Reproducible builds are critical** (e.g., audited environments)
2. **Using custom/private registries** (not Docker Hub)
3. **Regulatory compliance requires it** (e.g., FDA, finance)
4. **Building security-sensitive applications** (control every dependency)

**Example: Custom registry with full versions:**

```dockerfile
# Custom registry: Use full versions for reproducibility
FROM my-registry.example.com/rust:1.88.0-bookworm
# Not Docker Hub, so full version is appropriate
```

### Prevention Checklist

Before committing Dockerfile changes:

- [ ] Using official Docker Hub images? Use X.Y format (not X.Y.Z)
- [ ] Using custom registry? Consider full X.Y.Z for reproducibility
- [ ] Version matches MSRV in Cargo.toml (after normalization)?
- [ ] CI validation tests normalize versions before comparison?
- [ ] Comments explain why shortened format is used?

### Documentation Pattern

```dockerfile
# Multi-stage Dockerfile for Signal Fish Server

# Stage 1: Chef - Install cargo-chef for dependency management
# Using bookworm (Debian 12) which has mold in its repositories
# Version 1.88 matches MSRV in Cargo.toml
# Docker Hub uses X.Y format, not X.Y.Z
FROM rust:1.88-bookworm AS chef
#          ^^^^ Shortened format for Docker Hub (automatically includes patches)
RUN cargo install cargo-chef --locked
WORKDIR /app

```

### Real-World Example: The Fix

**Before (WRONG - caused CI failure):**

```dockerfile
# Dockerfile:7
FROM rust:1.88.0-bookworm AS chef
#          ^^^^^^ Full version - tag doesn't exist on Docker Hub
```

**Error:**

```text

ERROR: manifest for rust:1.88.0-bookworm not found

```

**After (CORRECT):**

```dockerfile
# Dockerfile:7
FROM rust:1.88-bookworm AS chef
#          ^^^^ Shortened format - matches Docker Hub convention
```

**Key Insight:** Docker Hub official images use X.Y tags to provide automatic patch updates.
Full X.Y.Z versions are not published for official images.

---

## 3. AWK Multi-Line Content Processing

### The Problem

AWK record separators default to newlines.
When extracting multi-line code blocks (e.g., from Markdown),
using newline-separated output causes each line to become a separate record in the downstream pipeline,
breaking validation logic.

### The Solution: NUL Byte Delimiters

Use NUL bytes (`\0`) as record separators to preserve multi-line content through pipelines.

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

When you need multiple fields (e.g., line number, attributes, content),
use a custom field separator that won't appear in content:

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

### AWK Portability: gawk vs mawk

**Critical Issue**: Ubuntu CI runners use **mawk** by default, not gawk. Many gawk-specific features
are not portable.

```awk
# ❌ WRONG: gawk-specific syntax (fails on mawk)
# mawk doesn't support "\0" escape
printf "%s\0", content
# mawk's match() doesn't support capture groups
if (match($0, /pattern/, arr))

# ✅ CORRECT: POSIX-compatible (works on both gawk and mawk)
# Use %c with value 0 for NUL byte
printf "%s%c", content, 0
# Use sub() instead of match() for extraction
sub(/pattern/, "", var)

```

**Why This Matters:**

- Local development often uses gawk (GNU awk)
- CI/CD runners (Ubuntu) default to mawk (Mike's awk)
- Scripts that work locally can fail in CI due to these differences
- **Always test AWK scripts on Ubuntu/mawk before committing**

### AWK Pattern Portability: ERE vs Prefix Matching

**Critical Issue**: Complex AWK patterns with alternation (e.g., `/^```(Rust|Rust)(,.*)?$/`)
can behave differently across AWK implementations. Prefix patterns are more portable.

#### The Problem: Alternation and Optional Groups

```awk
# ❌ FRAGILE: Alternation with optional suffix (complex to maintain)
/^```[Rr]ust(,.*)?$/ {
  # Matches: ```rust, ```Rust, ```rust,ignore, ```Rust,ignore
  # BUT: Doesn't match ```rust ignore (space instead of comma)
  # AND: Fails on ```rust,no_run or other valid fence formats
}

```

**Issues with exact pattern matching:**

1. **Brittle fence format assumptions** - Assumes comma separator, fails on spaces
2. **Maintenance burden** - Adding new fence formats requires pattern updates
3. **Test coverage gaps** - Hard to test all possible fence format variations
4. **Portability concerns** - Complex regex can behave differently across AWK versions

#### The Solution: Prefix Patterns with Flexible Attribute Extraction

```awk
# ✅ ROBUST: Prefix pattern (matches any fence format)
/^```[Rr]ust/ {
  in_block = 1
  block_start = NR
  content = ""
  attributes = $0

  # Extract attributes using POSIX sub() (portable across mawk/gawk)
  attrs = $0
  sub(/^```[Rr]ust,?/, "", attrs)  # Remove prefix, keep attributes
  # Now attrs contains: "ignore", "no_run", "", "ignore no_run", etc.

  next
}

```

**Benefits of prefix matching:**

1. **Flexible fence formats** - Works with: `Rust,ignore`, `Rust ignore`, `Rust,no_run`, etc.
2. **Future-proof** - New attribute formats automatically supported
3. **Portable** - Uses POSIX `sub()` instead of gawk-specific `match()`
4. **Maintainable** - Single pattern handles all variations

#### Real-World Example: The Fix

**Before (FRAGILE):**

```awk
# doc-validation.yml:210 (BEFORE)
/^```[Rr]ust(,.*)?$/ {
  # Only matches: ```rust and ```rust,<attributes>
  # Fails on: ```rust ignore (space separator)
  # Fails on: ```rust,ignore no_run (multiple attributes)
}

```

**Problems encountered:**

- Fence format: ` ```Rust ignore` (space, not comma) didn't match
- Test suite had 119 code blocks with various fence formats
- Pattern needed constant updates for new attribute styles

**After (ROBUST):**

```awk
# doc-validation.yml:210 (AFTER)
/^```[Rr]ust/ {
  # Matches ANY fence starting with ```rust or ```Rust
  # Handles all attribute formats automatically
  in_block = 1
  attrs = $0
  sub(/^```[Rr]ust,?/, "", attrs)  # Flexible attribute extraction
  next
}

```

**Results:**

- All 119 test code blocks now validate correctly
- Works with: `Rust,ignore`, `Rust ignore`, `Rust,no_run`, `Rust,edition2021`
- Future fence formats automatically supported
- No need to update pattern for new attribute styles

#### When to Use Prefix Patterns vs Exact Matching

| Scenario                       | Pattern Type | Example                | Rationale                      |
|--------------------------------|--------------|------------------------|--------------------------------|
| Code fence detection           | Prefix       | `/^```[Rr]ust/`        | Flexible attribute handling    |
| Closing fence                  | Exact        | `/^```$/`              | Must match exactly (no prefix) |
| Language detection (no attrs)  | Exact        | `/^```Rust$/`          | Only plain code blocks         |
| Strict validation              | Exact        | `/^```Rust,ignore$/`   | Enforce specific format        |
| General extraction             | Prefix       | `/^```python/`         | Handle any Python fence        |

#### Testing AWK Patterns

**Validate pattern portability:**

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

echo '```rust ignore no_run' | awk '
  /^```[Rr]ust/ {
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)
    print "attrs: [" attrs "]"
  }
'
# Output: attrs: [ ignore no_run]
```

**Add comprehensive fence format tests:**

```bash
# Test all fence format variations
test_fences=(
  "```rust"
  "```Rust"
  "```rust,ignore"
  "```Rust,ignore"
  "```rust ignore"
  "```rust,no_run"
  "```rust ignore no_run"
  "```rust,edition2021"
)

for fence in "${test_fences[@]}"; do
  result=$(echo "$fence" | awk '/^```[Rr]ust/ {print "MATCH"}')
  if [ "$result" = "MATCH" ]; then
    echo "✓ $fence"
  else
    echo "✗ $fence"
  fi
done

```

#### Documentation Pattern

```bash
# .github/workflows/doc-validation.yml

# Extract Rust code blocks from markdown files
# Uses prefix pattern /^```[Rr]ust/ instead of exact match for flexibility:
# - Handles both ```rust and ```Rust (case-insensitive)
# - Works with any attribute format: rust,ignore OR rust ignore
# - Future-proof: new attribute styles automatically supported
# - Portable: POSIX-compatible pattern works on gawk and mawk
awk '
  # Match opening fence with prefix pattern (flexible)
  /^```[Rr]ust/ {
    in_block = 1
    attrs = $0
    sub(/^```[Rr]ust,?/, "", attrs)  # Remove prefix, keep attributes
    next
  }
  # ... rest of AWK script
'

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
    # POSIX-compatible: Use %c format instead of "\0" escape
    printf "%s%c", content, 0
  }
}

# Case-insensitive matching
/^```[Rr]ust(,.*)?$/ {  # Matches both "rust" and "Rust"
  in_block = 1
}

# Extract attributes with POSIX-compatible sub() instead of match()
# ❌ WRONG (gawk-only):
if (match($0, /```rust,(.*)/, arr)) {
  attrs = arr[1]
}

# ✅ CORRECT (POSIX-compatible):
attrs = $0
sub(/^```[Rr]ust,?/, "", attrs)  # Remove prefix, leaving only attributes

```

---

## 4. Shellcheck Integration in CI/CD

### Self-Validating Workflows

GitHub Actions workflows should validate their own inline bash scripts using shellcheck:

```yaml

jobs:
  shellcheck-workflow:
    name: Shellcheck Workflow Scripts
    runs-on: ubuntu-latest
    steps:

      - uses: actions/checkout@<SHA>

      - name: Install shellcheck

        run: sudo apt-get update && sudo apt-get install -y shellcheck

      - name: Extract and validate inline shell scripts

        run: |
          set -euo pipefail
          TEMP_DIR=$(mktemp -d)
          trap 'rm -rf "$TEMP_DIR"' EXIT

          # Extract inline scripts from workflow YAML
          awk '/name: My Script Step/,/^      - name:/ {
            if (/run: \|/) { in_script=1; next }
            if (in_script && /^      - name:/) { exit }
            if (in_script && /^          /) { print substr($0, 11) }
          }' .github/workflows/my-workflow.yml > "$TEMP_DIR/script.sh"

          # Validate with shellcheck
          if ! shellcheck -s bash "$TEMP_DIR/script.sh"; then
            echo "✗ Shellcheck found issues"
            exit 1
          fi

```

### Variable Quoting Best Practices

Always quote variables to prevent word splitting and glob expansion:

```bash
# ❌ WRONG: Unquoted variables (shellcheck SC2086)
file=$1
cat $file                    # Fails if file has spaces
rm $TEMP_DIR/*.txt           # Glob expansion issues

# ✅ CORRECT: Quoted variables
file="$1"
cat "$file"                  # Works with spaces in filename
rm "$TEMP_DIR"/*.txt         # Quote variable, not glob

# ✅ CORRECT: Arrays for multiple arguments
files=("file1.txt" "file with spaces.txt")
cat "${files[@]}"            # Proper array expansion

```

### Common Shellcheck Warnings in CI

#### SC2086: Unquoted variable expansion

```bash
# ❌ WRONG
total=$COUNTER
file=$FILE_PATH

# ✅ CORRECT
total="$COUNTER"
file="$FILE_PATH"

```

#### SC2034: Unused variable

```bash
# In documentation examples, unused variables are often acceptable
# Suppress with comment:
# shellcheck disable=SC2034
EXAMPLE_VAR="for documentation only"

```

#### SC2046: Unquoted command substitution

```bash
# ❌ WRONG
files=$(find . -name "*.txt")
cat $files

# ✅ CORRECT
while IFS= read -r file; do
  cat "$file"
done < <(find . -name "*.txt")

```

### Shellcheck + AWK Limitations

Shellcheck validates Bash syntax but does **not** validate AWK syntax
embedded in heredocs or inline scripts.

```text
# Shellcheck will NOT catch AWK syntax errors here:
awk '
  BEGIN { print "hello" }    # Shellcheck ignores this
  { invalid_awk_syntax }     # Shellcheck won't catch this
' file.txt

```

**Solution**: AWK scripts are validated through actual execution in CI.
If an AWK script has syntax errors, the workflow will fail at runtime.

### Variable Naming Conventions

Use consistent naming to improve shellcheck compliance and readability:

```bash
# ✅ CORRECT: Clear naming conventions
# Constants and cross-script values: UPPERCASE
TEMP_DIR=$(mktemp -d)
COUNTER_FILE="$TEMP_DIR/counters"

# Local variables and loop iterators: lowercase
for file in *.md; do
  total=$((total + 1))
  validate "$file"
done

```

---

## 5. Bash Subshells & Variable Scope

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

## 6. Lychee Link Checker Configuration and Best Practices

### The Problem

Lychee's `include` field in `.lychee.toml` is for **URL regex filtering**, not file glob patterns.
Using file globs in `include` silently fails to filter anything.

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

## 7. Case-Sensitive Filesystem Issues

### The Problem

Windows and macOS default to case-insensitive filesystems, but Linux (including CI runners) is case-sensitive.
Links and imports that work locally may break in CI.

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

## 8. Docker Smoke Test Patterns

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

## 9. Action Version Pinning

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

## 10. Magic Numbers & Documentation

### Always Document Timeout Values

```yaml
# ❌ WRONG: Unexplained magic number
jobs:
  test:
    timeout-minutes: 15

# ✅ CORRECT: Documented reasoning
jobs:
  test:
    timeout-minutes: 15  # Generous timeout for building docs with all features

```

### Document AWK Field Extraction

```awk
# ❌ WRONG: Magic number without context
print substr($0, 11)

# ✅ CORRECT: Explain the calculation
# Extract script content: skip the 10-space indentation
# (6 spaces for YAML step level + 4 for script content)
# plus line number + tab from workflow YAML structure = 11 characters to skip
print substr($0, 11)

```

### Document Counter File Formats

```bash
# ❌ WRONG: Unexplained file format
echo "0 0 0 0" > "$COUNTER_FILE"

# ✅ CORRECT: Document the schema
# Counter file format: 4 space-separated integers (total validated skipped failed)
# Example: "10 7 2 1" means 10 total blocks, 7 validated, 2 skipped, 1 failed
echo "0 0 0 0" > "$COUNTER_FILE"

```

---

## 11. Workflow Path Filtering Best Practices

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

## 12. Minimal Permissions (Security)

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

## 13. Common CI Anti-Patterns

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

## 14. Debugging Workflow Failures

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

## 15. Configuration File Validation Tests (Preventative Pattern)

### The Pattern: Test Configuration Consistency

Instead of waiting for CI to fail, use data-driven tests to validate configuration consistency:

```rust

// tests/ci_config_tests.rs

#[test]
fn test_msrv_consistency_across_config_files() {
    // Single source of truth: Cargo.toml rust-version
    let msrv = extract_toml_version(&cargo_content, "rust-version");

    // Validate rust-toolchain.toml
    let toolchain_version = extract_yaml_version(&toolchain_content, "channel");
    assert_eq!(
        toolchain_version, msrv,
        "rust-toolchain.toml channel must match Cargo.toml rust-version"
    );

    // Validate clippy.toml
    let clippy_msrv = extract_toml_version(&clippy_content, "msrv");
    assert_eq!(clippy_msrv, msrv, "clippy.toml msrv must match");

    // Validate Dockerfile
    assert!(dockerfile_version == msrv, "Dockerfile Rust version must match");
}

#[test]
fn test_required_ci_workflows_exist() {
    let required_workflows = vec![
        "ci.yml",
        "yaml-lint.yml",
        "actionlint.yml",
        "unused-deps.yml",
        "workflow-hygiene.yml",
    ];

    for workflow in required_workflows {
        assert!(
            workflows_dir.join(workflow).exists(),
            "Required workflow missing: {}", workflow
        );
    }
}

#[test]
fn test_no_language_specific_cache_mismatch() {
    let root = repo_root();
    let is_rust_project = root.join("Cargo.toml").exists();

    // Detect requirements-*.txt variants (e.g., requirements-docs.txt for MkDocs)
    let has_any_requirements_txt = root
        .read_dir()
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("requirements") && name.ends_with(".txt")
            })
        })
        .unwrap_or(false);
    let is_python_project = root.join("requirements.txt").exists()
        || root.join("Pipfile").exists()
        || root.join("pyproject.toml").exists()
        || has_any_requirements_txt;

    for workflow_file in workflow_files {
        let content = read_file(&workflow_file);

        // Check for Python caching on non-Python Rust projects
        if !is_python_project
            && is_rust_project
            && (content.contains("cache: 'pip'") || content.contains("cache: pip"))
        {
            panic!("Python pip cache found in Rust project workflow");
        }
    }
}

#[test]
fn test_markdown_files_have_language_identifiers() {
    for file in find_markdown_files() {
        for (line_num, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                let fence = line.trim_start().trim_start_matches('`').trim();
                assert!(
                    !fence.is_empty(),
                    "{}:{}: Missing language identifier (MD040)",
                    file.display(), line_num + 1
                );
            }
        }
    }
}

#[test]
fn test_scripts_are_executable() {
    for script in find_scripts(&["scripts", ".githooks"]) {
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode();
            let is_executable = mode & 0o111 != 0;

            assert!(
                is_executable,
                "{} is not executable.\nFix: chmod +x {} && git update-index --chmod=+x {}",
                script.display(), script.display(), script.display()
            );
        }
    }
}

#[test]
fn test_typos_config_exists_and_is_valid() {
    assert!(
        Path::new(".typos.toml").exists(),
        ".typos.toml is required for spell checking"
    );

    let content = read_file(".typos.toml");
    assert!(
        content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section"
    );
}

```

### Benefits of Configuration Tests

**Early Detection:**

- Catch issues during `cargo test` (before pushing)
- Fast feedback loop (< 1 second for all tests)
- No waiting for CI to fail

**Actionable Errors:**

```rust

// Error messages include fix instructions
assert_eq!(
    toolchain_version, msrv,
    "rust-toolchain.toml channel must match Cargo.toml rust-version.\n\
     Expected: {msrv}\n\
     Found: {toolchain_version}\n\
     Fix: Update rust-toolchain.toml to use channel = \"{msrv}\""
);

```

**Prevents Regression:**

- Once a configuration issue is fixed, add a test
- Test prevents the same issue from recurring
- Documents configuration requirements

**Self-Documenting:**

- Test names describe what's validated
- Tests serve as executable documentation
- Easy to add new validation rules

### When to Add Configuration Tests

Add a test whenever you:

1. **Fix a CI configuration issue** - Prevent recurrence
2. **Add a new configuration file** - Validate it exists and is correct
3. **Establish a consistency requirement** - MSRV across files, naming conventions
4. **Add a new required workflow** - Test that it exists
5. **Add a coding standard** - Markdown linting, spell checking

### Pattern: Test Configuration Files, Not Content

```rust
// ✅ GOOD: Test configuration requirements
#[test]
fn test_markdownlint_config_exists() {
    assert!(Path::new(".markdownlint.json").exists());
}

// ❌ BAD: Test implementation details
#[test]
fn test_markdownlint_config_exact_content() {
    let content = read_file(".markdownlint.json");
    assert_eq!(content, r#"{"MD040": true, ...}"#); // Too brittle
}

```

### Integration with CI

Configuration tests run as part of standard test suite:

```bash
# Local development
cargo test --test ci_config_tests

# CI workflow
cargo test --all-features  # Includes ci_config_tests

```

**Fast execution:**

- No external dependencies (pure Rust file reading)
- No network calls
- Parallel test execution
- Total time: < 1 second for all tests

---

## 16. Scheduled Workflows for Proactive Monitoring

### The Problem: Reactive vs Proactive Security

Running security audits and maintenance tasks only on code changes is reactive:

```yaml
# ❌ REACTIVE: Only runs when code changes
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

```

**Issues:**

- New CVEs published overnight won't trigger workflow
- Advisory databases update independently of code
- Stale dependencies accumulate between changes
- Link rot occurs in documentation
- Nightly toolchains become outdated

### The Solution: Scheduled Workflows

Add cron schedules for proactive monitoring:

```yaml
# ✅ PROACTIVE: Runs on code changes AND on schedule
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    # Daily security audit at noon UTC to catch new CVEs

    - cron: '0 12 * * *'


```

### When to Use Scheduled Workflows

| Workflow Type                | Recommended Schedule | Rationale                              |
|------------------------------|----------------------|----------------------------------------|
| Security audits (cargo-deny) | Daily                | New CVEs published frequently          |
| Dependency updates           | Weekly               | Balance freshness with stability       |
| Link checking                | Weekly               | Catch external link rot                |
| Workflow hygiene             | Weekly               | Detect stale toolchains                |
| Unused dependencies          | Weekly               | Proactive dependency cleanup           |
| Documentation validation     | Weekly               | Catch formatting drift                 |

### Real-World Example: Daily Security Audits

From `/workspaces/signal-fish-server/.github/workflows/ci.yml`:

```yaml

name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    # Daily security audit at noon UTC to catch new CVEs

    - cron: '0 12 * * *'

jobs:
  deny:
    name: Dependency Audit
    runs-on: ubuntu-latest
    # This job handles all security audits including vulnerabilities (cargo-audit),
    # licenses, banned dependencies, and source verification (cargo-deny).
    # Runs on push/PR and daily via schedule (see workflow triggers).
    steps:

      - name: Checkout repository

        uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2

      - name: Run cargo-deny

        uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14 # v2.0.15
        with:
          arguments: --all-features

```

**Benefits:**

- Detects new vulnerabilities published overnight
- Catches RustSec advisory database updates
- Alerts team to security issues even without code changes
- Proactive security posture instead of reactive

### Common Cron Schedules

```yaml
# Every day at noon UTC
- cron: '0 12 * * *'

# Every Monday at midnight UTC
- cron: '0 0 * * 1'

# Every week on Monday at 6 AM UTC
- cron: '0 6 * * 1'

# First day of every month
- cron: '0 0 1 * *'

# Every 6 hours
- cron: '0 */6 * * *'


```

### Preventing Alert Fatigue

**Use different schedules for different priorities:**

```yaml
# High priority: Daily security audits
security-audit:
  schedule:

    - cron: '0 12 * * *'  # Daily at noon

# Medium priority: Weekly dependency cleanup
unused-deps:
  schedule:

    - cron: '0 0 * * 1'  # Weekly on Monday

# Low priority: Monthly workflow hygiene
workflow-hygiene:
  schedule:

    - cron: '0 6 1 * *'  # First of month at 6 AM


```

**Add clear comments explaining schedule choices:**

```yaml

schedule:
  # Daily security audit at noon UTC to catch new CVEs
  # More frequent than code changes because advisory DB updates independently

  - cron: '0 12 * * *'


```

### Notification Configuration

**For scheduled workflows that may fail:**

```yaml

jobs:
  security-audit:
    runs-on: ubuntu-latest
    steps:

      - name: Run cargo-deny

        uses: EmbarkStudios/cargo-deny-action@<SHA> # v2.0.15
        with:
          arguments: --all-features

      # Send notification on failure (scheduled runs only)

      - name: Notify on failure

        if: failure() && github.event_name == 'schedule'
        uses: actions/github-script@<SHA>
        with:
          script: |
            github.rest.issues.create({
              owner: context.repo.owner,
              repo: context.repo.repo,
              title: '🚨 Scheduled security audit failed',
              body: 'Daily security audit detected new vulnerabilities.\n\n\
                     Workflow: ${{ github.server_url }}/${{ github.repository }}/\
                     actions/runs/${{ github.run_id }}',
              labels: ['security', 'automated']
            })

```

### Best Practices for Scheduled Workflows

1. **Document why the schedule exists** - Comment explaining frequency choice
2. **Use appropriate frequency** - Balance freshness with noise
3. **Different schedules for different priorities** - Daily for security, weekly for cleanup
4. **Include workflow trigger in comments** - Document that job runs on schedule
5. **Test scheduled logic** - Ensure workflow behaves correctly for cron triggers
6. **Stagger schedules** - Don't run everything at midnight UTC (spread load)
7. **Configure notifications** - Alert on failures for scheduled runs

### Testing Scheduled Workflow Logic

```yaml
# Test both push/PR and scheduled triggers
- name: Run security audit

  run: |
    if [ "${{ github.event_name }}" = "schedule" ]; then
      echo "Running scheduled security audit (daily check for new CVEs)"
    else
      echo "Running security audit (triggered by code change)"
    fi
    cargo deny check advisories

```

### Preventing Duplicate Runs

**Use concurrency control to prevent overlap:**

```yaml

concurrency:
  group: ${{ github.workflow }}-${{ github.event_name }}
  cancel-in-progress: true

```

This ensures that:

- Scheduled run won't overlap with push/PR runs
- Multiple scheduled runs won't queue up if workflow is slow
- Resources are used efficiently

---

## 17. Extracting Inline Scripts to External Files

### The Problem: AWK in YAML Breaks Shellcheck

Inline AWK programs in YAML `run: |` blocks cause shellcheck
failures when the AWK code contains apostrophes or single quotes.
Shellcheck parses the entire block as bash and misinterprets AWK
quoting boundaries.

### The Solution: External Script Files

Extract AWK programs (especially those > 10 lines) to
`.github/scripts/`:

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

**Benefits:**

- Eliminates shellcheck false positives from AWK quoting
- AWK files can be validated independently
  (`awk -f script.awk /dev/null`)
- Easier to test, version, and review
- `scripts/validate-ci.sh` validates external AWK files
  automatically

### When to Extract vs Inline

| AWK Program Size          | Recommendation   | Rationale                       |
|---------------------------|------------------|---------------------------------|
| 1-5 lines, no quotes     | Inline OK        | Simple enough to keep inline    |
| 5-10 lines                | Consider extract | Readability benefit             |
| > 10 lines                | Always extract   | Maintainability and testability |
| Any size with apostrophes | Always extract   | Shellcheck compatibility        |

---

## 18. Docker-Based Actions and Toolchain Overrides

### The Problem

Some GitHub Actions (e.g., `cargo-deny-action`, `cargo-audit-action`) run
inside their own Docker container with a pre-installed Rust toolchain. If the
repository's `rust-toolchain.toml` pins a specific version, rustup inside the
container tries to install that version -- which may not be available, causing
the action to fail.

### The Solution: `RUSTUP_TOOLCHAIN` Environment Variable

Override `rust-toolchain.toml` inside the container by setting `RUSTUP_TOOLCHAIN`:

```yaml
# ✅ CORRECT: Override toolchain for Docker-based actions
- name: Run cargo-deny
  uses: EmbarkStudios/cargo-deny-action@<SHA> # v2.0.15
  env:
    RUSTUP_TOOLCHAIN: stable  # Use container's stable toolchain
  with:
    arguments: --all-features
```

### When to Use This Pattern

| Action Type | Needs Override? | Rationale |
|-------------|-----------------|-----------|
| Metadata-only (cargo-deny, cargo-audit) | Yes | Only reads `Cargo.lock`/`Cargo.toml`, no compilation |
| Compilation actions (build, test) | No | Needs exact toolchain version for correctness |
| Linting actions (clippy) | No | Lint results depend on Rust version |
| Formatting actions (rustfmt) | Depends | Format output may vary by version |

**Key Insight:** Actions that only inspect dependency metadata and lock files
(not compile code) do not need the project's exact Rust version. Overriding
with `stable` avoids toolchain installation failures in Docker containers.

---

## Agent Checklist

### AWK Best Practices

- [ ] AWK multi-line extraction uses NUL byte delimiters (`printf "%s%c", content, 0`)
- [ ] AWK uses POSIX-compatible `printf "%c", 0` instead of `"\0"` (mawk compatibility)
- [ ] AWK uses `sub()` instead of `match()` with capture groups (mawk compatibility)
- [ ] AWK END block handles unclosed blocks at EOF
- [ ] AWK scripts tested on Ubuntu/mawk, not just local gawk
- [ ] AWK programs > 10 lines extracted to `.github/scripts/` (not inline in YAML)
- [ ] AWK programs containing apostrophes always in external files

### Shellcheck & Bash Best Practices

- [ ] All variables quoted: `"$var"` not `$var` (prevents SC2086)
- [ ] Workflows include shellcheck validation of inline scripts
- [ ] Variable naming: UPPERCASE for constants, lowercase for locals
- [ ] Bash pipeline counters use file-based propagation, not subshell variables
- [ ] Workflows use `set -euo pipefail` and `trap` for cleanup
- [ ] File discovery is dynamic (`find`), not hardcoded lists

### GitHub Actions Best Practices

- [ ] Lychee file patterns in CLI args, not `.lychee.toml` `include` field
- [ ] All Markdown links use exact case matching actual filenames
- [ ] Docker smoke tests use retry loops with `Docker logs` on failure
- [ ] Action versions pinned with SHA256 digests (not mutable tags)
- [ ] SHA pins include version comment (e.g., `# v4.2.2`)
- [ ] Permissions are minimal (`contents: read` by default)
- [ ] Workflow path filters include the workflow file itself
- [ ] Concurrency control prevents duplicate runs
- [ ] Timeout values documented with comments explaining duration
- [ ] Security audits run on schedule (daily), not just on code changes
- [ ] Scheduled workflows have clear comments explaining frequency choice
- [ ] Docker-based actions that only inspect metadata use `RUSTUP_TOOLCHAIN: stable` env override
- [ ] Workflow YAML files use 2-space indentation (matching `.yamllint.yml`)

---

## Related Skills

- [ci-cd-troubleshooting](./ci-cd-troubleshooting.md) — Diagnosing CI failures, cache errors, configuration mismatches
- [container-and-deployment](./container-and-deployment.md) — Docker builds, Kubernetes config, health checks
- [testing-strategies](./testing-strategies.md) — Test design, regression tests, CI integration
- [supply-chain-security](./supply-chain-security.md) — Dependency auditing, SBOM, reproducible builds
- [msrv-and-toolchain-management](./msrv-and-toolchain-management.md) — MSRV updates and toolchain consistency
- [mandatory-workflow](./mandatory-workflow.md) — Required checks before commit/push

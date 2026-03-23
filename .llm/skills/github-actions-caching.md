# Skill: GitHub Actions Caching & Action Versioning

<!--
  trigger: GitHub actions, caching, cache, rust-cache, action versions,
  dockerfile, Docker version, msrv
  | Patterns for language-specific caching, explicit-version action refs, and Docker version formats | Infrastructure
-->

**Trigger**: When configuring caching in workflows, pinning action versions,
or dealing with Docker Hub image tag formats.

---

## When to Use

- Setting up or auditing caching in GitHub Actions workflows
- Pinning action versions to explicit version tags
- Dealing with `rust:X.Y` vs `rust:X.Y.Z` format in Dockerfiles
- Validating MSRV consistency between `Cargo.toml` and `Dockerfile`
- Debugging cache misses or wrong-ecosystem caching

## When NOT to Use

- Container build and deployment patterns (see [Container Docker](./container-docker.md))
- Scheduled workflow patterns (see [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md))

## TL;DR

- Match cache configuration to project language (Python cache on Rust project = silent failure)
- Always use explicit action version tags (for example `@v4.2.2`), never moving refs
- Docker Hub official images use X.Y tags (`rust:1.88`), not X.Y.Z — normalize when comparing

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
    key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements.txt') }}

- name: Build Rust project
  run: cargo build               # ← Rust, not Python!
```

**Symptoms:**

- `ERROR: Cache entry deserialization failed, entry ignored`
- `ERROR: Unable to locate executable file: pip`
- Cache always misses (slower CI)

#### Rust Caching on Node Project (WRONG)

```yaml
# ❌ WRONG: Rust caching for a Node project
- uses: Swatinem/rust-cache@v2
  # Looks for Cargo.toml, finds nothing, silently does nothing

- name: Build Node project
  run: npm run build             # ← Node, not Rust!
```

### Solution: Match Configuration to Project Language

```yaml
# ✅ CORRECT: Rust-specific caching
- name: Cache Rust dependencies
  uses: Swatinem/rust-cache@5cb072d7354962be830356aa6b146f7612846014 # v2.7.5
  with:
    prefix-key: "rust"

- name: Build Rust project
  run: cargo build --locked
```

```yaml
# ✅ CORRECT: Python-specific caching
- uses: actions/setup-python@v5
  with:
    python-version: '3.11'
    cache: 'pip'
- name: Install dependencies
  run: pip install -r requirements.txt
```

```yaml
# ✅ CORRECT: Node-specific caching
- uses: actions/setup-node@v4
  with:
    node-version: '20'
    cache: 'npm'
- name: Install dependencies
  run: npm ci
```

### Detection: Identifying Ecosystem Mismatches

```bash
# For Rust projects, these should NOT appear in .github/workflows:
grep -r "pip\|requirements\.txt\|setup\.py" .github/workflows/  # Python
grep -r "npm\|yarn\|package\.json\|node_modules" .github/workflows/  # Node
grep -r "bundle\|Gemfile\|gem install" .github/workflows/  # Ruby

# For Rust projects, these SHOULD appear:
grep -r "cargo\|Cargo\.toml\|rust-cache" .github/workflows/
```

**Red flags by ecosystem:**

| Indicator        | Rust                        | Python                              | Node                                  |
|------------------|-----------------------------|-------------------------------------|---------------------------------------|
| Cache paths      | `~/.cargo/`, `target/`      | `~/.cache/pip`                      | `node_modules/`, `.npm/`              |
| Dependency files | `Cargo.toml`, `Cargo.lock`  | `requirements.txt`, `Pipfile.lock`  | `package.json`, `package-lock.json`   |
| Build commands   | `cargo build`               | `pip install`, `python setup.py`    | `npm install`, `npm run build`        |

### Prevention Checklist

- [ ] Identify project language (check `Cargo.toml`, `package.json`, `requirements.txt`)
- [ ] Cache paths match project language
- [ ] Files referenced in `hashFiles()` exist
- [ ] Tool/action selection is language-appropriate
- [ ] Dependency install commands match project language
- [ ] Workflow tested with cold cache

---

## 2. Explicit Version Refs for Actions

### Always Use Explicit Version Tags

```yaml
# ❌ WRONG: Moving refs can change between runs
- uses: dtolnay/rust-toolchain@stable
- uses: taiki-e/install-action@v2

# ✅ CORRECT: Explicit version tags are readable and auditable
- uses: actions/checkout@v6.0.2
- uses: Swatinem/rust-cache@v2.8.2
```

**Trade-off:** Version tags are not immutable like SHAs.
**Compensating controls:** ban moving refs (`stable/main/master/latest`), keep refs
consistent across workflows, and use Dependabot + CI checks for updates.

### Enforcing Explicit-Version Policy with Tests

```rust
// tests/ci_config_tests.rs
#[test]
fn test_github_actions_use_version_refs_not_commit_hashes() {
    let workflows_dir = std::path::Path::new(".github/workflows");
    let mut violations = Vec::new();

    for entry in std::fs::read_dir(workflows_dir).unwrap() {
        let path = entry.unwrap().path();
        let is_workflow = path.extension()
            .map(|ext| ext == "yml" || ext == "yaml")
            .unwrap_or(false);

        if is_workflow {
            let content = std::fs::read_to_string(&path).unwrap();
            for (line_num, line) in content.lines().enumerate() {
                if line.trim().starts_with("uses:") {
                    let action_ref = line.split('@').nth(1);
                    if let Some(ref_part) = action_ref {
                        let ref_value = ref_part.split_whitespace().next().unwrap_or("");
                        let is_commit_hash = ref_value.len() == 40
                            && ref_value.chars().all(|c| c.is_ascii_hexdigit());
                        let is_moving_ref = matches!(
                            ref_value,
                            "stable" | "beta" | "nightly" | "main" | "master" | "latest"
                        );
                        let is_version_tag =
                            ref_value.starts_with('v') && ref_value.len() > 1;

                        if is_commit_hash || is_moving_ref || !is_version_tag {
                            violations.push(format!("{}:{}: {}", path.display(), line_num + 1, line.trim()));
                        }
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "GitHub Actions must use explicit version tags and disallow commit hashes/moving refs.\n\
         Violations:\n{}\n\
         Fix: uses: owner/repo@vX.Y.Z",
        violations.join("\n")
    );
}
```

---

## 3. Docker Version Format Conventions

### The Problem: Docker Hub Tag Format vs Semantic Versioning

Docker Hub official images use a shortened version format (`rust:1.88` not `rust:1.88.0`).
Using full X.Y.Z versions fails with `manifest unknown`.

**Critical Rule:** Use X.Y format for Docker Hub official images.

```dockerfile
# ✅ CORRECT: Docker Hub format (X.Y)
FROM rust:1.88-bookworm      # Works

# ❌ WRONG: Full semantic version (X.Y.Z)
FROM rust:1.88.0-bookworm    # Error: manifest unknown
```

**Why X.Y:** Automatically pulls latest patch releases (security fixes included).

### Normalize When Comparing to Cargo.toml

```bash
DOCKERFILE_VERSION=$(grep '^FROM rust:' Dockerfile | head -1 | sed -E 's/FROM rust:([0-9]+\.[0-9]+).*/\1/')
CARGO_VERSION=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "([0-9]+\.[0-9]+).*/\1/')

if [ "$DOCKERFILE_VERSION" != "$CARGO_VERSION" ]; then
  echo "ERROR: Dockerfile ($DOCKERFILE_VERSION) doesn't match Cargo.toml ($CARGO_VERSION)"
  exit 1
fi
```

### Version Format by Context

| Context                       | Format       | Example  | Rationale                     |
|-------------------------------|--------------|----------|-------------------------------|
| `Cargo.toml`                  | Full (X.Y.Z) | `1.88.0` | Semantic versioning, MSRV     |
| `rust-toolchain.toml`         | Full (X.Y.Z) | `1.88.0` | Exact toolchain pinning       |
| Dockerfile (official images)  | Short (X.Y)  | `1.88`   | Docker Hub convention         |
| Custom Docker images/registry | Full (X.Y.Z) | `1.88.0` | Explicit version control      |

---

## Related Skills

- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Workflow structure, permissions, path filters
- [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md) — Shellcheck, Bash best practices
- [GitHub Actions Config Tests](./github-actions-config-tests.md) — Automated validation of CI configuration
- [GitHub Actions Release](./github-actions-release.md) — Release gating, cargo --locked, preflight hardening
- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures and cache errors

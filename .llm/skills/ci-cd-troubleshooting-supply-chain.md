# Skill: CI/CD Troubleshooting - Supply Chain & Stale Reference Patterns

<!--
  trigger: action ref policy, supply chain, dockerfile copy, stale script,
  action not found, continue-on-error, dockerbuildkit warning, secrets env,
  workflow script missing
  | Patterns 21-25: Dockerfile COPY stale paths, explicit action version refs,
  stale workflow scripts, invalid/moving action refs, Dockerfile false-positive
  security warnings
  | Infrastructure
-->

**Trigger**: When debugging Dockerfile `COPY` path errors, invalid or moving
GitHub Action refs, stale workflow script references, or Dockerfile BuildKit
`SecretsUsedInArgOrEnv` false-positive warnings.

See also: [CI CD Troubleshooting Ecosystem](./ci-cd-troubleshooting-ecosystem.md),
[CI CD Troubleshooting Scripts](./ci-cd-troubleshooting-scripts.md),
[CI CD Troubleshooting Links](./ci-cd-troubleshooting-links.md),
[CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md)

---

## TL;DR

- **Stale Dockerfile COPY**: Audit all `COPY`/`ADD` instructions when removing directories
- **Action refs policy**: All `uses:` references must use explicit version tags
  (for example `@v6.0.3`), not commit SHAs and not moving refs (`@stable`, `@main`)
- **Action syntax policy**: Treat malformed remote refs (missing `@ref`, empty ref,
  or missing `owner/repo`) as violations, not as ignorable lines
- **Stale script references**: Audit workflow `run:` steps when deleting scripts;
  `continue-on-error: true` silently masks these
- **Action ref drift**: floating refs (`@stable`, `@v2`) change unexpectedly; pin to an explicit release tag
- **Dockerfile BuildKit warning**: Add `# check=skip=SecretsUsedInArgOrEnv` as first line of Dockerfile

---

## Pattern 21: Dockerfile COPY Targets Referencing Non-Existent Directories

### Symptom

```text
ERROR: failed to calculate checksum of ref: "/vendor": not found
ERROR: failed to solve: failed to compute cache key
```

### Root Cause

When path dependencies or vendored directories are removed from the repo, the
Dockerfile `COPY` instructions referencing those paths are not updated.

```dockerfile
# PROBLEM: /vendor was removed from the repo but Dockerfile still copies it
COPY vendor/ /app/vendor/
COPY third_party/custom-lib/ /app/third_party/custom-lib/
```

Local Docker builds may succeed if cached layers hide the missing path. CI builds
with `--no-cache` expose the failure immediately.

### Solution

```dockerfile
# CORRECT: Only COPY paths that exist in the repository
COPY Cargo.toml Cargo.lock /app/
COPY src/ /app/src/
# Removed: COPY vendor/ /app/vendor/ (vendor directory was deleted)
```

**Audit all Dockerfile COPY and ADD instructions after removing files:**

```bash
grep -E '^\s*(COPY|ADD)\s' Dockerfile* | awk '{print $2}'

for src in $(grep -E '^\s*COPY\s' Dockerfile | awk '{print $2}'); do
    if [ ! -e "$src" ]; then
        echo "ERROR: Dockerfile references non-existent path: $src"
    fi
done
```

### Prevention CI Test

```rust
// tests/ci_config_tests.rs

#[test]
fn test_dockerfile_copy_sources_exist() {
    let dockerfile = read_file("Dockerfile");

    for (line_num, line) in dockerfile.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("COPY") || trimmed.starts_with("ADD") {
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if tokens.len() >= 3 {
                let source = tokens[1];
                if source.starts_with("--from=") { continue; }  // Multi-stage
                let source_path = Path::new(source);
                assert!(source_path.exists(),
                    "Dockerfile:{}: COPY source does not exist: {}",
                    line_num + 1, source);
            }
        }
    }
}
```

**Checklist when removing files or directories:**

- [ ] Search Dockerfiles for `COPY` and `ADD` references to the removed path
- [ ] Search `.dockerignore` for entries that reference the removed path
- [ ] Run `docker build --no-cache` locally to verify the build still works
- [ ] Check multi-stage builds — intermediate stages may also reference the path

---

## Pattern 22: Moving Action Refs Used in GitHub Actions Workflows

### Symptom

```yaml
# Workflow uses moving references
- uses: taiki-e/install-action@v2        # Floating major tag
- uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: stable                    # Moving toolchain alias
```

### Root Cause

Floating refs (`@stable`, `@main`, `@v2`) are moving targets and can change
without a workflow edit. This reduces reproducibility and auditability.

This repository intentionally uses explicit version tags instead of commit SHAs.
Trade-off: tags are mutable; mitigation relies on strict versioning policy,
Dependabot updates, least-privilege permissions, and CI validation tests.

**Common ways moving refs get introduced:**

1. Copying examples that use `@stable` / `@main` / `@v2`
2. Manual “simplification” from explicit version tags to channels
3. Inconsistent updates where one workflow gets pinned and others do not

### Solution

```yaml
# WRONG: Moving refs
- uses: taiki-e/install-action@v2
- uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: stable

# CORRECT: Explicit version tags
- uses: taiki-e/install-action@v2.68.8
- uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: 1.88.0  # exact pinned value (for example from rust-toolchain.toml)
```

**Audit existing workflows for moving refs:**

```bash
grep -rnE 'uses: .+@(stable|beta|nightly|main|master|latest)$' .github/workflows/
grep -rnE 'uses: .+@v[0-9]+$' .github/workflows/   # floating major tags
```

**Enforcement hooks/tests:**

```bash
./scripts/check-workflow-hygiene.sh
cargo test --test ci_config_tests test_github_actions_use_version_refs_not_commit_hashes
```

## Pattern 23: Workflow Script References to Non-Existent Files

### Symptom

```text
/home/runner/work/repo/repo/./scripts/verify-sccache.sh: No such file or directory
Error: Process completed with exit code 127.
```

### Root Cause

When scripts are removed or renamed, workflow files that reference them are not
updated. Especially dangerous with `continue-on-error: true`:

```yaml
# PROBLEM: Script was deleted but workflow still calls it
- name: Verify sccache
  run: ./scripts/verify-sccache.sh
  continue-on-error: true  # Masks the "file not found" error!
```

### Solution

Remove the step, update the path, or inline the logic. Prefer inlining simple
scripts to avoid future staleness:

```yaml
- name: Verify sccache
  run: |
    if command -v sccache >/dev/null 2>&1; then sccache --show-stats; fi
```

**Checklist when deleting or renaming scripts:**

- [ ] Search all workflow files for references to the old script path
- [ ] Search `Makefile`, `justfile`, and other task runners
- [ ] Update or remove `continue-on-error` steps that called the script
- [ ] Verify CI passes after the change (not just "green with silent failures")

---

## Pattern 24: Action Reference Not Allowed by Policy

### Symptom

```text
An action could not be found at the URI
  'https://api.github.com/repos/taiki-e/install-action/tarball/abc123...'
```

### Root Cause

Workflow uses a disallowed ref type:

- moving ref (`@stable`, `@main`, `@v2`)
- commit hash (`@<40-char-sha>`)
- malformed remote reference (for example missing `@ref`, empty `@`, or `uses: checkout@v1`)

### Solution

Use an explicit version tag and apply it consistently across all workflows:

```bash
grep -rn 'taiki-e/install-action@' .github/workflows/
```

Then update all occurrences to the same explicit version tag, for example:
`- uses: taiki-e/install-action@v2.68.8`.

---

## Pattern 25: Dockerfile False-Positive Security Warnings

### Symptom

```text
WARNING: SecretsUsedInArgOrEnv: Do not use ARG or ENV instructions for sensitive data
  Dockerfile:15: ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
  Dockerfile:18: ENV SIGNAL_FISH_SECURITY_ENABLED=false
```

### Root Cause

Docker BuildKit flags ENV variables with security-related names (SECURITY, AUTH, KEY,
TOKEN, SECRET, PASSWORD, CREDENTIAL) even when they contain non-sensitive values.
BuildKit only checks variable **names**, not values.

### Solution

Add a BuildKit check skip directive as the **first line** of the Dockerfile:

```dockerfile
# check=skip=SecretsUsedInArgOrEnv
FROM rust:1.88-bookworm AS chef

ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
ENV SIGNAL_FISH_SECURITY_ENABLED=false
```

### When to Use This Suppression

| Scenario | Action |
|----------|--------|
| ENV with security-related name but non-sensitive value | Suppress with `check=skip` |
| ENV with actual secret value | Never put secrets in ENV — use BuildKit secrets |
| ARG with build-time secret | Use `--mount=type=secret` instead |
| New ENV variable triggers warning | Evaluate if truly sensitive first |

---

## Lesson Learned: rustfmt --check on Documentation Code Blocks

`rustfmt --check` returns exit code 1 for **both** parse errors and formatting differences.
Treat `rustfmt` failures on doc snippets as **warnings**, not hard errors.

## Related Skills

- [CI CD Troubleshooting Ecosystem](./ci-cd-troubleshooting-ecosystem.md) — Language mismatch, cache, toolchain
- [CI CD Troubleshooting Linting](./ci-cd-troubleshooting-linting.md) — Clippy, typos, markdown
- [CI CD Troubleshooting Scripts](./ci-cd-troubleshooting-scripts.md) — Shell scripts, Miri, test filtering
- [CI CD Troubleshooting Links](./ci-cd-troubleshooting-links.md) — Lychee, link checking
- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Diagnostic workflow, quick reference
- [Supply Chain Audit Policy](./supply-chain-audit-policy.md) — Security audits and scanning
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Workflow patterns

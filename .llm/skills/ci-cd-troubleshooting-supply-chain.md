# Skill: CI/CD Troubleshooting - Supply Chain & Stale Reference Patterns

<!--
  trigger: sha pinning, supply chain, dockerfile copy, stale script, action not found, continue-on-error, dockerbuildkit warning, secrets env, workflow script missing
  | Patterns 21-25: Dockerfile COPY stale paths, SHA pinning, stale workflow scripts, SHA not found, Dockerfile false-positive security warnings
  | Infrastructure
-->

**Trigger**: When debugging Dockerfile `COPY` path errors, missing SHA pins on GitHub
Actions, stale workflow script references, action SHA not found errors, or Dockerfile
BuildKit `SecretsUsedInArgOrEnv` false-positive warnings.

See also: [ci-cd-troubleshooting-ecosystem.md](./ci-cd-troubleshooting-ecosystem.md),
[ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md),
[ci-cd-troubleshooting-links.md](./ci-cd-troubleshooting-links.md),
[ci-cd-troubleshooting-categories.md](./ci-cd-troubleshooting-categories.md)

---

## TL;DR

- **Stale Dockerfile COPY**: Audit all `COPY`/`ADD` instructions when removing directories
- **SHA pinning**: All `uses:` references must be `@<40-char-sha> # vX.Y.Z` — tags are mutable
- **Stale script references**: Audit workflow `run:` steps when deleting scripts; `continue-on-error: true` silently masks these
- **Action SHA not found**: Force-pushed/rebased action repo; look up new SHA with `gh api repos/OWNER/REPO/git/refs/tags/vX.Y.Z --jq '.object.sha'`
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

## Pattern 22: SHA Pinning Stripped from GitHub Actions Workflows

### Symptom

```yaml
# Workflow uses tag-based references instead of SHA pins
- uses: actions/checkout@v4.2.2          # Mutable tag — supply chain risk
- uses: dtolnay/rust-toolchain@stable    # Mutable tag — supply chain risk
```

### Root Cause

Tags are Git references that can be moved to point to any commit. An attacker who
gains push access to an action repo can retag a release. SHA pins are immutable.

**Common ways SHA pins get stripped:**

1. Copying workflow snippets from documentation (docs use short tags)
2. Dependabot or Renovate updating to tag-only format
3. Manual edits that simplify the `uses:` line

### Solution

```yaml
# WRONG: Mutable tag reference (supply chain risk)
- uses: actions/checkout@v4.2.2

# CORRECT: Immutable SHA pin with version comment
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
```

**Find the SHA for a given action version:**

```bash
gh api repos/actions/checkout/git/refs/tags/v4.2.2 --jq '.object.sha'
```

**Audit existing workflows for missing SHA pins:**

```bash
grep -rn 'uses: .*@v[0-9]' .github/workflows/
grep -rn 'uses: .*@stable\|@main\|@master' .github/workflows/
```

### Prevention

The CI test `test_workflow_actions_are_sha_pinned` in `tests/ci_config_tests.rs`
iterates all `uses:` lines and asserts the ref after `@` is exactly 40 lowercase hex
characters. Add workflow header comments to document the required format:

```yaml
# All action references MUST use SHA pins for supply chain security.
# Format: uses: owner/repo@<40-char-sha> # vX.Y.Z
```

---

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

## Pattern 24: Action SHA Not Found

### Symptom

```text
An action could not be found at the URI
  'https://api.github.com/repos/taiki-e/install-action/tarball/abc123...'
```

### Root Cause

SHA-pinned action reference points to a commit that no longer exists because the
action maintainer force-pushed, rebased, or deleted the commit.

### Solution

Look up the current SHA for the version tag and update the workflow:

```bash
gh api repos/taiki-e/install-action/git/refs/tags/v2.44.30 --jq '.object.sha'
```

Then update to `- uses: taiki-e/install-action@<new-40-char-sha> # v2.44.30`.
Update ALL workflow files that reference the same action, verify the SHA is exactly
40 hex characters, and include the version tag as a trailing comment.

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

`rustfmt --check` returns exit code 1 for **both** parse errors and formatting
differences, making it impossible to distinguish "not valid Rust" from "valid but
unformatted." When validating Rust code blocks in documentation, treat `rustfmt`
failures as **warnings**, not hard errors — doc snippets are often fragments or
pseudo-code that won't parse. Reserve hard errors for `cargo clippy` / `cargo test`
on production code.

---

## Related Skills

- [ci-cd-troubleshooting-ecosystem.md](./ci-cd-troubleshooting-ecosystem.md) — Language mismatch, cache, toolchain
- [ci-cd-troubleshooting-linting.md](./ci-cd-troubleshooting-linting.md) — Clippy, typos, markdown
- [ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md) — Shell scripts, Miri, test filtering
- [ci-cd-troubleshooting-links.md](./ci-cd-troubleshooting-links.md) — Lychee, link checking
- [ci-cd-troubleshooting-categories.md](./ci-cd-troubleshooting-categories.md) — Diagnostic workflow, quick reference
- [supply-chain-security](./supply-chain-audit-policy.md) — Security audits and vulnerability scanning
- [github-actions-best-practices](./github-actions-workflow-config.md) — Workflow patterns

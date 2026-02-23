# Skill: CI/CD Troubleshooting - Diagnostic Workflow & Summary

<!--
  trigger: ci failure, ci debug, diagnostic workflow, ci checklist, ci prevention,
  ci categories, quick reference, ci error messages
  | Diagnostic workflow, prevention checklist, quick reference error table,
  summary categories, real-world examples, escalation | Infrastructure
-->

**Trigger**: When you need the systematic diagnostic workflow for CI failures, the
prevention checklist before committing workflow changes, or the quick-reference error
message table. This is the "meta" guide for CI/CD troubleshooting.

See also: [ci-cd-troubleshooting-ecosystem.md](./ci-cd-troubleshooting-ecosystem.md),
[ci-cd-troubleshooting-linting.md](./ci-cd-troubleshooting-linting.md),
[ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md),
[ci-cd-troubleshooting-links.md](./ci-cd-troubleshooting-links.md),
[ci-cd-troubleshooting-supply-chain.md](./ci-cd-troubleshooting-supply-chain.md)

---

## Diagnostic Workflow

### Step 1: Identify Failure Type

```text
CI Failure
    |
    +-- Compilation error ------> Check Rust version, dependencies, features
    +-- Test failure -----------> Check env vars, filesystem case, test data
    +-- Lint failure -----------> Check clippy version, lint configuration
    +-- Cache error ------------> Check cache keys, action versions
    +-- Docker error -----------> Check base image, build context, COPY paths
    +-- Workflow error ---------> Check syntax, permissions, secrets
    +-- Exit code 127 ----------> Check script references exist in repo
    +-- Supply chain risk ------> Check action SHA pins (not tags)
    +-- Action not found -------> SHA pin references deleted commit (force-push/rebase)
    +-- Docker warning ---------> BuildKit false positive on ENV variable names
```

### Step 2: Check Recent Changes

```bash
# What changed since last successful run?
git diff HEAD~1 HEAD -- .github/workflows/

# Did we update dependencies?
git diff HEAD~1 HEAD -- Cargo.toml Cargo.lock

# Did we change Rust version?
git diff HEAD~1 HEAD -- rust-toolchain.toml clippy.toml Dockerfile
```

### Step 3: Reproduce Locally

```bash
# Match CI environment exactly:
cargo clean
rustc --version  # Verify matches MSRV
cargo test --locked --all-features
```

### Step 4: Compare Configurations

```bash
./scripts/check-msrv-consistency.sh

# Check for ecosystem mismatches
grep -r "pip\|npm\|bundle" .github/workflows/  # Should be empty for Rust-only project
grep -r "cargo\|rust" .github/workflows/       # Should be present
```

### Step 5: Check Staleness

```bash
grep -E "nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}" .github/workflows/
# Are any >6 months old?

grep -E "uses: .+@[a-f0-9]{40}" .github/workflows/
# Are any from >1 year ago?

grep "FROM rust:" Dockerfile
# Is version current or outdated?
```

---

## Prevention Checklist (Agent Self-Review)

Before committing workflow changes, verify:

### Configuration Matching

- [ ] Workflow uses caching appropriate for project language (Rust = `rust-cache`, not pip/npm)
- [ ] All cache paths reference files that actually exist (e.g., Cargo.lock, not requirements.txt)
- [ ] Base images match project MSRV (Docker `FROM rust:X.Y` = Cargo.toml `rust-version`)
- [ ] No language-specific commands for wrong ecosystem

### File Reference Integrity

- [ ] Dockerfile `COPY`/`ADD` sources all exist in the repository
- [ ] Workflow `run:` script references (`.sh` files) all exist in the repository
- [ ] Steps with `continue-on-error: true` are not masking missing file errors

### Version Consistency

- [ ] MSRV consistent across: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, Dockerfile
- [ ] Pinned nightly toolchains documented with age and update criteria
- [ ] All action `uses:` references are SHA-pinned (`@<40-char-sha> # vX.Y.Z`), not tag-only
- [ ] Action SHA pins are recent (<1 year) or have documented reason for age

### Dependency Hygiene

- [ ] Unused dependencies removed (cargo machete passes)
- [ ] All dependencies support project MSRV
- [ ] cargo deny check passes (security, licenses)

### Testing

- [ ] Workflow tested in CI (push to branch, verify passes)
- [ ] Local reproduction verified (cargo commands match CI)
- [ ] Both feature configurations tested (default and --all-features)
- [ ] Cache invalidation tested (workflow runs correctly with cold cache)

---

## Quick Reference: Common Error Messages

| Error Message | Root Cause | Fix |
|---------------|------------|-----|
| `Cache entry deserialization failed` | Wrong cache type or corrupted | Use language-appropriate caching or bust cache |
| `Unable to locate executable file: pip` | Python tools on Rust project | Remove Python-specific actions/commands |
| `requires rustc X.Y.Z or newer` | Dependency needs newer Rust than MSRV | Update MSRV or pin older dependency version |
| `No such file or directory` (case-sensitive) | Linux CI vs macOS/Windows local | Fix import case to match filename exactly |
| `Permission denied` | Workflow needs additional permissions | Add `permissions:` section to workflow |
| `regex parse error` in lychee | `.lychee.toml` `exclude` uses glob syntax | Escape `.` as `\\.`, use `.*` not `*` |
| Script exits with code 1, no error message | `grep` found no matches under `set -euo pipefail` | Add `\|\| true` after grep, or use AWK |
| False positive broken links in code blocks | Link checker scans inside fenced code blocks | Use AWK with fence tracking |
| YAML parse error in markdown file | Non-YAML content in `yaml`-fenced code block | Use `text` for logs; split mixed blocks |
| Lychee reports broken URL from `.lychee.toml` | Lychee scans its own config | Exclude `.lychee.toml` via `--exclude-path` |
| Test assertion fails on config regex pattern | `contains("http://localhost")` vs regex | Test regex behavior (compile + match) |
| `failed to calculate checksum of ref: not found` | Dockerfile `COPY` references removed path | Remove or update stale `COPY` instructions |
| Action behavior changes without workflow edit | `uses:` references a mutable tag | Pin with SHA pin |
| `No such file or directory` (exit code 127) | `run:` step calls a deleted script | Remove stale script reference |
| `toolchain 'X.Y.Z' is not installed` in cargo-deny | Docker action uses own toolchain | Set `RUSTUP_TOOLCHAIN: stable` env var |
| Lychee scans dotfiles despite config | lychee v0.21.0 bug #1936 | Pin `lycheeVersion: v0.22.0` |
| `exclude_path` in `.lychee.toml` has no effect | Confirmed bug for glob-expanded paths | Use `--exclude-path` CLI flags |
| TOML validator fails on "before/after" example | Duplicate `[dependencies]` headers in one block | Split into separate fenced code blocks |
| `An action could not be found at the URI` | SHA pin references force-pushed commit | Look up current SHA for version tag |
| `SecretsUsedInArgOrEnv` Docker build warning | BuildKit flags ENV with security-related names | Add `# check=skip=SecretsUsedInArgOrEnv` as first line |
| AlignedVec alignment lost after Bytes conversion | rkyv serialize() drops alignment via into_vec() | Use serialize_aligned() for zero-copy access |
| WSL bash has no installed distributions (Windows CI) | Command::new("bash") resolves to WSL not Git Bash | Use Git Bash path on Windows CI runners |

---

## Summary: The Twelve Categories

### Category 1: Configuration Mismatch

**Example:** Python caching on Rust project | **Fix Time:** Minutes

### Category 2: Dependency Hygiene

**Example:** 15+ unused dependencies | **Fix Time:** Hours

### Category 3: Toolchain Staleness

**Example:** Nightly from 360 days ago | **Fix Time:** Minutes to Hours

### Category 4: Validation Script Fragility

**Example:** `grep` exit code 1 kills script; lychee regex vs glob confusion
**Prevention:** Use AWK over grep; `|| true` with grep; treat `.lychee.toml` exclude as regex

### Category 5: Code Fence and Config File Mismatches

**Example:** YAML validator fails on `yaml`-tagged shell output; lychee self-scanning
**Prevention:** Match code fence language tags to content; exclude `.lychee.toml` from self-scanning

### Category 6: Stale File References and Supply Chain Gaps

**Example:** Dockerfile `COPY vendor/` after vendor deleted; tag-only action references
**Prevention:** Audit Dockerfiles/workflows when removing files; enforce SHA pinning

### Category 7: Formatter / Spell-Checker Failures in New Test Code

**Example:** `rustfmt --check` fails on new test file; `typos` flags British spellings in test data
**Prevention:** Run `cargo fmt` before staging; add test data files to `[files] extend-exclude`

### Category 8: Invalid YAML in Documentation Code Block Examples

**Example:** `...` placeholder in YAML block causes `yq` to fail
**Prevention:** Use `# ...` (YAML comment) instead of `...` as a placeholder

### Category 9: Miri Isolation Failures for Wall-Clock Time

**Example:** `chrono::Utc::now()` blocks Miri's isolation, aborting all tests in binary
**Prevention:** `#[cfg_attr(miri, ignore)]` on any test calling wall-clock APIs

### Category 10: POSIX Shell Portability

**Example:** `grep '\s'` fails on macOS BSD grep; `tac` unavailable on macOS
**Prevention:** Use `[[:space:]]`; replace `tac` with AWK reverse pass

### Category 11: Release Preflight Safety

**Example:** API errors treated as "no run found"; workflow IDs assumed unique
**Prevention:** Fail closed on API errors; assert uniqueness; conditional artifact attachment

### Category 12: Panic Policy Precision

**Example:** `#[cfg(test)]` on standalone function instead of `mod tests` block
**Prevention:** Apply `#[cfg(test)]` to `mod tests`; use `cargo clippy --lib --bins`

---

## Real-World Examples

### Example 1: Python Cache on Rust Project (RESOLVED)

**Problem:** CI had `actions/cache@v4` with `~/.cache/pip` path on a Rust project.
**Symptoms:** Cache deserialization failures; `pip` executable not found; CI slower.
**Solution:** Replaced with `Swatinem/rust-cache@v2.7.5`.

### Example 2: 360-Day-Old Nightly Toolchain (RESOLVED)

**Problem:** `toolchain: nightly-2025-02-21` (360 days old).
**Symptoms:** Dependencies fail to compile; security vulnerabilities.
**Solution:** Updated to `toolchain: nightly-2026-02-01`.

### Example 3: Accumulated Unused Dependencies (RESOLVED)

**Problem:** 15+ unused dependencies in Cargo.toml; no regular audit process.
**Solution:** Added weekly CI job with `cargo machete`; removed unused deps in PR.

---

## Escalation: When to Ask for Help

Self-service troubleshooting should resolve 90% of CI issues. Escalate when:

1. **Persistent cache corruption** (bust cache doesn't fix)
2. **GitHub Actions platform issues** (outage, service degradation)
3. **Upstream action breaking change** (action author made incompatible change)
4. **Security vulnerability** in pinned version (needs immediate attention)
5. **Resource limits hit** (workflow timeout, out of disk space, etc.)

---

## Related Skills

- [ci-cd-troubleshooting-ecosystem.md](./ci-cd-troubleshooting-ecosystem.md) — Patterns 1-6:
  ecosystem, cache, toolchain, Docker
- [ci-cd-troubleshooting-linting.md](./ci-cd-troubleshooting-linting.md) — Patterns 7-9:
  Clippy, typos, markdown
- [ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md) — Patterns 9-16:
  locked, Miri, shell scripts, YAML
- [ci-cd-troubleshooting-links.md](./ci-cd-troubleshooting-links.md) — Patterns 10-20:
  lychee, regex, cargo-deny
- [ci-cd-troubleshooting-supply-chain.md](./ci-cd-troubleshooting-supply-chain.md) —
  Patterns 21-25: SHA pinning, Dockerfile
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — Writing new
  workflows
- [msrv-management](./msrv-management.md) — MSRV updates and consistency
- [supply-chain-security](./supply-chain-audit-policy.md) — Security audits and vulnerability scanning
- [agent-self-review-checklist](./agent-self-review-checklist.md) — Pre-commit verification checklist

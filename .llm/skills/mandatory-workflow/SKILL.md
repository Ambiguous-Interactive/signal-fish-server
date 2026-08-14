---
name: mandatory-workflow
description: >-
  Run the repository's required formatting, linting, testing, and validation sequence. Use before
  completing or committing any change and when reproducing local CI gates.
---

# Mandatory Workflow

---

## When to Use

- After making ANY code change (Rust)
- Before committing or creating a PR
- When CI fails on lint/format/validation checks
- Setting up a new development environment

---

## When NOT to Use

- Choosing test strategies (see [Testing Core Patterns](../testing/SKILL.md))
- Configuring clippy rules (see [Clippy And Linting](../clippy-and-linting/SKILL.md))

---

## TL;DR

1. **Read the code** before modifying it — NEVER modify code you haven't read.
2. **Run the appropriate linters** after every change (see table below).
3. **Zero warnings, zero errors, zero production panic macros** — all linters and policy checks enforce strict compliance.
4. **For user-visible changes**: run changelog flow
   [Classify User Visible Changes](../classify-user-visible-changes/SKILL.md) ->
   [Update Changelog Keep A Changelog](../update-changelog-keep-a-changelog/SKILL.md) ->
   [Review Changelog Entries](../review-changelog-entries/SKILL.md).

---

## Core Workflow (Every Change)

```bash
# 1. Before any change - read the code first
# NEVER modify code you haven't read

# 2. After Rust changes (ALWAYS run in order)
cargo fmt
cargo clippy --all-targets --all-features  # Zero warnings allowed
cargo test --all-features

# 3. Doc comments (clippy and cargo test never reach these lints)
export RUSTDOCFLAGS="-D warnings -D rustdoc::broken_intra_doc_links \
  -D rustdoc::private_intra_doc_links -D rustdoc::invalid_codeblock_attributes"
cargo doc --locked --no-deps --all-features
cargo doc --locked --no-deps --no-default-features

# 4. Supply chain checks (run before pushing)
cargo deny --all-features check            # Advisories, licenses, bans, sources

```

A public doc comment must not link a private item: `private_intra_doc_links`
rejects it, and only the `Rustdoc Validation` workflow catches that.

### Pre-Push Validation

```bash
# Always run before pushing
scripts/check-ci-config.sh           # Catch CI configuration issues
scripts/check-msrv-consistency.sh    # Verify MSRV consistency (if MSRV-related changes)
scripts/check-doc-consistency.sh     # Version sync + changelog + docs accuracy guard

```

- `check-ci-config.sh`: Catches outdated action versions incompatible with current `Cargo.lock`

  format (see [Supply Chain Audit Policy](../supply-chain-security/SKILL.md))

- `check-msrv-consistency.sh`: Validates all configuration files use the same Rust version as

  `Cargo.toml` (see [MSRV Management](../msrv-management/SKILL.md))

- `check-doc-consistency.sh`: Enforces Cargo version reference sync, Keep a Changelog

  structure/link validity, and protocol/documentation anti-drift checks (see
  [Doc Accuracy Guarantees](../documentation/references/accuracy-and-drift.md))

---

## Linting Requirements by File Type

| File Type        | Linter Commands                                          | Zero Tolerance |
| ---------------- | -------------------------------------------------------- | -------------- |
| **Rust** (`.rs`) | `cargo fmt && cargo clippy --all-targets --all-features` | No warnings    |

---

## Installing Linters (if missing)

```bash
# Rust toolchain
rustup component add rustfmt
rustup component add clippy

```

---

## Commit and Pull Request Policy

Do not create commits, push branches, or open pull requests unless the user has
explicitly requested publication (for example, "open a PR" or "advance this to a
green PR"). When publication is explicitly requested, the agent may perform the
complete workflow after confirming the intended diff:

1. Stage only files that belong to the requested change; preserve unrelated user
   changes.
2. Create a terse, intentional commit.
3. Push the current topic branch with local `git`.
4. Create and inspect the pull request with the connected VS Code GitHub
   extension / GitHub app.
5. Monitor required checks and continue fixing in-scope failures until the PR is
   green.

GitHub CLI (`gh`) is an optional fallback, not a prerequisite. Its absence is not
a blocker when the Git remote accepts pushes and the connected VS Code GitHub
extension / GitHub app can perform the operation. Use an authenticated `gh` only
when the extension/app cannot perform a required operation.

Suggested commit message format:

```text

<type>: <imperative subject>

feat: add spectator mode to rooms
fix: resolve WebSocket cleanup race (#152)
perf: reduce allocations in message broadcast
test: add concurrency tests for room joins
docs: update protocol documentation
chore: update MSRV from 1.87.0 to 1.88.0

```

**When changes are ready and publication was not explicitly requested:**

1. ✅ Verify all checks pass (fmt, clippy, test)
2. ✅ Provide commit instructions to user
3. ❌ Do not infer permission to commit or publish

---

## Session-End Verification (MANDATORY)

Before ending any work session, **always** run the full validation gauntlet to
ensure git hooks and CI will pass cleanly:

```bash
# 1. Core checks (must all pass)
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked --all-features
RUSTDOCFLAGS="-D warnings -D rustdoc::broken_intra_doc_links \
  -D rustdoc::private_intra_doc_links -D rustdoc::invalid_codeblock_attributes" \
  cargo doc --locked --no-deps --all-features

# 2. Script-level policy checks
scripts/check-doc-consistency.sh --staged   # or --changed-files <files>
scripts/check-workflow-hygiene.sh
scripts/check-llm-file-sizes.sh
scripts/check-llm-example-files.sh
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree

# 3. Hook/local-policy test suites (run before handoff; hooks stay fast)
cargo test --locked --test doc_consistency_policy_tests --test doc_consistency_script_tests
cargo test --locked --test ci_config_tests
```

If hook files or hook-adjacent policy code changed, rerun pre-commit with profiling enabled and keep it sub-second:

```bash
SIGNAL_FISH_HOOK_PROFILE=1 pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
```

Any run above 1000ms must be investigated and optimized before handoff.

**Why this matters**: The agent workflow runs hook/local-policy test suites
that validate script output, internal path classifications, and CI config
consistency. A change that passes broad Rust tests alone may still fail hook or
policy guards if script output or policy configuration changed. Always verify
the full chain. Git hooks themselves stay sub-second and inspect the staged Git
index or pushed commits; the `-Worktree` preflights let agents run the same cheap
checks on unstaged workflow/hook policy work before handoff. Agents and local CI
are responsible for catching semantic failures before the hook is ever reached.

---

## PR Checklist

- [ ] `cargo fmt` — no formatting issues
- [ ] `cargo clippy --all-targets --all-features` — zero warnings
- [ ] `cargo test --all-features` — all tests pass
- [ ] `cargo doc --no-deps` under strict `RUSTDOCFLAGS` — no doc-link warnings
- [ ] `cargo deny --all-features check` — supply chain checks pass
- [ ] `scripts/check-ci-config.sh` — CI config validated
- [ ] `scripts/check-msrv-consistency.sh` — MSRV consistency verified (if MSRV changed)
- [ ] `scripts/check-doc-consistency.sh` — version/changelog/docs consistency validated
- [ ] New code has exhaustive tests (see [Testing Core Patterns](../testing/SKILL.md))
- [ ] Documentation updated (see [Documentation Standards](../documentation/SKILL.md))
- [ ] CHANGELOG decision documented via [Classify User Visible Changes](../classify-user-visible-changes/SKILL.md)
- [ ] CHANGELOG updated for user-facing changes via [Update Changelog Keep A Changelog](../update-changelog-keep-a-changelog/SKILL.md)
- [ ] CHANGELOG reviewed via [Review Changelog Entries](../review-changelog-entries/SKILL.md)
- [ ] Breaking changes documented
- [ ] MSRV update documented (if applicable, see [MSRV Management](../msrv-management/SKILL.md))

---

## Security Checklist (Pre-Merge)

- [ ] No `.unwrap()` on user input (see [Defensive Programming](../defensive-programming/SKILL.md))
- [ ] Production `.expect()` / `.unwrap()` additions have both a nearby `// SAFETY:` rationale
      and a matching `#[allow(clippy::expect_used)]` / `#[allow(clippy::unwrap_used)]`
- [ ] Rate limiting in place for public endpoints
- [ ] Auth tokens validated before privileged operations
- [ ] No secrets logged (check tracing fields)
- [ ] Input length limits enforced
- [ ] No integer overflow in arithmetic (use `saturating_*` or `checked_*`)
- [ ] No unchecked array/slice indexing (use `.get()` or `.last()`)

Use [Web Service Security Auth](../web-service-security/SKILL.md)
and [Code Review Checklist](../code-review-checklist/SKILL.md) skills for comprehensive audit.

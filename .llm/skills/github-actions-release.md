# Skill: GitHub Actions Release Gating

<!--
  trigger: release, preflight, cargo locked, path filtered workflow,
  artifact upload, api error, workflow id, release gate
  | Patterns for release preflight checks, cargo --locked consistency, and hardened release workflows | Infrastructure
-->

**Trigger**: When writing release workflows, preflight gates,
or ensuring `--locked` consistency across all cargo commands.

---

## When to Use

- Writing or auditing a release preflight workflow
- Ensuring all `cargo` commands in CI use `--locked`
- Conditionally attaching build artifacts
- Handling path-filtered required workflows in release gates
- Hardening GitHub API calls in release scripts against transient failures

## When NOT to Use

- Scheduled workflow patterns (see [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md))
- General workflow configuration (see [GitHub Actions Workflow Config](./github-actions-workflow-config.md))

## TL;DR

- Use `--locked` on every `cargo` command in CI — missing it silently resolves different deps
- Check `if: steps.<id>.outcome == 'success'` before uploading artifacts
- Fail closed on GitHub API errors in preflight — an outage should block, not pass, the release
- Path-filtered required workflows need special handling: check changed files before treating a missing run as failure
- Assert `WORKFLOW_ID` uniqueness at the start of preflight scripts

---

## 1. Cargo `--locked` Consistency

### Use `--locked` Across All Cargo Commands in CI

Every `cargo` invocation in CI should use `--locked` to ensure the checked-in `Cargo.lock` is
respected. Without it, CI may silently resolve different dependency versions than what was tested.

```yaml
# ❌ WRONG: Inconsistent --locked usage
- run: cargo build --locked
- run: cargo test              # Missing --locked, may resolve different deps
- run: cargo clippy            # Also missing --locked

# ✅ CORRECT: All cargo commands use --locked
- run: cargo build --locked
- run: cargo test --locked
- run: cargo clippy --locked --all-targets --all-features
```

**Important for validation scripts:** When scanning CI files for `--locked` compliance,
parse complete `run:` blocks (including multiline `run: |` content and `\` continuations)
before checking flags. Line-by-line scans miss real regressions where `--locked` appears
on a later continued line.

---

## 2. Conditional Artifact Attachment

Do not assume build artifacts exist. Use step IDs and outcome checks to conditionally attach
artifacts, preventing failures when a prior step was skipped or failed.

```yaml
- name: Build release binary
  id: build
  run: cargo build --release --locked

- name: Upload artifact
  if: steps.build.outcome == 'success'
  uses: actions/upload-artifact@v6.0.0
  with:
    name: release-binary
    path: target/release/my-binary
```

---

## 3. Release Preflight: Fail Closed on API Errors

When querying GitHub APIs during release preflight, always check the exit code before interpreting
the response. An API outage should block the release, not silently pass it.

```bash
# ❌ WRONG: API failure treated as "no runs found"
RUNS=$(gh api "repos/${REPO}/actions/workflows/${WF_ID}/runs" \
  --jq '.workflow_runs[0].conclusion')

# ✅ CORRECT: Fail closed on API errors
if ! RUNS=$(gh api "repos/${REPO}/actions/workflows/${WF_ID}/runs" \
  --jq '.workflow_runs[0].conclusion' 2>/dev/null); then
  echo "ERROR: GitHub API request failed for workflow ${WF_ID}"
  exit 1
fi
```

---

## 4. Assert Workflow ID Uniqueness

If the release preflight iterates over a list of required workflow names or IDs, assert that each
ID is unique at the start of the script. Duplicate entries cause silent mismatches where only
one workflow is actually verified.

```bash
# Assert no duplicate workflow IDs at start of preflight
UNIQUE_COUNT=$(printf '%s\n' "${WORKFLOW_IDS[@]}" | sort -u | wc -l)
TOTAL_COUNT=${#WORKFLOW_IDS[@]}
if [ "$UNIQUE_COUNT" -ne "$TOTAL_COUNT" ]; then
  echo "ERROR: Duplicate workflow IDs detected in REQUIRED_WORKFLOWS"
  exit 1
fi
```

---

## 5. Path-Filtered Workflows in Release Gates

### The Problem

When a required workflow uses `paths:` filters, it only runs when the commit touches matching files.
A release preflight gate that checks for successful runs of all required workflows will fail if a
path-filtered workflow was legitimately skipped.

```yaml
# doc-validation.yml only runs when documentation changes:
on:
  pull_request:
    paths:
      - '**/*.md'
      - '**/*.rs'
      - '.github/workflows/doc-validation.yml'
```

If a release commit only changes `Cargo.toml`, the Documentation Validation workflow will not run.
A naive preflight check sees "no completed run" and blocks the release.

### The Solution: Register Path-Filtered Workflows

The release workflow declares a `PATH_FILTERED_WORKFLOWS` associative array mapping workflow names
to their path patterns. When no completed run is found, the preflight checks whether the commit
actually touched matching paths before treating absence as an error.

From `.github/workflows/release.yml`:

```bash
# Declare path-filtered workflows and their trigger patterns.
# Keep in sync with the `paths:` block in each workflow file.
declare -A PATH_FILTERED_WORKFLOWS
PATH_FILTERED_WORKFLOWS["Documentation Validation"]=\
  "*.md *.rs Cargo.toml Cargo.lock .github/workflows/doc-validation.yml .github/scripts/"

# When no completed run is found for a workflow:
if [[ -v "PATH_FILTERED_WORKFLOWS[$WORKFLOW_NAME]" ]]; then
  CHANGED_FILES=$(gh api "repos/${REPO}/commits/${COMMIT_SHA}" \
    --jq '.files[].filename')

  PATHS_MATCH=0
  for pattern in ${PATH_FILTERED_WORKFLOWS[$WORKFLOW_NAME]}; do
    # Match changed files against patterns
    if echo "$CHANGED_FILES" | grep -q "$pattern"; then
      PATHS_MATCH=1
      break
    fi
  done

  if [ "$PATHS_MATCH" -eq 0 ]; then
    echo "OK: commit did not touch relevant paths — skip expected"
  else
    echo "ERROR: commit touched relevant paths but workflow did not run"
    FAILED=1
  fi
fi
```

### Adding a New Path-Filtered Required Workflow

When adding a new workflow with `paths:` filters to the required list:

1. Add the workflow name and path patterns to `PATH_FILTERED_WORKFLOWS` in `release.yml`
2. Keep the patterns in sync with the `paths:` block in the workflow file
3. Add the workflow to `REQUIRED_WORKFLOW_NAMES` in `tests/ci_config_tests.rs`

**Validated by:** `test_release_workflow_handles_path_filtered_workflows` in `tests/ci_config_tests.rs`
ensures every required workflow with `paths:` filters appears in `PATH_FILTERED_WORKFLOWS`.

---

## 6. Cross-Platform Release Binaries

A crate-only release leaves Windows / macOS / ARM users with nothing to download. Add a matrix
job that builds a standalone binary per OS/arch and attaches each (with a checksum) to the Release.

- **Split build from upload into two jobs.** A `build-binaries` matrix (one leg per target)
  compiles and uploads each archive + checksum as a workflow artifact via
  `actions/upload-artifact`; a single `attach-binaries` job then `download-artifact`s all of them
  and performs ONE `softprops/action-gh-release` upload. Funnelling every asset through one release
  API call avoids the race where N parallel matrix legs PATCH/upload to the same Release
  concurrently (intermittent 422s / clobbered assets).
- **Pin the target list in a drift test** (`REQUIRED_RELEASE_TARGETS`) so a platform can't silently
  vanish from the matrix. Covered triples here: Linux `x86_64`/`aarch64`, macOS `x86_64`/`aarch64`,
  Windows `x86_64`/`aarch64`.
- **`fail-fast: false` + run attach on partial success.** Give `attach-binaries` an
  `if: ${{ !cancelled() && needs.publish.result == 'success' }}` so one platform's toolchain
  hiccup attaches the binaries that DID build instead of skipping the attach job entirely (a
  plain `needs:` on a failed matrix job would skip it and strip every binary off the release).
- **Cross-compile the awkward targets instead of chasing runners:** build both macOS targets on
  Apple Silicon (`macos-14`) — the native toolchain cross-compiles `x86_64` and avoids the
  deprecating Intel runners. For Linux `aarch64`, install `gcc-aarch64-linux-gnu` **and**
  `libc6-dev-arm64-cross` (the latter is only a _recommends_, so `--no-install-recommends` drops it
  and linking fails with `cannot find Scrt1.o`).
- **Ship a `.sha256` next to each archive.**
- **Run `build-binaries` in parallel with `publish`** (`needs: [preflight]`), and gate
  `attach-binaries` on `needs: [publish, build-binaries]` so the Release/tag exists before upload.
- Build with **default features** to match the container image and dodge C-crypto cross-toolchain
  pain (`aws-lc-sys`/`ring` only arrive via the optional `tls` feature).

**Validated by:** `test_release_workflow_builds_all_platform_binaries` and
`test_release_workflow_attaches_binaries_with_checksums`.

---

## Agent Checklist

- [ ] All `cargo` commands in CI use `--locked` consistently
- [ ] `--locked` validation scans complete `run:` blocks, not line-by-line
- [ ] Artifact upload steps use `if: steps.<id>.outcome == 'success'` guards
- [ ] Release preflight fails closed on GitHub API errors (checks `gh api` exit code)
- [ ] Required workflow ID lists asserted unique at start of preflight
- [ ] Path-filtered required workflows registered in `PATH_FILTERED_WORKFLOWS`
- [ ] `PATH_FILTERED_WORKFLOWS` patterns kept in sync with each workflow's `paths:` block
- [ ] Release-binary matrix covers every `REQUIRED_RELEASE_TARGETS` triple with `fail-fast: false`
- [ ] Each release archive ships a `.sha256` checksum and is uploaded to the Release

---

## Related Skills

- [GitHub Actions Config Tests](./github-actions-config-tests.md) — Tests that validate release workflow correctness
- [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md) — Cron schedules, job guards
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Path filters, permissions, concurrency
- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures

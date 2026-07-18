# Session 055 — release publication recovery

## Scope

Repair the failed 0.4.1 publication, eliminate redundant operator-entered
release identity, keep crates.io probes out of the source checkout, and carry
the remediation through local policy validation, adversarial review, and a
fully green pull request.

## Baseline evidence

- Release run `29627225895` failed closed because the dispatched version was
  `0.10.1` while `Cargo.toml` contained `0.4.1`.
- Release run `29627247402` resolved commit
  `b41d678fb96821eef998dd2ff97d6439b64dafeb`, passed CI preflight, created the
  annotated `v0.4.1` tag, published the verified GHCR image, and built all six
  binary targets.
- Its crates.io idempotency probe wrote `crate-version.json` into the checkout.
  The following `cargo publish --locked` correctly rejected that untracked file
  and exited 101 before uploading the crate.
- crates.io therefore still exposes 0.4.0 as latest, and no GitHub Release for
  v0.4.1 exists. The immutable tag and GHCR state are valid recovery inputs.

## Approved invariants

- Manual publication derives the version from the default branch; operators do
  not retype it.
- Existing release tags are immutable, annotated, merged into the default
  branch, and reused as the exact source of an idempotent retry.
- Network probes and downloaded registry artifacts live under `RUNNER_TEMP`,
  never in the source checkout.
- A clean Git worktree is an explicit publication precondition. The workflow
  never bypasses Cargo with `--allow-dirty`.
- `CRATES_IO_TOKEN` remains the authentication mechanism and is required before
  mutation only when crates.io does not already contain the matching release.

## Red/green log

Green implementation:

- Manual dispatch now derives strict semver from the reviewed default branch.
  A production resolver reuses an existing tag only when it is annotated,
  reachable from the dispatched commit and default branch, and has matching
  Cargo/changelog metadata at its immutable source commit.
- Publication readiness now validates crates.io checksum and embedded VCS
  revision plus boolean `dirty: false` before any tag/container mutation. A
  missing version requires the existing token; an already-published matching
  retry does not.
- Registry JSON and crate downloads use a trapped `RUNNER_TEMP` directory. Both
  readiness and final publication run an untracked-aware clean-worktree helper,
  and the final gate immediately precedes Cargo publication.
- Data-driven Git and mocked-registry fixtures execute the production helpers
  across absent/matching/lightweight/unmerged/mismatched tag states, direct tag
  events, clean/tracked/untracked worktrees, absent/matching/conflicting crates,
  and registry failure.
- Adversarial review strengthened fixture isolation against inherited Git
  signing, rejected dirty/missing/wrong-type Cargo VCS provenance, and made the
  registry-probe test assert the entire source checkout remains clean.

Targeted evidence:

- `cargo test --locked --test release_publish_tests -- --nocapture`: 4 passed.
- `cargo test --locked --test ci_config_tests`: 285 passed, 1 ignored after the
  SBOM path assertion was aligned with the isolated `source/` checkout.
- `actionlint`, `shellcheck`, workflow hygiene, LLM size/example policy,
  markdown, documentation consistency, and `git diff --check`: passed.

Full verification:

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo test --locked --all-features`: passed after rebuilding one stale local
  incremental test artifact; the fresh 16-player binary passed 13/13 both in
  isolation and in the complete rerun.
- `cargo deny --all-features check` through `scripts/check-ci-config.sh`: passed
  with the repository's existing duplicate-dependency warnings.
- Hook readiness, pre-commit worktree policy (905 ms), and pre-push worktree
  policy (539 ms): passed.
- Explicit documentation policy suites: 15 passed. Explicit CI configuration
  suite: 285 passed, 1 ignored.
- Independent adversarial review: zero remaining issues after three findings
  were fixed and covered by behavioral fixtures.

## Pull request review

- PR #185 opened at commit `a8c50ac` and all fast workflows passed.
- Cursor Bugbot found that retry detachment replaced the default relative TOML
  helper with the historical tag's copy. The resolver now snapshots the
  dispatch-revision helper under runner-temporary storage before detaching, and
  a production-path fixture proves recovery when the historical tag does not
  contain that helper.
- GitHub Copilot was explicitly requested and awaited, but the service reported
  that the requesting account had reached its review quota. A retry remains
  scheduled after the feedback fix and CI rerun.

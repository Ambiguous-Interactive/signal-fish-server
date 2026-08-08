# Session 107 — Prepared release source integrity

## Scope and prioritization

Remote triage found no open pull requests, drafts, or dependency updates. The
latest exact `main` checks had no failures, while P53 and P56 remained fixed
hosted-evidence cohorts at 3/20 attempts. Issue #312 was therefore the next
bounded release dependency: publish 0.6.0 from the reviewed preparation commit
`3b1ad61eb3657fa910a9390ea144725fd464e0df`.

Publication could not safely start. Documentation PR #313 had advanced `main`
to `5545daf1435374df8a913b5ec533f387076ebd08`, no `v0.6.0` tag existed, and the
manual resolver selected the dispatch revision whenever the tag was absent.
That would publish a different source than issue #312 permits. Issue #314
records this release-orchestration defect.

## Failure-first evidence

The executable release fixture previously asserted that an absent tag used the
dispatched head. A new case added a later documentation commit after the
prepared version and failed RED: the resolver returned the later commit instead
of the version-introduction commit. Independent RED cases covered a later
same-version `Cargo.toml` edit and a shallow first-parent history.

## Fix

- Manual publication remains restricted to the default branch and derives the
  version from its reviewed `Cargo.toml`; there is no new operator-entered
  version or source identity.
- When the tag is absent, the resolver scans only first-parent commits that
  changed `Cargo.toml` and selects the unique boundary whose version differs
  from its first parent.
- Shallow history and multiple introductions of the same version fail closed
  before tag or registry mutation.
- Existing annotated-tag retries and direct-tag validation retain their prior
  immutable-source checks.
- The canonical runbook explains source selection, and the development guide no
  longer instructs maintainers to bypass the workflows with a hand-pushed tag.

## Changelog classification

This changes maintainer-visible release behavior and prevents public artifacts
from acquiring the wrong source identity. `CHANGELOG.md` records it under
Unreleased `Fixed`; runtime server and wire behavior are unchanged.

## Validation

- The exact resolver regression was observed RED before the fix and passes
  after it.
- Additional executable absent-tag cases prove that zero matching introduction
  commits fail closed and that release notes added or corrected only after the
  selected introduction cannot make missing or mismatched changelog metadata
  publishable. These cases passed against the fixed resolver without another
  production change.
- The complete `release_publish_tests` suite passes, including registry,
  release, clean-worktree, retry, and direct-tag boundaries.
- Focused release workflow policy tests pass.
- Root formatting, strict all-target/all-feature Clippy, and the complete locked
  all-feature test suite pass. Cargo deny, CI configuration, MSRV, workflow
  hygiene, documentation, Markdown, LLM policy, hook readiness, and worktree
  pre-commit/pre-push checks also pass. Profiled pre-commit runs completed in
  947 ms and 921 ms; an intervening 1,230 ms run was traced to transient Git
  changed-file discovery rather than a changed hook path.
- The first adversarial review found missing absent-tag failure coverage; the
  evaluator added zero-match and missing/mismatched changelog cases. The second
  review found that the zero-match symlink target was untracked; it is now
  committed, with assertions proving a clean fixture and preserved historical
  symlink-blob semantics.
- The final independent adversarial pass reported zero findings across release
  safety, shell portability, fixture validity, documentation, changelog, PLAN,
  and session scope.

Hosted CI, merge, and the subsequent issue #312 publication remain pending at
this point in the session.

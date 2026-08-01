# Session 074 — Release lockfile completeness

## Scope

Carry the existing 0.5.2 release pull request [#238](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/238)
to green without stacking another pull request, while preserving the intentional
`rust-analyzer` developer-toolchain component for VS Code compatibility.

## Baseline and triage

- `main` at `0aedbe9` was green across all 14 exact-SHA push workflows.
- The repository had six open umbrella issues and no dependency pull requests.
  Issue #207 remains the highest direct gameplay-impact follow-up.
- The only open pull request was the mergeable 0.5.2 release PR. Eleven hosted
  workflows passed, while CI and Advanced Safety failed.
- Both failures had one deterministic cause: `fuzz/Cargo.lock` still pinned
  `signal-fish-server` 0.5.1 after the release commit bumped the root to 0.5.2.

## Failure-first evidence

The release fixture was expanded to include the fuzz package before production
automation changed. Its semver synchronization test failed because the prepared
fuzz lockfile remained at 1.2.3 while the other graphs advanced to 1.2.4.

An expanded malformed-lock negative test then found a second gap in the first
dynamic implementation: a mandatory lockfile that had lost its root-package
entry could disappear from discovery. The preflight now explicitly requires the
root, native, and fuzz graphs in addition to dynamically discovering future
graphs.

## Implementation

- Updated `fuzz/Cargo.lock` to the release version 0.5.2.
- Release preparation now discovers every tracked lockfile embedding the root
  path package, checks for a sibling manifest, validates the released baseline,
  updates the package version, and fully resolves each graph with locked
  metadata so an otherwise-undetected stale local dependency also fails closed.
- Shared package-block parsing selects only the unsourced local package. A
  registry-only lock is ignored, while a mixed lock updates the path entry and
  preserves the registry version, source, and checksum byte-for-byte. Exact
  basename discovery also ignores lookalike `*-Cargo.lock` files.
- Mandatory current graphs fail closed if discovery becomes vacuous or a
  package entry disappears.
- Every tracked manifest receives a full locked-metadata preflight before lock
  contents select rewrite targets, so a future graph whose stale lock has not
  yet gained the root-package entry is rejected instead of silently omitted.
- The unpublished fuzz package retains an exact local path-dependency version:
  `cargo-deny` deliberately rejects a path-only dependency as a wildcard. The
  release preparer synchronizes that constraint with the root version, includes
  the manifest in byte-identical rollback, and real Cargo resolution proves
  patch, minor, and major prepared fuzz graphs build locked.
- The preparation workflow derives both its exact allowed diff and staged file
  set from the same dynamic graph rule.
- A future standalone-package fixture proves new tracked graphs are included
  without editing release automation.
- Workspace-lock diagnostics now identify the corresponding `Cargo.toml` with
  platform-neutral escaped path rendering, including whitespace and apostrophes.
- Failed postflight checks restore byte-identical release inputs. The workflow
  captures recovery patches on failed later steps and can resume after a
  transient PR-creation failure only when the existing release branch tree is
  exact; conflicting branches and a branch head that changes before final PR
  verification still fail closed. The production branch and PR helpers are
  executed in tests across absent, reusable, conflicting, and API failure
  states rather than guarded only by workflow substring checks.
- `rust-toolchain.toml` includes `rust-analyzer` for VS Code compatibility; CI
  jobs that request explicit components retain their narrower setup.

## Verification

Focused release tests (including real Cargo resolution, path/registry/mixed
graphs, lookalike filenames, and rollback), workspace-lockfile tests, workflow
policy tests, shell syntax and ShellCheck, CI configuration validation, workflow
hygiene, tooling parity, and MSRV consistency are green. The first hosted
candidate exposed the `cargo-deny` wildcard rule in the fuzz graph; its focused
corrective test, the exact fuzz `cargo deny check bans`, locked metadata, format,
ShellCheck, and deny-warnings Clippy validation are also green. The final
nightly AddressSanitizer lane exposed Cargo's newer `--locked` diagnostic
(`cannot update the lock file`) in the stale-graph negative test; the assertion
now accepts both supported Cargo phrasings while still requiring the lockfile
and `--locked` evidence. The final Copilot pass also aligned the missing
mandatory-lock diagnostic with the discovery condition and corrected the
workspace-lock parser contract comment. The parser guard also retains a
matching unsourced root package with no parseable version as an explicit error
instead of treating the malformed graph as irrelevant. Existing-branch retries
fetch the advertised branch ref into `FETCH_HEAD`, reject a branch that moves
during verification, and use a BSD/GNU-portable rollback `mktemp` template.
Final hosted validation and the reviewer loop are recorded on PR #238 before
publication completes.

## Follow-up

After the release dependency is green, issue #207 remains the next gameplay
priority. Issue #239 now tracks a deterministic TURN-only interop lane that uses
server-minted credentials against local coturn and proves relayed data channels
plus the WebSocket fallback floor without representing the test as production
TURN operation.

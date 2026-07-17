# Session 051 — Issue 155 Release Automation

## Objective

Complete issue 155's two-phase manual release flow: prepare a reviewed release
commit from a semantic-version dropdown, then publish the crate, GitHub Release,
and packaged artifacts.

## Design

- Reused the already-merged P11 publication path in `.github/workflows/release.yml`.
  It already owns the canonical annotated tag, crates.io publication, verified
  multi-architecture GHCR image, GitHub Release, SBOM, and platform archives.
- Added `.github/workflows/prepare-release.yml` as the missing first phase. It
  accepts `patch`, `minor`, or `major`, supports a no-write dry run, requires a
  default-branch dispatch, validates exact tag/branch nonexistence, and opens a
  `release/vX.Y.Z` pull request.
- The real PR path uses the organization's auto-commit GitHub App installation
  token. A PR created with `GITHUB_TOKEN` would not start normal pull-request
  workflows, so silently falling back would leave the release commit without
  the repository's required CI evidence.
- Added `scripts/prepare-release.sh` as the deterministic local and workflow
  implementation. It updates `Cargo.toml`, all tracked lockfiles containing the
  root path package, public dependency examples, `.llm/context.md`, and the dated
  changelog section/link graph; it then validates locked Cargo metadata and the
  repository documentation policy.

## Verification

- Data-driven tests cover all three bump choices and every synchronized output.
- Negative tests cover invalid input, invalid dates, empty release notes,
  duplicate release sections, missing lockfile identity, and arithmetic
  overflow.
- CI policy tests pin the workflow choice input, non-cancellable concurrency,
  least privilege, GitHub App token, branch/tag collision checks, recovery
  artifact, PR creation, and handoff to the publication workflow.

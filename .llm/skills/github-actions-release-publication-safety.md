# Skill: GitHub Actions Release Publication Safety

<!--
  trigger: cargo publish, release retry, dirty checkout, immutable tag,
  crates.io probe, runner temp, release identity
  | Retry-safe release identity and clean publication workspace patterns | Infrastructure
-->

**Trigger**: When publishing a crate, recovering a partial release, resolving
an existing release tag, or inspecting registry state before publication.

---

## One Release Identity

Manual release publication must run from the default branch and derive its
version from the reviewed `Cargo.toml`. A second free-form workflow input is not
confirmation; it is a competing identity that can be mistyped.

If the matching tag already exists during a retry:

1. Fetch the exact remote tag and require an annotated tag object.
2. Require its commit to be reachable from the commit that was dispatched, not
   merely from a later default-branch tip.
3. Detach at the tagged commit and revalidate Cargo and changelog metadata.
4. Reuse that source revision without moving the tag.

This permits a workflow-only fix on `main` to finish a partially published older
commit while preserving the immutable artifact identity.

## Readiness Before Mutation

Run the authoritative crates.io checksum and `.cargo_vcs_info.json` comparison
before creating a tag or publishing GHCR state. Fail closed on API errors,
malformed checksums, downloaded-byte mismatches, and source-revision conflicts.
Require embedded Cargo VCS metadata to contain the exact revision and the
boolean value `dirty: false`; missing, dirty, or wrong-type metadata is unsafe.

Require `CRATES_IO_TOKEN` only when the version is absent. A matching
already-published crate must remain recoverable without upload credentials.

## Publication Workspace Boundary

Registry lookups are readiness probes, not package source generation. Write
response JSON, downloaded `.crate` archives, and similar data only to a
`mktemp` directory under `$RUNNER_TEMP`, with an EXIT cleanup trap.

Immediately before `cargo publish`, run:

```bash
dirty=$(git status --porcelain=v1 --untracked-files=all)
```

Fail with the complete dirty-path list when it is non-empty. `git diff` alone is
insufficient because it misses untracked files. Never add `--allow-dirty`;
Cargo's own protection remains the final defense.

## Checklist

- [ ] Manual dispatch derives Cargo version from the default branch.
- [ ] Existing retry tags are annotated, immutable, dispatched-commit
  ancestors, and metadata-consistent.
- [ ] Registry collision checks run before irreversible publication.
- [ ] Registry scratch files live under `$RUNNER_TEMP` and are cleaned.
- [ ] A `git status --porcelain=v1 --untracked-files=all` gate immediately
  precedes `cargo publish`.
- [ ] `cargo publish` never uses `--allow-dirty`.
- [ ] Tests execute production resolver, registry-probe, and clean-gate logic
  against absent, matching, and conflicting fixtures.

## Related Skills

- [GitHub Actions Release Gating](./github-actions-release.md)
- [CI Configuration Validation Tests](./github-actions-config-tests.md)
- [Repository Source Hygiene Guards](./repo-source-hygiene-guards.md)

# Session 108 — Signal Fish Server 0.6.0 publication

## Scope and prioritization

PR #315 merged P66's exact-source recovery as `8b48a3e`, leaving issue #312 as
the next bounded release dependency. P53 and P56 remain fixed hosted-evidence
cohorts at 3/20 eligible attempts; neither has further deterministic in-repo
work before its pre-registered sample reaches 20.

Release - Publish Crate run [`31278681384`][release-run] dispatched from that
`main` head and completed all 14 jobs successfully. The resolver selected the
reviewed P65 source `3b1ad61eb3657fa910a9390ea144725fd464e0df`, not the later
dispatch revision. Preflight found its required CI and Documentation Validation
runs green before any public mutation.

## Immutable public identities

- Annotated tag object `b23397e79d12938e18477dd7befd88940d5f02d5` dereferences
  to source `3b1ad61eb3657fa910a9390ea144725fd464e0df`.
- The unyanked crates.io `signal-fish-server 0.6.0` archive is 2,485,421 bytes
  with SHA-256
  `cf03c6d26cd2a6806e4495fdeb500443c94ef625494c1b1c9fe486e3186031e3`.
  Its Cargo VCS metadata names the same source revision and a clean tree.
- GitHub Release [`367304725`][release] is public, non-draft, and
  non-prerelease. Its CycloneDX 1.5 SBOM has SHA-256
  `7801a3e1037b73865843221e9d0a97c98953fc3eea2ecd9be064d99067e3897a`
  and identifies `signal-fish-server 0.6.0`.
- The six supported binary archives and their published SHA-256 values are:

  | Target | SHA-256 |
  | --- | --- |
  | `aarch64-apple-darwin` | `50f9f0eabc2889acee6d3f13be01ef5734a09f2ecf63ffb0a892de7704232657` |
  | `aarch64-pc-windows-msvc` | `38816258fb238c4c968ec5e701498772e64facfa85e2d1b6ef19deef07b4c4cf` |
  | `aarch64-unknown-linux-gnu` | `7a6c010e42a6d1653a31c894e1518b437d25134575d6a58c18f9c14fdde381b8` |
  | `x86_64-apple-darwin` | `14214cdb46178f2a97d25f85c24a4b61265c05262430a95246695c25416d89bb` |
  | `x86_64-pc-windows-msvc` | `9b3231fb5f608365ef0777e8ff5ae1b551a82a5a739d1fe2a557b76ab7c5b79c` |
  | `x86_64-unknown-linux-gnu` | `0c47c76e4b3003e15d6238a513992f3a64161ee62953b6da52e21cc0e2b9b143` |

- GHCR aliases `v0.6.0`, `0.6.0`, `0.6`, and `sha-3b1ad61` resolve to
  manifest
  `sha256:14d68c82bb34fc22cabca6e1203fe4871fd9e7f2f07215cbd57ebe87f7b94a63`.
  Its exact platform set is `linux/amd64`, `linux/arm64`, and `linux/arm/v7`;
  every child image labels version `0.6.0` and revision
  `3b1ad61eb3657fa910a9390ea144725fd464e0df`.

## Independent verification

- `scripts/check-crates-io-release.sh` downloaded the public archive, matched
  its registry checksum, and verified the embedded clean source revision.
- `scripts/verify-release-image.sh` independently resolved all four aliases,
  the exact platform set, and every child image's OCI version/revision labels.
- Every binary archive's attached checksum passed `sha256sum -c`; all six
  archives contained the expected target-named executable.
- The public Release API exposed exactly one SBOM, six archives, and six
  corresponding checksum files. The Release notes record the verified GHCR
  digest and full source revision.
- The discarded 0.5.3 candidate remains absent from remote tags, crates.io,
  GitHub Releases, and GHCR.
- The exact dispatch head's ten applicable hosted workflows, including CI,
  Advanced Safety, Documentation Validation, Docker Publish, and the release
  workflow, are terminal green.

## Changelog classification and remaining boundary

This session records already-published 0.6.0 artifacts and updates PLAN status.
It changes no server API, configuration, wire behavior, runtime behavior,
performance, security contract, or release contents, so no new changelog entry
is appropriate.

P66 and issue #312 are complete. P53 and P56 retain their unchanged 20-attempt
hosted acceptance thresholds; no result is inferred from their current 3/20
samples and no timing or gameplay oracle is weakened.

[release-run]: https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/31278681384
[release]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.6.0

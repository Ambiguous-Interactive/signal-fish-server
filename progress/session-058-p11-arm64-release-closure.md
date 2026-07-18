# Session 058 — P11 ARM64 release closure

## Trigger

Issue #122 still reported that `latest` could not be pulled on Linux ARM64 even
though PR #123 had added the multi-architecture build and the release policy had
since grown exact platform drift guards. The tracking state needed to be tested
against the registry rather than inferred from workflow configuration.

## Evidence

Fresh Buildx inspection proved that both published indexes contain the promised
runtime platform set:

- `latest` resolves to
  `sha256:31d8adb477ffe2aea65b6edcd0cefae671cf16bf48e6e52122ea9c464a3a1c7c`;
- `0.5.0` resolves to
  `sha256:af1f3b965f8ec7e7f7678112bd260485d092669d1bba49f7dc0d4eb0849487c8`;
- each index contains `linux/amd64`, `linux/arm64`, and `linux/arm/v7`; and
- the canonical verifier passed for `v0.5.0`, `0.5.0`, and `0.5`, proving one
  shared release digest plus revision
  `16ac09b042436b6dbc5cca0b68c462eb2a8ab33f` and version `0.5.0` labels on
  every runtime manifest.

The in-repo policy surface independently pins the same contract:

- `.github/workflows/docker-publish.yml` publishes all three platforms;
- `Dockerfile` maps every published platform to an explicit Rust target and
  linker; and
- `tests/ci_config_tests.rs` requires ARM64, validates the workflow platform
  list, and checks the cross-compilation map.

## Outcome

Issue #122 received the current registry evidence and was closed as completed.
`PLAN.md` now records the current-release audit under P11. No production or
workflow change was needed, and no changelog entry was added because the
user-visible fix is already recorded in the existing #122 changelog entry.

## Verification

- `docker buildx imagetools inspect` for `latest` and `0.5.0`
- `bash scripts/verify-release-image.sh` for `v0.5.0`, `0.5.0`, and `0.5`
- focused multi-architecture and Dockerfile policy tests
- documentation consistency, Markdown, internal-link, and LLM file-size checks

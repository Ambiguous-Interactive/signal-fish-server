# Documentation Requirements

See [Documentation Standards](skills/documentation/SKILL.md) for full standards.

Every feature/bugfix requires: doc comments with examples, CHANGELOG entry,
README updates if user-facing.

Run `./scripts/check-doc-consistency.sh` before handoff to prevent
version/changelog/protocol doc drift.

Config and binary wire-format drift rules:
[Config and Wire-Format Drift](config-wire-format-drift.md)

## Code Fence and CI Pitfalls

- **Code fence language tags must match content** -- tag blocks as `yaml` only for valid YAML,
  `bash` for shell/AWK, `text` for logs or mixed output.
- **Split mixed-content blocks** -- a block with both shell commands and YAML must be two
  separate fenced blocks with appropriate tags, not one `yaml` block.
- **`.lychee.toml` `exclude` patterns are regex, not globs** -- escape `.` as `\\.`,
  use `.*` not `*`, anchor with `^`. See
  [CI/CD Troubleshooting Pattern 13](skills/ci-cd-troubleshooting/references/link-checking.md).
- **Lychee self-scans `.toml` files** -- use `--exclude-path .lychee.toml` or add exclusions.
- **TOML/JSON/YAML "before/after" examples need separate blocks** -- duplicate table headers
  (e.g., two `[dependencies]`) in one block is invalid and will fail CI validation.
- **Config readers must not depend on exact delimiter spacing** -- TOML/YAML allow
  `key=value`, `key = value`, leading whitespace, and tabs. Prefer a parser; otherwise
  use exact-key, anchored, whitespace-tolerant, section-aware helpers such as
  `scripts/read-toml-string.sh` instead of `grep '^key = '` or `awk -F ' = '`.
- **PowerShell fixtures/scripts cannot overload functions** -- PowerShell silently
  replaces earlier functions with later functions of the same name in a command
  block or script file. Use one helper per name and distinct names for distinct
  behaviors.
- **Local CI aggregate helpers must survive failures under `set -e`** -- helper
  failure branches should record the check in `FAILED_CHECKS` and return success;
  the final summary owns the nonzero exit. Required local-CI script gates should
  fail closed when their script file is missing, not silently skip.
- **Avoid accidental setext headings in skills** -- keep a blank line between
  `**Trigger**: ...` and a following `---` separator, or markdownlint will treat
  the trigger line as a heading (MD003/MD026).
- **Skill examples must be split into dedicated files** -- when documenting incidents or
  walkthroughs, create one `*-example-*.md` file per example and link from the parent
  skill. Do not keep multi-example "mega" sections inside a single skill file.
- **Use descriptive markdown link text for internal docs** -- avoid filename-as-label links
  like `[testing-core-patterns](...)`; prefer human-readable labels like
  `[Core Testing Patterns](...)`. Enforce with `./scripts/check-markdown-link-text.sh`.
- **Rust Markdown block validation needs extractor + classifier coverage** -- the Rust
  extractor preserves leading blank lines, while the validator classifies snippets
  against a leading-blank-normalized copy. Keep that distinction so CI compiles
  complete items that start after blank lines without mutating the content written
  to `rustfmt`/`rustc`. Extractor tests must cover CommonMark closing fences with
  trailing whitespace, and external-context downgrades must only warn when every
  compiler error is a missing-context diagnostic; mixed syntax errors must fail.
  Placeholder-looking tokens or comments (`.. Default::default()`, `// Example:`,
  `// Note:`) may only skip non-item fragments; item-level Rust must compile or
  fail. User-facing `docs/*` Rust blocks are validated, Rustdoc-style top-level
  statement snippets may compile through a wrapper harness, intentionally
  non-compilable Rust-shaped inventories should be marked `rust,ignore` rather
  than hidden through path-based skips. The canonical extractor recognizes `rust`
  and `Rust` fences only; helper extractors and fixtures must preserve byte-for-byte
  parity with that behavior, including ignored non-canonical forms such as `RUST`.
  Do not reintroduce bare AWK fence prefixes like `/^```[Rr]ust/`; workflow AWK
  hygiene should flag Rust fence regexes unless they use a token boundary such as
  `^```+[Rr]ust([[:space:],]|$)` or delegate to the canonical extractor.
  The AWK/Python extractor parity fixture must stay wired into `doc-validation.yml`,
  and release preflight path filters must include every doc-validation trigger path
  so fixture, tooling, or internal-link-checker-only release commits do not bypass
  Documentation Validation.
- **Dependabot auto-merge gating must be CI-aware and squash-only** -- never enable
  Dependabot auto-merge while pull request CI workflows are pending or failing; require
  completed workflow runs with `success`/`skipped` conclusions, then use
  `gh pr merge --auto --squash --match-head-commit ...` to stay compatible with squash-only repos.
- **Dependabot auto-merge must retry transient GitHub merge API errors** -- treat
  `unstable status`, `GraphQL: Something went wrong while executing your query`,
  rate limits, and HTTP 5xx-style merge errors as retryable with a capped counter/backoff;
  keep policy, permission, and unsupported auto-merge errors on fail-fast or fallback paths.
- **`Swatinem/rust-cache` in `pull_request` workflows must use `with.save-if` gating** --
  allow cache restore everywhere, but condition cache writes to trusted contexts (for example,
  `github.event_name != 'pull_request' ||
  github.event.pull_request.head.repo.full_name == github.repository`)
  so fork PRs cannot fail CI in `Swatinem/rust-cache` post-job save steps.
- **`taiki-e/install-action` can hit a transient Windows "bash startup failure"** -- the
  `windows-latest` runner image intermittently fails the install with
  `install-action: installation failed due to bash startup failure` BEFORE the action's own
  internal retries run (upstream runner-image bug,
  <https://github.com/actions/partner-runner-images/issues/169>). It is transient infra, not
  our bug. `install-action` exposes no retry input, so harden the install step with the repo's
  `id` + `continue-on-error: true` + `if: steps.<id>.outcome == 'failure'` retry idiom (the same
  pattern as the SBOM step in `release.yml`) rather than adding a third-party retry action — see
  the `ci.yml` nextest job. The retry is harmless cross-OS because the first attempt succeeds on
  non-Windows. When duplicating an action step for a retry, keep BOTH `uses:` lines on the same
  pinned ref so `test_same_action_uses_consistent_ref_across_workflows` stays green.
- **`cargo chef cook` emits benign `edition is set on ... which is deprecated` warnings** -- under
  cargo 1.88 the Docker build's `cargo chef cook` step prints
  `warning: edition is set on library/binary/benchmark signal_fish_server which is deprecated`.
  These originate from cargo-chef's generated `recipe.json` skeleton target tables, NOT from our
  `Cargo.toml` (which sets `edition` only in `[package]`). The build succeeds; the warnings are an
  external cargo-chef artifact. `cargo install cargo-chef` is pinned to an explicit version in the
  `Dockerfile` (currently `0.1.77`, the latest stable, which also trims lints from the recipe) for
  reproducibility — bump it deliberately, not implicitly via "latest".
- **Multi-arch container builds cross-compile; the gotchas are toolchain-shaped, not Rust-shaped** --
  the release image (`docker-publish.yml` + `Dockerfile`) is a `linux/amd64,linux/arm64,linux/arm/v7`
  manifest. The strategy is cross-compilation, NOT QEMU emulation of the compile: the `builder`
  stage is pinned to `--platform=$BUILDPLATFORM` and cross-compiles to `$TARGETARCH`; only the tiny
  runtime stage (`useradd` + a 2-package `apt-get`) runs under QEMU (hence `docker/setup-qemu-action`
  is still required). Hard-won specifics, all enforced by the
  `tests/ci_config_tests.rs::test_docker_publish_builds_multi_arch_manifest` /
  `test_dockerfile_cross_compiles_for_target_platform` guards and
  `REQUIRED_CONTAINER_PLATFORMS`/`REQUIRED_RELEASE_TARGETS`:
  (1) Native-vs-cross is decided by comparing `$TARGETARCH` to `$BUILDARCH`, NOT by hard-coding
  amd64 as "native" — GitHub's runner is amd64 (so arm64/armv7 are cross) but a dev box may be
  arm64 (so amd64 is the cross target); hard-coding breaks one or the other.
  (2) Under `--no-install-recommends` you MUST name the cross libc dev package explicitly
  (`libc6-dev-arm64-cross` / `libc6-dev-armhf-cross` / `libc6-dev-amd64-cross`) alongside the cross
  GCC; it is only a _recommends_ of the compiler, and without it linking dies with `cannot find
  Scrt1.o`. This bites the `aarch64-unknown-linux-gnu` job in `release.yml` too.
  (3) The image ships DEFAULT features (no `tls`), which keeps `aws-lc-sys`/`ring` (C-crypto needing
  cmake/nasm + a cross sysroot) out of the build entirely — cross-compiling stays pure-Rust +
  `getrandom` syscall + `libc`. Build release binaries with default features for the same reason.
  (4) `mold` is intentionally dropped from the multi-arch `Dockerfile` (it is still used in the
  devcontainer / mutation job): cross-link correctness across three arches beats the marginal
  link-time win. (5) `armv7-unknown-linux-gnueabihf` links fine once (2) is satisfied — armv7-A has
  64-bit atomics (LDREXD), so there is no `__atomic_*`/`-latomic` problem despite it being 32-bit.
- **`rustls-pemfile` (RUSTSEC-2025-0134) is banned proactively, not because it is present** --
  `deny.toml` carries a `[[bans.deny]]` for the unmaintained `rustls-pemfile`, but the crate is
  NOT in the dependency tree (`cargo tree -i rustls-pemfile` matches nothing) on the default build
  or via the optional `tls` feature. Our rustls stack parses PEM through the maintained
  `rustls-pki-types` instead. The ban exists to stop a future dependency bump from silently
  reintroducing the advisory (e.g., via the `tls` feature's `axum-server`/`rustls` cert+key
  loading). Keep the ban; revisit only if a needed dependency hard-requires `rustls-pemfile`.

# Documentation Requirements

See [Documentation Standards](skills/documentation-standards.md) for full standards.

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
  [CI/CD Troubleshooting Pattern 13](skills/ci-cd-troubleshooting-links.md).
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

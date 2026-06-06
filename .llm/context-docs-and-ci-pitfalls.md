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
- **Avoid accidental setext headings in skills** -- keep a blank line between
  `**Trigger**: ...` and a following `---` separator, or markdownlint will treat
  the trigger line as a heading (MD003/MD026).
- **Skill examples must be split into dedicated files** -- when documenting incidents or
  walkthroughs, create one `*-example-*.md` file per example and link from the parent
  skill. Do not keep multi-example "mega" sections inside a single skill file.
- **Use descriptive markdown link text for internal docs** -- avoid filename-as-label links
  like `[testing-core-patterns](...)`; prefer human-readable labels like
  `[Core Testing Patterns](...)`. Enforce with `./scripts/check-markdown-link-text.sh`.
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

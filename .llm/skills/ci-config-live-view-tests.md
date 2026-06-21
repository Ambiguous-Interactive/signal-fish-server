# Skill: CI Config Live View Tests

<!--
  trigger: live config, comment-stripped, read_live_file, strip_comment_lines,
  commented-out config, drift guard
  | Testing config presence assertions against active uncommented config
  | Infrastructure
-->

**Trigger**: When adding or reviewing Rust drift-guard tests that assert required
tokens exist in comment-bearing configuration files.

---

## When to Use

- Adding presence assertions for `.yml`, `Dockerfile`, `.sh`, `.ps1`, `.toml`,
  or JSONC files such as `.devcontainer/devcontainer.json`
- Moving repeated config-reader logic into `tests/common/mod.rs`
- Proving a config guard fails when the only matching token is commented out

---

## When NOT to Use

- Markdown files; `#` is a heading, not a comment
- Absence assertions where a commented occurrence should still fail the test
- Assertions that intentionally require a comment, such as shebangs or
  Dockerfile directives

---

## Rule

A raw `String::contains` presence assertion can pass after a required config
line is commented out, because the comment still contains the token. Presence
checks on comment-bearing config must assert against the live view:

```rust
let content = read_live_file(&root.join(".github/workflows/docker-publish.yml"));
assert!(content.contains("docker/setup-qemu-action"));
```

Use the helpers in `tests/common/mod.rs`:

- `read_live_file(path)` for full-file presence checks
- `strip_comment_lines(&block)` for extracted blocks
- raw `read_file(path)` for absence checks and required-comment checks

Inline trailing comments on live config lines are preserved. Only full-line
`#` and `//` comments are stripped.

---

## Regression Proof

When introducing or changing this class of guard, keep the helper contract
locked by tests in `tests/ci_config_tests.rs`:

- `test_strip_comment_lines_removes_full_line_hash_and_slash_comments`
- `test_read_live_file_is_strip_of_read_file`
- `test_drift_guards_reject_commented_out_config`

The final test is the red/green proof: a token that appears only in a commented
line must disappear from the live view, so a presence guard fails instead of
silently passing.

---

## Related Skills

- [CI Configuration Validation Tests](./github-actions-config-tests.md) — Main pattern for config drift guards
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Workflow authoring rules

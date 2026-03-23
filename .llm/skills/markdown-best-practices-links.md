# Skill: Markdown Link Validation

<!--
  trigger: markdown, link validation, lychee, relative links, anchor links, broken links, link checking
  | Best practices for internal and external link validation in Markdown documentation
  | Documentation
-->

**Trigger**: When fixing broken links, setting up link validation in CI, or dealing with case-sensitive
link failures on Linux.

See also:

- [Markdown Best Practices Code Blocks](./markdown-best-practices-code-blocks.md) — Code block best practices
- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — Linting rules and CI/CD integration
- [Markdown Best Practices Formatting](./markdown-best-practices-formatting.md) — Proper nouns and spell checking

---

## TL;DR

- Use relative paths for internal documentation: `[Guide](../docs/guide.md)`
- Use human-readable internal link labels (for example `Core Testing Patterns`, not
  `testing-core-patterns`)
- Case sensitivity matters on Linux — verify exact filename case
- Test links locally with lychee before pushing
- Exclude placeholder URLs in `.lychee.toml`, not real documentation links

---

## Link Label Quality

For internal markdown links, the visible label should describe the destination,
not repeat the literal filename.

```markdown
❌ REDUNDANT
See [testing-core-patterns](../.llm/skills/testing-core-patterns.md).

✅ HUMAN-READABLE
See [Core Testing Patterns](../.llm/skills/testing-core-patterns.md).
```

Enforce this rule with:

```bash
./scripts/check-markdown-link-text.sh
./scripts/check-markdown-link-text.sh --fix
```

---

## Relative vs Absolute Links

**Internal documentation:**
Use relative links for files within the repository:

```markdown
✅ CORRECT: Relative link
See the [configuration guide](../docs/configuration.md) for details.

❌ WRONG: Absolute GitHub URL
See the [configuration guide](https://github.com/myorg/myrepo/blob/main/docs/configuration.md).
```

**Benefits of relative links:**

- Work in forks and local clones
- Work offline
- No broken links when repository is renamed
- Faster to type and maintain

**External resources:**
Use absolute URLs for external documentation:

```markdown
✅ CORRECT: Absolute URL for external resource
See the [Tokio documentation](https://tokio.rs) for async patterns.
```

---

## Case Sensitivity

Linux filesystems are case-sensitive. Links must match filename case exactly.

```markdown
❌ WRONG: Case mismatch (works on macOS/Windows, fails on Linux)
See [testing guide](Skills/testing-core-patterns.md)
# Actual file: skills/testing-core-patterns.md

✅ CORRECT: Exact case match
See [testing guide](skills/testing-core-patterns.md)
```

**How to avoid case sensitivity issues:**

1. Use tab completion when creating links locally
2. Run link validation locally before pushing
3. Test on Linux (WSL, Docker, or CI) if developing on macOS/Windows
4. Use consistent casing convention (prefer lowercase for directory names)

---

## Anchor Links

Markdown headers automatically become link anchors:

```markdown
# Section Title
This creates anchor: #section-title

# Multi-Word Section
This creates anchor: #multi-word-section

# Section with Code: `main()`
This creates anchor: #section-with-code-main
```

**Linking to anchors:**

```markdown
✅ CORRECT: Link to section in same file
See the [installation section](#installation) below.

✅ CORRECT: Link to section in another file
See [testing patterns](testing.md#unit-testing-patterns).

❌ WRONG: Incorrect anchor transformation
See [testing patterns](testing.md#Unit-Testing-Patterns).
# Anchors are lowercase with hyphens, not title case
```

**Anchor transformation rules:**

1. Convert to lowercase
2. Replace spaces with hyphens
3. Remove most punctuation (except hyphens)
4. Keep alphanumeric characters

---

## Placeholder URLs and Test Fixtures

**Problem:** Test code and documentation examples often contain placeholder URLs that should not be validated.

**Solution:** Configure `.lychee.toml` to exclude placeholder patterns:

```toml
# .lychee.toml
exclude = [
    # Test fixture and example URLs
    "https://github.com/owner/repo/*",
    "https://github.com/%7B%7B%7D/*",  # URL-encoded {{{}}} placeholder
    "https://github.com/{}/*",          # Template placeholder
    "https://example.com/*",
    "http://localhost*",
]
```

**Pattern:** Exclude by URL pattern, not by file path. This allows you to:

- Keep placeholder URLs in test fixtures
- Skip validation of example code
- Avoid false positives in CI

**When NOT to exclude:**

- Real documentation links (even in test files)
- Links to actual dependencies or tools
- Links that readers will actually follow

---

## Common Pitfalls

### Pitfall 1: Case Sensitivity in Links

**Symptom:** Links work locally (macOS/Windows) but fail in CI (Linux).

**Solution:**

- Verify link case matches filename exactly
- Use tab completion when creating links
- Test on Linux before pushing (WSL, Docker)

**Prevention:**

```rust
// Add to tests/ci_config_tests.rs
#[test]
fn test_documentation_links_case_sensitive() {
    // Verify all markdown links point to existing files
    // with correct case
}
```

### Pitfall 2: Absolute URLs for Internal Links

**Symptom:** Links break when repository is forked or renamed.

**Solution:**
Use relative paths for internal documentation:

```markdown
❌ WRONG: Absolute GitHub URL
[config](https://github.com/org/repo/blob/main/docs/config.md)

✅ CORRECT: Relative path
[config](../docs/config.md)
```

### Pitfall 3: Not Excluding Test Fixtures

**Symptom:** Link checker fails on placeholder URLs in test code.

**Solution:**
Configure `.lychee.toml` to exclude test fixtures by URL pattern:

```toml
exclude = [
    "https://example.com/*",
    "https://github.com/owner/repo/*",
    "http://localhost*",
]
```

---

## Local Validation

```bash
# Run link checking
lychee --config .lychee.toml './**/*.md'

# Auto-fix markdown issues
markdownlint-cli2 --fix '**/*.md' '#target/**'
```

---

## Pre-commit Hook for Link Checking

Add to `.githooks/pre-commit`:

```bash
# Track blocking checks
FAILURES=0

# Check for links
if command -v lychee >/dev/null 2>&1; then
    echo "[pre-commit] Checking links (offline mode)..."
    STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)
    if [ -n "$STAGED_MD" ]; then
        # shellcheck disable=SC2086
        if ! lychee --offline --config .lychee.toml $STAGED_MD >/dev/null 2>&1; then
            echo "[pre-commit] ERROR: Link checking failed"
            FAILURES=$((FAILURES + 1))
        fi
    fi
else
    echo "[pre-commit] Skipping link check (lychee not installed)"
fi

if [ "$FAILURES" -ne 0 ]; then
    exit 1
fi
```

---

## GitHub Actions Integration

```yaml
link-check:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v6.0.2
    - uses: lycheeverse/lychee-action@v2.7.0
      with:
        args: --verbose './**/*.md' --config .lychee.toml
      env:
        GITHUB_TOKEN: ${{secrets.GITHUB_TOKEN}}
```

---

## Related Skills

- [Markdown Best Practices Code Blocks](./markdown-best-practices-code-blocks.md) — Code block best practices
- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — Linting rules and CI/CD integration
- [Markdown Best Practices Formatting](./markdown-best-practices-formatting.md) — Proper nouns and spell checking
- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Debugging link check failures

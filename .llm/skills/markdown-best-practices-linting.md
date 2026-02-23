# Skill: Markdown Linting and CI/CD Integration

<!--
  trigger: markdown, linting, markdownlint, MD040, MD041, MD013, CI/CD, markdown validation, typos
  | Markdown linting rules, CI/CD integration, and validation testing
  | Documentation
-->

**Trigger**: When setting up markdown linting in CI, fixing markdownlint errors, or writing
tests for markdown validation.

See also:

- [markdown-best-practices-code-blocks](./markdown-best-practices-code-blocks.md) — Code block best practices
- [markdown-best-practices-links](./markdown-best-practices-links.md) — Link validation
- [markdown-best-practices-formatting](./markdown-best-practices-formatting.md) — Proper nouns and spell checking

---

## TL;DR

- MD040: All fenced code blocks must specify a language identifier
- MD041: First line should be a top-level heading
- MD013: Line length — disable for technical docs (`"MD013": false`)
- Run `./scripts/check-markdown.sh fix` to auto-fix most issues with the pinned tool version
- Use `typos` for spell checking; configure via `.typos.toml`

---

## Markdown Linting Rules

### MD040: Code Blocks Must Have Language

**Rule:** All fenced code blocks must specify a language identifier.

**Why:** Enables syntax highlighting, aids accessibility, prevents ambiguity.

**Fix:**

````markdown
❌ BEFORE:
```
fn main() {}
```

✅ AFTER:
```rust
fn main() {}
```
````

### MD013: Line Length

**Rule:** Lines should not exceed specified length (often 80 or 120 characters).

**Why:** Improves readability, works better with diff tools.

**When to disable:** Technical documentation often has long lines (URLs, code examples, tables).

**Configuration:**

```json
{
  "MD013": false
}
```

### MD041: First Line Should Be Top-Level Heading

**Rule:** Markdown files should start with a `# Heading`.

**Why:** Improves document structure, aids navigation.

**Fix:**

```markdown
❌ BEFORE:
This is a paragraph...

✅ AFTER:
# Document Title

This is a paragraph...
```

### Table Formatting (Project Style)

Keep table formatting readable and consistent even when not enforced by a specific
markdownlint rule. Use `markdownlint-cli2 --fix` for basic normalization, then
manually align difficult tables if needed.

### Lint Test Fixtures

Test fixture markdown files often contain intentional lint violations. Exclude them
from linting by adding paths to `.markdownlintignore` rather than weakening rules
project-wide.

---

## Local Validation

**Before committing:**

```bash
# Install pinned markdownlint version once (see .markdownlint-version)
npm install -g markdownlint-cli2@$(cat .markdownlint-version)

# Run markdown linting (uses pinned version enforcement)
./scripts/check-markdown.sh

# Run link checking
lychee --config .lychee.toml './**/*.md'

# Run spell checking
typos

# Auto-fix markdown issues
./scripts/check-markdown.sh fix
```

---

## Pre-commit Hook

Add to `.githooks/pre-commit` or `.git/hooks/pre-commit`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Check markdown files via the project script.
# The script enforces the pinned markdownlint-cli2 version.
if [ -x scripts/check-markdown.sh ]; then
    if ! scripts/check-markdown.sh; then
        echo "[pre-commit] ERROR: Markdown linting failed"
        echo "[pre-commit] To auto-fix: ./scripts/check-markdown.sh fix"
        exit 1
    fi
fi

# Check for typos
if command -v typos >/dev/null 2>&1; then
    echo "[pre-commit] Checking for typos..."
    typos
fi
```

---

## GitHub Actions Workflow

**Minimal workflow:**

```yaml
name: Documentation Validation

on:
  push:
    branches: [main]
    paths:
      - '**/*.md'
      - '.markdownlint.json'
      - '.markdownlintignore'
      - '.lychee.toml'
      - '.typos.toml'
      - '.github/workflows/markdownlint.yml'
  pull_request:
    branches: [main]
    paths:
      - '**/*.md'
      - '.markdownlint.json'
      - '.markdownlintignore'
      - '.lychee.toml'
      - '.typos.toml'
      - '.github/workflows/markdownlint.yml'

jobs:
  markdown-lint:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@<SHA> # v4.2.2
      - uses: DavidAnson/markdownlint-cli2-action@<SHA> # v22.0.0
        with:
          globs: |
            **/*.md
            !target/**
            !node_modules/**

  link-check:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@<SHA> # v4.2.2
      - uses: lycheeverse/lychee-action@<SHA> # v2.7.0
        with:
          args: --verbose './**/*.md' --config .lychee.toml
        env:
          GITHUB_TOKEN: ${{secrets.GITHUB_TOKEN}}

  spell-check:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@<SHA> # v4.2.2
      - uses: crate-ci/typos@<SHA> # v1.30.1
        with:
          config: .typos.toml
```

**Key features:**

- Runs on markdown and related documentation-config changes (path filters)
- Separate jobs for different types of validation
- Uses official actions with SHA pinning
- Explicitly sets least-privilege `permissions` for each job

---

## Testing Markdown Validation

### Data-Driven Tests

Add to `tests/ci_config_tests.rs`:

```rust
#[test]
fn test_markdown_files_have_language_identifiers() {
    // Walk markdown files; flag any ``` line with no language identifier (MD040)
    let markdown_files = find_markdown_files(&repo_root());
    let mut violations = Vec::new();
    for file in markdown_files {
        let content = read_file(&file);
        for (line_num, line) in content.lines().enumerate() {
            let fence = line.trim_start().trim_start_matches('`').trim();
            if line.trim_start().starts_with("```") && fence.is_empty() {
                violations.push(format!("{}:{}: missing language (MD040)", file.display(), line_num + 1));
            }
        }
    }
    assert!(violations.is_empty(), "Code blocks without language:\n{}", violations.join("\n"));
}

#[test]
fn test_typos_config_has_required_sections() {
    let content = read_file(&repo_root().join(".typos.toml"));
    assert!(content.contains("[default.extend-words]"));
    assert!(content.contains("[default.extend-identifiers]"));
}

#[test]
fn test_markdownlint_config_exists() {
    assert!(repo_root().join(".markdownlint.json").exists());
}
```

---

## Checklist: Markdown Documentation Quality

Before committing markdown changes:

- [ ] All code blocks have language identifiers
- [ ] Proper nouns use correct capitalization (HashiCorp, not Hashicorp)
- [ ] Internal links use relative paths, not absolute GitHub URLs
- [ ] Link case matches filename case exactly (test on Linux if developing on macOS/Windows)
- [ ] Technical terms added to `.typos.toml` in correct section
- [ ] Mixed-case terms in `[default.extend-identifiers]`, lowercase in `[default.extend-words]`
- [ ] Tables have consistent column alignment
- [ ] No trailing whitespace
- [ ] File starts with top-level heading (`# Title`)
- [ ] Local validation passes: `./scripts/check-markdown.sh`, `lychee`, `typos`

---

## Quick Reference: Validation Commands

```bash
./scripts/check-markdown.sh                # Lint
./scripts/check-markdown.sh fix            # Auto-fix
lychee --config .lychee.toml './**/*.md'  # Links
typos                                      # Spell check
```

---

## Related Skills

- [markdown-best-practices-code-blocks](./markdown-best-practices-code-blocks.md) — Code block language identifiers
- [markdown-best-practices-links](./markdown-best-practices-links.md) — Link validation patterns
- [markdown-best-practices-formatting](./markdown-best-practices-formatting.md) — Proper nouns, spell checking
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD integration
- [ci-cd-troubleshooting](./ci-cd-troubleshooting-categories.md) — Debugging markdown lint failures
- [testing-strategies](./testing-core-patterns.md) — Data-driven tests for validation

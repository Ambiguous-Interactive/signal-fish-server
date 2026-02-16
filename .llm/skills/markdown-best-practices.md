# Skill: Markdown Best Practices

<!-- trigger: markdown, documentation, link validation, code blocks, proper nouns, capitalization, technical writing | Best practices for writing and validating Markdown documentation | Documentation -->

**Trigger**: When writing Markdown documentation, fixing link validation issues, or improving documentation quality.

---

## When to Use

- Writing or editing Markdown documentation files
- Fixing link validation failures in CI
- Dealing with proper noun capitalization in documentation
- Adding code blocks to documentation
- Resolving spell checking issues in technical documentation
- Setting up markdown validation in CI/CD

---

## When NOT to Use

- Code comments (see [documentation-standards](./documentation-standards.md))
- API documentation (see [api-design-guidelines](./api-design-guidelines.md))
- Error messages and logging (see [observability-and-logging](./observability-and-logging.md))

---

## TL;DR

**Code Blocks:**
- Always specify language identifier: ` ```rust`, ` ```bash`, ` ```json`, never ` ```
- Use lowercase language names: ` ```rust` not ` ```Rust`
- For plain text examples, use ` ```text`

**Proper Nouns:**
- Capitalize correctly: "HashiCorp", "GitHub", "WebSocket", "Rust"
- Technical identifiers (filenames, field names) stay lowercase in prose
- When in doubt, check the official brand guidelines

**Links:**
- Use relative paths for internal documentation: `[guide](../docs/guide.md)`
- Case sensitivity matters on Linux - verify exact filename case
- Test links locally with lychee before pushing

**Spell Checking:**
- Add technical terms to `.typos.toml`
- Lowercase terms go in `[default.extend-words]`
- Mixed-case terms go in `[default.extend-identifiers]`

---

## Code Block Language Identifiers

### The Rule: Always Specify Language

Every code block MUST have a language identifier for proper syntax highlighting and validation.

```markdown
❌ WRONG: No language identifier
```
some code here
```

✅ CORRECT: Language identifier specified
```rust
fn main() {
    println!("Hello, world!");
}
```
```

### Common Language Identifiers

| Content Type | Identifier | Example |
|--------------|------------|---------|
| Rust code | `rust` | ` ```rust` |
| Shell commands | `bash` or `sh` | ` ```bash` |
| JSON | `json` | ` ```json` |
| TOML | `toml` | ` ```toml` |
| YAML | `yaml` or `yml` | ` ```yaml` |
| Plain text/output | `text` | ` ```text` |
| Dockerfile | `dockerfile` | ` ```dockerfile` |
| SQL | `sql` | ` ```sql` |
| JavaScript | `javascript` or `js` | ` ```javascript` |
| TypeScript | `typescript` or `ts` | ` ```typescript` |
| Python | `python` | ` ```python` |

### Code Block Attributes

Rust code blocks can have special attributes:

```markdown
```rust,ignore
// This code won't be tested by rustdoc
```

```rust,no_run
// This code will be compiled but not executed
```

```rust,should_panic
// This code is expected to panic
```

```rust,edition2021
// Specify Rust edition
```
```

**Attribute Formats:**
- Comma-separated: ` ```rust,ignore`
- Space-separated: ` ```rust ignore` (also valid)
- Multiple attributes: ` ```rust,ignore,no_run` or ` ```rust ignore no_run`

### Case Sensitivity

Language identifiers should be lowercase for consistency:

```markdown
✅ CORRECT: Lowercase
```rust
fn main() {}
```

❌ AVOID: Mixed case (works but inconsistent)
```Rust
fn main() {}
```
```

**Exception:** Both `rust` and `Rust` are valid and work identically. Use lowercase for consistency.

---

## Proper Noun Capitalization

### The Challenge

Technical documentation contains a mix of:
1. **Proper nouns** (company names, product names) - require specific capitalization
2. **Technical identifiers** (filenames, field names, code patterns) - must match code exactly
3. **Common nouns** (general terms) - follow standard English rules

### Guidelines

#### Company and Product Names

Always use official capitalization:

| Correct | Incorrect |
|---------|-----------|
| HashiCorp | Hashicorp, hashicorp |
| GitHub | Github, github |
| WebSocket | Websocket, websocket |
| PostgreSQL | Postgresql, postgres |
| MongoDB | Mongodb, mongo |
| JavaScript | Javascript, javascript |
| TypeScript | Typescript, typescript |

**How to find official capitalization:**
1. Check the company's official website
2. Look at their GitHub organization name
3. Check their brand guidelines
4. When in doubt, use the capitalization from their logo

#### Technical Terms and Protocols

Protocol and technology names often have specific capitalization:

| Term | Capitalization | Context |
|------|----------------|---------|
| WebSocket | Mixed case | Protocol name (proper noun) |
| websocket | Lowercase | In code, URLs (`ws://`), or when referring to the concept generically |
| WebRTC | Mixed case | Protocol name |
| REST API | Uppercase | Architectural style |

**Pattern:**
- Proper noun/brand: Use official capitalization
- In prose referring to the concept: Can be lowercase
- In code/technical context: Match code exactly

#### File Names and Code Identifiers

When referring to files, functions, or code elements, match the code exactly:

```markdown
✅ CORRECT: Matches actual filenames
The `Cargo.toml` file defines dependencies.
Edit `src/main.rs` to change the entry point.
The `signal_fish_server` crate provides the library API.

❌ WRONG: Doesn't match code
The `cargo.toml` file defines dependencies.
Edit `src/Main.rs` to change the entry point.
The `SignalFishServer` crate provides the library API.
```

**Rule:** Technical identifiers are not proper nouns - they must match code exactly, regardless of capitalization rules.

---

## Link Validation

### Relative vs Absolute Links

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

### Case Sensitivity

Linux filesystems are case-sensitive. Links must match filename case exactly.

```markdown
❌ WRONG: Case mismatch (works on macOS/Windows, fails on Linux)
See [testing guide](Skills/testing-strategies.md)
# Actual file: skills/testing-strategies.md

✅ CORRECT: Exact case match
See [testing guide](skills/testing-strategies.md)
```

**How to avoid case sensitivity issues:**
1. Use tab completion when creating links locally
2. Run link validation locally before pushing
3. Test on Linux (WSL, Docker, or CI) if developing on macOS/Windows
4. Use consistent casing convention (prefer lowercase for directory names)

### Anchor Links

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

### Placeholder URLs and Test Fixtures

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

## Spell Checking Configuration

### The Tool: typos

The `typos` spell checker has two configuration sections with different purposes:

#### `[default.extend-words]` - Lowercase Technical Terms

For lowercase technical terms, abbreviations, and common technical jargon:

```toml
[default.extend-words]
# Rust ecosystem
rustc = "rustc"
tokio = "tokio"
axum = "axum"
serde = "serde"
clippy = "clippy"

# Build tools
dockerfile = "dockerfile"
yaml = "yaml"

# Technical terms
websocket = "websocket"
async = "async"
```

**Use for:**
- Rust crate names
- Command-line tools
- File formats
- Technical abbreviations
- Common typos that are actually correct in technical context

#### `[default.extend-identifiers]` - Mixed-Case Terms

For company names, product names, and code identifiers with mixed case:

```toml
[default.extend-identifiers]
# Company names (proper nouns with specific capitalization)
HashiCorp = "HashiCorp"  # NOT "Hashicorp" or "hashicorp"
GitHub = "GitHub"        # NOT "Github" or "github"

# Protocol names
WebSocket = "WebSocket"  # NOT "Websocket"
WebRTC = "WebRTC"

# Code identifiers
CamelCase = "CamelCase"
PascalCase = "PascalCase"
```

**Use for:**
- Company names with specific capitalization
- Product/brand names with mixed case
- Protocol names with mixed case
- Code identifiers (CamelCase, PascalCase)

### Why Two Sections?

The `typos` tool splits identifiers on case boundaries:
- `HashiCorp` → `Hash` + `I` + `Corp` (analyzed as separate components)
- `hashicorp` → `hashicorp` (analyzed as single word)

**This means:**
- `extend-words` handles unsplit, lowercase words
- `extend-identifiers` handles split, mixed-case terms

**If you put `HashiCorp` in `extend-words`, it won't work** because typos splits it into components and doesn't find a match.

### Adding New Technical Terms

**Process:**
1. CI fails with typo error
2. Verify it's a legitimate technical term (not an actual typo)
3. Determine if it's lowercase or mixed-case
4. Add to appropriate section in `.typos.toml`
5. Re-run validation

**Example:**

```bash
# CI error:
# ERROR: Typo found: HashiCorp (did you mean: Hashicorp?)

# 1. Verify: Check official website - it's "HashiCorp"
# 2. Identify: Mixed-case company name
# 3. Add to .typos.toml:
[default.extend-identifiers]
HashiCorp = "HashiCorp"  # Company name, proper capitalization

# 4. Commit and push
```

### Common Terms to Whitelist

**Rust Ecosystem:**
```toml
[default.extend-words]
rustc = "rustc"
rustup = "rustup"
rustfmt = "rustfmt"
clippy = "clippy"
tokio = "tokio"
axum = "axum"
serde = "serde"
async = "async"
await = "await"
impl = "impl"
```

**Build and Infrastructure:**
```toml
[default.extend-words]
dockerfile = "dockerfile"
yaml = "yaml"
toml = "toml"
json = "json"
github = "github"
gitlab = "gitlab"
cicd = "cicd"
```

**Networking and Protocols:**
```toml
[default.extend-words]
websocket = "websocket"
webrtc = "webrtc"
http = "http"
https = "https"

[default.extend-identifiers]
WebSocket = "WebSocket"
WebRTC = "WebRTC"
```

---

## Markdown Linting Rules

### MD040: Code Blocks Must Have Language

**Rule:** All fenced code blocks must specify a language identifier.

**Why:** Enables syntax highlighting, aids accessibility, prevents ambiguity.

**Fix:**
```markdown
❌ BEFORE:
```
fn main() {}
```

✅ AFTER:
```rust
fn main() {}
```
```

### MD060: Table Alignment

**Rule:** Table columns must have consistent alignment.

**Why:** Improves readability, prevents parsing errors.

**Fix:**
```markdown
❌ BEFORE:
| Column | Value |
|--------|-------|
|  foo   | bar  |

✅ AFTER:
| Column | Value |
|--------|-------|
| foo    | bar   |
```

**Auto-fix:** Run `markdownlint-cli2 --fix '**/*.md'`

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

---

## CI/CD Integration

### Local Validation

**Before committing:**

```bash
# Run markdown linting
markdownlint-cli2 '**/*.md' '#target/**' '#node_modules/**'

# Run link checking
lychee --config .lychee.toml './**/*.md'

# Run spell checking
typos

# Auto-fix markdown issues
markdownlint-cli2 --fix '**/*.md' '#target/**'
```

### Pre-commit Hook

Add to `.githooks/pre-commit` or `.git/hooks/pre-commit`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Check markdown files (if markdownlint-cli2 is installed)
if command -v markdownlint-cli2 >/dev/null 2>&1; then
    echo "[pre-commit] Checking markdown files..."
    if ! markdownlint-cli2 '**/*.md' '#target/**' '#node_modules/**'; then
        echo "[pre-commit] ERROR: Markdown linting failed"
        echo "[pre-commit] To auto-fix: markdownlint-cli2 --fix '**/*.md'"
        exit 1
    fi
else
    echo "[pre-commit] Skipping markdown check (markdownlint-cli2 not installed)"
fi

# Check for typos
if command -v typos >/dev/null 2>&1; then
    echo "[pre-commit] Checking for typos..."
    typos
fi
```

### GitHub Actions Workflow

**Minimal workflow:**

```yaml
name: Documentation Validation

on:
  push:
    branches: [main]
    paths: ['**/*.md']
  pull_request:
    branches: [main]
    paths: ['**/*.md']

jobs:
  markdown-lint:
    runs-on: ubuntu-latest
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
    steps:
      - uses: actions/checkout@<SHA> # v4.2.2
      - uses: lycheeverse/lychee-action@<SHA> # v2.7.0
        with:
          args: --verbose './**/*.md' --config .lychee.toml
        env:
          GITHUB_TOKEN: ${{secrets.GITHUB_TOKEN}}

  spell-check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<SHA> # v4.2.2
      - uses: crate-ci/typos@<SHA> # v1.30.1
        with:
          config: .typos.toml
```

**Key features:**
- Only runs on markdown file changes (path filters)
- Separate jobs for different types of validation
- Uses official actions with SHA pinning
- Includes configuration file changes in path triggers

---

## Common Pitfalls and Solutions

### Pitfall 1: Forgetting Language Identifiers

**Symptom:** CI fails with MD040 errors.

**Solution:**
```bash
# Find all code blocks without language identifiers
grep -r '^```$' --include="*.md" .

# Manually add language after opening backticks
# Or use auto-fix if your linter supports it
```

### Pitfall 2: Case Sensitivity in Links

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

### Pitfall 3: Mixed-Case Terms in Wrong Section

**Symptom:** Typos checker still flags "HashiCorp" even though it's in `.typos.toml`.

**Solution:**
```toml
# ❌ WRONG: Mixed-case in extend-words
[default.extend-words]
HashiCorp = "HashiCorp"  # Won't work

# ✅ CORRECT: Mixed-case in extend-identifiers
[default.extend-identifiers]
HashiCorp = "HashiCorp"  # Works
```

### Pitfall 4: Absolute URLs for Internal Links

**Symptom:** Links break when repository is forked or renamed.

**Solution:**
Use relative paths for internal documentation:

```markdown
❌ WRONG: Absolute GitHub URL
[config](https://github.com/org/repo/blob/main/docs/config.md)

✅ CORRECT: Relative path
[config](../docs/config.md)
```

### Pitfall 5: Not Excluding Test Fixtures

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

## VS Code Integration

### Recommended Extensions

Add to `.vscode/extensions.json`:

```json
{
  "recommendations": [
    "davidanson.vscode-markdownlint",
    "streetsidesoftware.code-spell-checker"
  ]
}
```

### Settings Configuration

Add to `.vscode/settings.json`:

```json
{
  "markdownlint.config": {
    "MD040": true,  // Require language identifiers
    "MD013": false  // Disable line length (too strict for technical docs)
  },
  "cSpell.words": [
    "rustc",
    "tokio",
    "axum",
    "HashiCorp",
    "WebSocket"
  ]
}
```

**Benefits:**
- Real-time linting feedback
- Auto-fix on save (if configured)
- Spell checking with technical terms
- Consistent formatting across team

---

## Testing Markdown Validation

### Data-Driven Tests

Add to `tests/ci_config_tests.rs`:

```rust
#[test]
fn test_markdown_files_have_language_identifiers() {
    let markdown_files = find_markdown_files(&repo_root());
    let mut violations = Vec::new();

    for file in markdown_files {
        let content = read_file(&file);

        for (line_num, line) in content.lines().enumerate() {
            // Check for opening code fence without language
            let fence_marker = "```";
            if line.trim_start().starts_with(fence_marker) {
                let fence_content = line.trim_start()
                    .trim_start_matches('`')
                    .trim();

                if fence_content.is_empty() {
                    violations.push(format!(
                        "{}:{}: Code block missing language identifier (MD040)",
                        file.display(),
                        line_num + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found code blocks without language identifiers:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_typos_config_has_required_sections() {
    let typos_config = repo_root().join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml is required for spell checking"
    );

    let content = read_file(&typos_config);

    assert!(
        content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section"
    );

    assert!(
        content.contains("[default.extend-identifiers]"),
        ".typos.toml must have [default.extend-identifiers] section"
    );
}

#[test]
fn test_markdownlint_config_exists() {
    let config = repo_root().join(".markdownlint.json");

    assert!(
        config.exists(),
        ".markdownlint.json is required for markdown linting"
    );
}
```

**Benefits:**
- Catch issues during `cargo test` (before CI)
- Fast execution (< 1 second)
- Clear error messages with file locations
- Prevents regression

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
- [ ] Local validation passes: `markdownlint-cli2`, `lychee`, `typos`

---

## Related Skills

- [documentation-standards](./documentation-standards.md) - Overall documentation requirements and quality standards
- [github-actions-best-practices](./github-actions-best-practices.md) - CI/CD integration for validation
- [ci-cd-troubleshooting](./ci-cd-troubleshooting.md) - Debugging link check and markdown lint failures
- [testing-strategies](./testing-strategies.md) - Data-driven tests for markdown validation
- [api-design-guidelines](./api-design-guidelines.md) - API documentation patterns

---

## Quick Reference

### Language Identifiers
- Rust: `rust`
- Shell: `bash` or `sh`
- Plain text: `text`
- JSON/YAML/TOML: `json`, `yaml`, `toml`

### Capitalization
- Company names: Official capitalization (HashiCorp, GitHub)
- Technical terms: Match code exactly (`Cargo.toml`, `main.rs`)
- Protocols: Mixed case for proper noun (WebSocket), lowercase in technical context

### Spell Checking
- Lowercase technical terms: `[default.extend-words]`
- Mixed-case company names: `[default.extend-identifiers]`

### Validation Commands
```bash
markdownlint-cli2 '**/*.md'                # Lint
markdownlint-cli2 --fix '**/*.md'          # Auto-fix
lychee --config .lychee.toml './**/*.md'  # Links
typos                                      # Spell check
```

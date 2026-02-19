# Skill: Markdown Best Practices

<!--
  trigger: markdown, documentation, link validation, code blocks, proper nouns, capitalization, technical writing
  | Best practices for writing and validating Markdown documentation
  | Documentation
-->

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

````markdown
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
````

### Common Language Identifiers

| Content Type | Identifier | Example |
|--------------|------------|---------|
| Rust code | `rust` | ` ```rust` |
| Shell commands | `bash` or `sh` | ` ```bash` |
| JSON | `json` | ` ```json` |
| JSON with Comments | `jsonc` | ` ```jsonc` |
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

````markdown
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
````

**Attribute Formats:**

- Comma-separated: ` ```rust,ignore`
- Space-separated: ` ```rust ignore` (also valid)
- Multiple attributes: ` ```rust,ignore,no_run` or ` ```rust ignore no_run`

### Case Sensitivity

Language identifiers should be lowercase for consistency:

````markdown
✅ CORRECT: Lowercase
```rust
fn main() {}
```

❌ AVOID: Mixed case (works but inconsistent)
```Rust
fn main() {}
```
````

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
| HashiCorp | `Hashicorp`, `hashicorp` |
| GitHub | `Github`, `github` |
| WebSocket | `Websocket`, `websocket` |
| PostgreSQL | `Postgresql`, `postgres` |
| MongoDB | `Mongodb`, `mongo` |
| JavaScript | `Javascript`, `javascript` |
| TypeScript | `Typescript`, `typescript` |

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
| `websocket` | Lowercase | In code, URLs (`ws://`), or when referring to the concept generically |
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

### Pitfall 6: MD044 Proper Names vs Docker Image References

<!-- markdownlint-disable-next-line MD044 -->
MD044 flags lowercase `rust` as a proper noun violation, but Docker image names
like `rust:1.88` **must** stay lowercase. Wrap Docker image references in backtick
inline code (e.g., `` `rust:1.88` ``) to suppress MD044 inside code spans.

### Pitfall 7: MD044 and URLs in HTML Attributes

The `.markdownlint.json` has `"html_elements": false` for MD044, meaning content inside
HTML elements is not checked for proper name capitalization. URLs in HTML attributes
(`href="..."`, `src="..."`) contain domain names like `github.io` that are correctly
lowercase.

The custom test `test_markdown_technical_terms_consistency()` mirrors this by stripping:

1. Markdown link URLs: `[text](url)` becomes `[text]`
2. HTML elements: `<a href="...">` tags are removed entirely
3. Raw URLs: `https://...`, `wss://...`, `ftp://...` are removed

**Example false positive (now prevented):**

```text
README.md:10: Incorrect capitalization: should be 'GitHub'
  Line: <a href="https://ambiguous-interactive.github.io/signal-fish-server/">
```

The `github` in `github.io` is a domain name and must stay lowercase.

**If adding new URL schemes** (e.g., `ssh://`), update the `RAW_URL_STRIP_PATTERN`
constant in `tests/ci_config_tests.rs`.

### Pitfall 8: Code Block Fence Tracking in Nested Examples

Opening fences can have info strings (`` ```rust ``), but closing fences must be
bare (`` ``` ``). A naive toggle (flip `in_block` on every `` ``` `` line) breaks
when documentation contains nested fence examples. Always match closing fences with
an exact `/^```$/` pattern.

### Pitfall 9: MD060 and Compact Table Styles

MD060 (no-space-in-code) may fire false positives on compact table styles that omit
padding around pipe characters. If your project uses compact tables, consider
disabling MD060 in `.markdownlint.json`.

### Pitfall 10: Lint Test Fixtures

Test fixture markdown files often contain intentional lint violations. Exclude them
from linting by adding paths to `.markdownlintignore` rather than weakening rules
project-wide.

### Pitfall 11: `json` vs `jsonc` Code Fence Tags

JSON with Comments (JSONC) uses `//` or `/* */` style comments. Standard JSON does
**not** allow comments. If a code block contains comments, use `` ```jsonc `` instead
of `` ```json ``. Using the wrong tag causes JSON validators to report syntax errors
on comment lines.

````markdown
❌ WRONG: Comments inside a json block
```json
{
  // This comment makes the JSON invalid
  "key": "value"
}
```

✅ CORRECT: Use jsonc for JSON with comments
```jsonc
{
  // Comments are valid in JSONC
  "key": "value"
}
```

✅ ALSO CORRECT: Remove comments for pure json
```json
{
  "key": "value"
}
```
````

**When to use each tag:**

| Content | Tag | Example |
|---------|-----|---------|
| Pure JSON (no comments) | `json` | API responses, `Cargo.lock` |
| JSON with `//` comments | `jsonc` | VS Code `settings.json`, `tsconfig.json` |
| JSON with `/* */` comments | `jsonc` | Configuration with inline docs |
| JSON with placeholder values like `[...]` | `jsonc` | Abbreviated examples |

**Why this matters:**

- CI validators may parse `` ```json `` blocks with a strict JSON parser
- `//` is not valid JSON syntax and causes parse errors
- `[...]` as a placeholder (meaning "more items here") is not valid JSON
- Using `jsonc` signals to validators and syntax highlighters that the
  content follows relaxed JSON rules

### Pitfall 12: Invalid Placeholders in JSON Code Blocks

Documentation sometimes uses `[...]` or `...` as shorthand for "more items here."
These are not valid JSON. Either use `jsonc` as the fence tag, or replace the
placeholder with valid JSON.

````markdown
❌ WRONG: Invalid placeholder in json block
```json
{
  "items": [
    "first",
    [...]
  ]
}
```

✅ CORRECT option A: Use jsonc tag
```jsonc
{
  "items": [
    "first",
    // ... more items
  ]
}
```

✅ CORRECT option B: Use valid JSON
```json
{
  "items": [
    "first",
    "second",
    "third"
  ]
}
```
````

### Pitfall 13: Mixed-Content Blocks Must Be Split

A single code block must contain only one language. When documentation shows a
sequence that spans multiple languages (e.g., shell commands that produce YAML
output, or a setup guide mixing bash and YAML), split the content into separate
fenced blocks with the correct tag for each.

**Why:** CI validators parse blocks according to their language tag. A
`` ```yaml `` block containing shell commands fails YAML parsing; a
`` ```bash `` block containing YAML fragments may fail `bash -n` syntax checks.

`````markdown
<!-- markdownlint-disable MD046 -->
#### Keep hooks in sync with CI

Run the same checks locally:

```bash
cargo fmt --check
cargo clippy
```

CI workflow equivalent:

```yaml
- run: cargo fmt --check
- run: cargo clippy
```
<!-- markdownlint-enable MD046 -->
`````

**Rule of thumb:** If content switches languages mid-block, add a closing fence
and open a new block with the correct tag.

### Pitfall 14: Bash Code Block Validation

Content tagged with `` ```bash `` may be validated as bash syntax. Only use the
`bash` fence tag for content that is actually valid shell script.

**Common mistakes:**

1. **Angle bracket placeholders are invalid bash** -- `<foo>` is parsed as
   redirection. Use `"$FOO"` (variable) or `your-foo` (literal) instead.

2. **Wrong fence tag for non-bash content** -- Error messages, Rust compiler
   output, Dockerfile instructions, AWK scripts, and YAML fragments are not
   bash. Use `text`, `rust`, `dockerfile`, `awk`, or `yaml` respectively.

3. **Empty if-blocks** -- A bash `if` or `else` branch with only a comment
   and no command is a syntax error. Use `:` (the colon no-op builtin) as a
   placeholder.

4. **AWK code with unmatched quotes** -- AWK snippets inside single-quoted
   bash strings can cause syntax errors when the AWK content contains
   unmatched quotes (e.g., `won't`). Either use a separate `awk` code block
   or escape carefully.

````markdown
❌ WRONG: Angle bracket placeholder in bash block
```bash
curl https://example.com/api/<your-token>
```

✅ CORRECT: Use a variable or literal placeholder
```bash
curl "https://example.com/api/${YOUR_TOKEN}"
```

❌ WRONG: Error output tagged as bash
```bash
error[E0308]: mismatched types
  --> src/main.rs:3:5
```

✅ CORRECT: Use text for non-bash output
```text
error[E0308]: mismatched types
  --> src/main.rs:3:5
```

❌ WRONG: Empty else branch (syntax error)
```bash
if [ -f "$file" ]; then
    process "$file"
else
    # nothing to do
fi
```

✅ CORRECT: Use colon no-op
```bash
if [ -f "$file" ]; then
    process "$file"
else
    : # nothing to do
fi
```
````

### Pitfall 15: MkDocs Material Tab Syntax vs MD046

MkDocs Material content tabs (`=== "Tab Name"`) require 4-space indented blocks
for the tab body. markdownlint MD046 (code-block-style: fenced) flags these as
indented code blocks. Wrap tabbed sections with
`<!-- markdownlint-disable MD046 -->` / `<!-- markdownlint-enable MD046 -->`.
Always re-enable after the section to avoid suppressing the rule for the rest
of the file.

````markdown
<!-- markdownlint-disable MD046 -->

=== "Docker"

    ```bash
    docker run -p 8080:8080 signal-fish-server
    ```

=== "Cargo"

    ```bash
    cargo run --release
    ```

<!-- markdownlint-enable MD046 -->
````

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

```jsonc
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
- [`github-actions-best-practices`](./github-actions-best-practices.md) - CI/CD integration for validation
- [ci-cd-troubleshooting](./ci-cd-troubleshooting.md) - Debugging link check and markdown lint failures
- [testing-strategies](./testing-strategies.md) - Data-driven tests for markdown validation
- [api-design-guidelines](./api-design-guidelines.md) - API documentation patterns

---

## Quick Reference

### Language Identifiers

- Rust: `rust`
- Shell: `bash` or `sh`
- Plain text: `text`
- JSON/YAML/TOML: `json`, `jsonc` (with comments), `yaml`, `toml`

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

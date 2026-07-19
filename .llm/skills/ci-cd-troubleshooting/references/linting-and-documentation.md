# CI/CD Troubleshooting - Linting & Documentation Patterns

See also: [CI CD Troubleshooting Ecosystem](ecosystem-and-toolchains.md),
[CI CD Troubleshooting Scripts](scripts-and-tests.md),
[CI CD Troubleshooting Links](link-checking.md),
[CI CD Troubleshooting Categories](diagnostic-workflow.md)

---

## TL;DR

- **Clippy check fails**: Run `cargo clippy --all-targets --all-features -- -D warnings` locally
- **Typos mixed-case**: `HashiCorp`, `GitHub`, `WebSocket` go in
  `[default.extend-identifiers]`, NOT `[default.extend-words]`
- **typos false positives in test data**: Add files with intentional "wrong" spellings to `[files] extend-exclude`
- **MD044 in URLs**: URL-stripping regex needs to cover all URL schemes; check `RAW_URL_STRIP_PATTERN`
- **YAML `...` in doc code blocks**: Replace with `# ...` (YAML comment) so validators can parse them

---

## Pattern 7: Clippy Lints in Test Code

### Symptom

```text
CI clippy step fails with:
error: this `if` statement can be collapsed
  --> src/room.rs:142:9
   = note: `-D clippy::collapsible-if` implied by `-D warnings`
```

### Root Cause

The CI clippy command uses `--all-targets`, which compiles and lints test code
(`#[cfg(test)]` modules and integration tests) in addition to production code.
Lints like `collapsible_if`, `needless_return`, and `single_match` are commonly
introduced in test code where developers focus on correctness rather than style.

### Solution

Run clippy with `--all-targets` locally before pushing:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

The `--all-targets` flag ensures test code, benchmarks, and examples are all
compiled and linted — matching what CI does.

---

## Pattern 8: MD044 Technical Terms in URLs

**Symptom:** `test_markdown_technical_terms_consistency` fails on a line containing
HTML tags or URLs with domain names like `github.io`, `docker.com`.

**Root Cause:** URLs contain domain names that are correctly lowercase. The test's
URL-stripping logic may not cover the URL scheme or format.

**Solution:**

<!-- markdownlint-disable MD044 -->

1. Verify the line contains a URL (not bare text like `"use github"`)
2. Check `RAW_URL_STRIP_PATTERN` in `tests/ci_config_tests.rs` covers the URL scheme
3. If a new scheme is needed, add it to the pattern: `(?:https?|wss?|ftp|newscheme)://\S+`
4. Add a test case to `test_technical_terms_url_stripping_skips_urls()`

**Not a bug if:** The term appears as bare text outside any URL (e.g., `"use github"`
should be `"use GitHub"`).

<!-- markdownlint-enable MD044 -->

---

## Pattern 9: Typos Configuration Issues (extend-words vs extend-identifiers)

### Symptom

```text
CI fails with:
ERROR: Typo found: HashiCorp (did you mean: Hashicorp?)
```

Even though you've added `hashicorp = "hashicorp"` to `.typos.toml`.

### Root Cause

**Mixed-case company names and proper nouns MUST use `[default.extend-identifiers]`,
not `[default.extend-words]`.**

1. **`[default.extend-words]`** - For lowercase technical terms (`tokio`, `axum`, `websocket`)
2. **`[default.extend-identifiers]`** - For mixed-case identifiers (`HashiCorp`, `WebSocket`, `CamelCase`)

- `extend-words` uses case-insensitive matching for lowercase terms
- `extend-identifiers` preserves exact case for mixed-case terms
- Typos uses CamelCase splitting: `HashiCorp` is treated as `Hash` + `I` + `Corp`

### Solution

```toml
# .typos.toml

[default.extend-words]
# Lowercase technical terms
axum = "axum"
tokio = "tokio"
websocket = "websocket"
rustc = "rustc"

[default.extend-identifiers]
# Mixed-case company names and proper nouns
HashiCorp = "HashiCorp"  # Company name (capital H, capital C)
GitHub = "GitHub"        # Company name (capital H)
WebSocket = "WebSocket"  # Protocol name (capital W, capital S)
```

### When to Use Each Section

| Term Type | Section | Example |
|-----------|---------|---------|
| Lowercase technical term | `extend-words` | `tokio`, `axum`, `rustc` |
| Lowercase abbreviation | `extend-words` | `async`, `impl`, `config` |
| Mixed-case company name | `extend-identifiers` | `HashiCorp`, `GitHub` |
| Mixed-case protocol | `extend-identifiers` | `WebSocket`, `WebRTC` |
| Code identifier | `extend-identifiers` | `params`, `consts`, `stdin` |

### Common Mixed-Case Terms That Need extend-identifiers

**Company names:** `HashiCorp`, `GitHub`, `GitLab`, `MongoDB`, `PostgreSQL`

**Protocol/Technology names:** `WebSocket`, `WebRTC`, `JavaScript`, `TypeScript`

### Complete .typos.toml Template

```toml
[default.extend-words]
# === Rust Crates ===
tokio = "tokio"
axum = "axum"

# === Build Tools ===
dockerfile = "dockerfile"
nightly = "nightly"

# === Technical Terms ===
websocket = "websocket"
async = "async"
msrv = "msrv"
cicd = "cicd"

[default.extend-identifiers]
# === Code Identifiers ===
params = "params"
consts = "consts"

# === Proper Nouns (Company Names) ===
HashiCorp = "HashiCorp"
GitHub = "GitHub"

# === Protocol Names ===
WebSocket = "WebSocket"
WebRTC = "WebRTC"
```

### Testing .typos.toml Configuration

```bash
typos                    # Check all files
typos path/to/file.md   # Check specific file
typos --write-changes   # Show what would be fixed
typos --dump-config     # Verify configuration is valid
```

### CI Test to Validate .typos.toml

```rust
// tests/ci_config_tests.rs

#[test]
fn test_typos_config_exists_and_is_valid() {
    let typos_config = repo_root().join(".typos.toml");
    assert!(typos_config.exists(), ".typos.toml is required for spell checking in CI");

    let content = read_file(&typos_config);
    assert!(content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section");
    assert!(content.contains("[default.extend-identifiers]"),
        ".typos.toml must have [default.extend-identifiers] section");
}
```

---

## Pattern 9b: Documentation Quality Issues (Markdown Linting)

### Symptom

```text
ERROR: MD040/fenced-code-language: Fenced code blocks should have a language specified
ERROR: typos found: HashiCorp (did you mean: Hashicorp?)
```

### Solutions

**A. Add language identifiers to all code blocks:**

```bash
# Find code blocks without language identifiers
grep -r '^```$' --include="*.md" .

# Auto-fix with markdownlint-cli2:
./scripts/check-markdown.sh fix
```

**B. Run markdown linting locally:**

```bash
npm install -g markdownlint-cli2@$(cat .markdownlint-version)
./scripts/check-markdown.sh  # Check ALL files, not just docs/
```

### Common Markdown Linting Rules

| Rule | Description | Fix |
|------|-------------|-----|
| **MD040** | Code blocks must have language identifiers | Add language after \`\`\` (bash, Rust, json, text) |
| **MD013** | Line length limit | Break long lines or disable rule |
| **MD041** | First line must be top-level heading | Add `# Title` as first line |
| **MD046** | Code block style | Use fenced code blocks (\`\`\`) consistently |

### Git Hook Boundary

Do not add markdownlint or typos checks to `.githooks/pre-commit`. Hooks are
last-resort staged-file guards and target <1 second. Run linting through agent
workflow, local CI, and GitHub CI instead.

---

## Related Skills

- [CI CD Troubleshooting Ecosystem](ecosystem-and-toolchains.md) — Language mismatch, cache, toolchain
- [CI CD Troubleshooting Scripts](scripts-and-tests.md) — Shell scripts, Miri, test filtering
- [CI CD Troubleshooting Links](link-checking.md) — Lychee, link checker patterns
- [CI CD Troubleshooting Categories](diagnostic-workflow.md) — Summary and diagnostic workflow

# Markdown Proper Nouns and Spell Checking

**Applies to**: When fixing capitalization of proper nouns, configuring the `typos` spell checker, or
dealing with technical term whitelisting.

See also:

- [Markdown Best Practices Code Blocks](./markdown-best-practices-code-blocks.md) — Code block language identifiers
- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — Linting rules and CI/CD integration
- [Markdown Best Practices Links](./markdown-best-practices-links.md) — Link validation

---

## TL;DR

**Proper Nouns:**

- Capitalize correctly: "HashiCorp", "GitHub", "WebSocket", "Rust"
- Technical identifiers (filenames, field names) stay lowercase in prose
- When in doubt, check the official brand guidelines

**Spell Checking:**

- Add technical terms to `.typos.toml`
- Lowercase terms go in `[default.extend-words]`
- Mixed-case terms go in `[default.extend-identifiers]`

---

## Proper Noun Capitalization

### The Challenge

Technical documentation contains a mix of:

1. **Proper nouns** (company names, product names) — require specific capitalization
2. **Technical identifiers** (filenames, field names, code patterns) — must match code exactly
3. **Common nouns** (general terms) — follow standard English rules

### Company and Product Names

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

**How to find official capitalization:** Check the company's official website, their GitHub
organization name, or their brand guidelines. When in doubt, use the capitalization from their logo.

### Technical Terms and Protocols

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

### File Names and Code Identifiers

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

**Rule:** Technical identifiers are not proper nouns — they must match code exactly, regardless of capitalization rules.

---

## Spell Checking Configuration

### The Tool: typos

The `typos` spell checker has two configuration sections with different purposes:

#### `[default.extend-words]` — Lowercase Technical Terms

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

#### `[default.extend-identifiers]` — Mixed-Case Terms

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

**If you put `HashiCorp` in `extend-words`, it won't work** because typos splits it into components.

### Adding New Technical Terms

1. CI fails with typo error → verify it's a legitimate technical term (not an actual typo)
2. Determine if it's lowercase or mixed-case
3. Add to appropriate section in `.typos.toml` and re-run validation

```text
# CI error: ERROR: Typo found: HashiCorp (did you mean: Hashicorp?)
# Fix: add to [default.extend-identifiers] in .typos.toml:
HashiCorp = "HashiCorp"
```

### Common Terms to Whitelist

```toml
[default.extend-words]
# Rust ecosystem
rustc = "rustc"
rustup = "rustup"
rustfmt = "rustfmt"
clippy = "clippy"
tokio = "tokio"
axum = "axum"
serde = "serde"
async = "async"
impl = "impl"
# Build / infra
dockerfile = "dockerfile"
yaml = "yaml"
toml = "toml"
json = "json"
cicd = "cicd"
# Networking
websocket = "websocket"
webrtc = "webrtc"

[default.extend-identifiers]
WebSocket = "WebSocket"
WebRTC = "WebRTC"
```

---

## Pitfall: Mixed-Case Terms in Wrong Section

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

---

## Pitfall: MD044 Proper Names vs Docker Image References

<!-- markdownlint-disable-next-line MD044 -->
MD044 flags lowercase `rust` as a proper noun violation, but Docker image names
like `rust:1.88` **must** stay lowercase. Wrap Docker image references in backtick
inline code (e.g., `` `rust:1.88` ``) to suppress MD044 inside code spans.

---

## Pitfall: MD044 and URLs in HTML Attributes

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

---

## VS Code Integration

Recommended extensions for `.vscode/extensions.json`: `davidanson.vscode-markdownlint`,
`streetsidesoftware.code-spell-checker`. Configure `.vscode/settings.json` to set
`"MD040": true` and disable `"MD013": false`, and add technical terms to `cSpell.words`.

---

## Quick Reference

### Capitalization

- Company names: Official capitalization (HashiCorp, GitHub)
- Technical terms: Match code exactly (`Cargo.toml`, `main.rs`)
- Protocols: Mixed case for proper noun (WebSocket), lowercase in technical context

### Spell Checking

- Lowercase technical terms: `[default.extend-words]`
- Mixed-case company names: `[default.extend-identifiers]`

---

## Related References

- [Markdown Best Practices Code Blocks](./markdown-best-practices-code-blocks.md) — Code block language identifiers
- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — Linting rules and CI/CD integration
- [Markdown Best Practices Links](./markdown-best-practices-links.md) — Link validation patterns

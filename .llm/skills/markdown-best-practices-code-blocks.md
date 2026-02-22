# Skill: Markdown Code Block Best Practices

<!--
  trigger: markdown, code blocks, language identifier, MD040, fenced code, json, jsonc, bash fence
  | Best practices for fenced code blocks in Markdown documentation
  | Documentation
-->

**Trigger**: When writing Markdown code blocks, fixing MD040 errors, or choosing the right language tag.

See also:

- [markdown-best-practices-linting](./markdown-best-practices-linting.md) — Linting rules and CI/CD integration
- [markdown-best-practices-links](./markdown-best-practices-links.md) — Link validation
- [markdown-best-practices-formatting](./markdown-best-practices-formatting.md) — Proper nouns and spell checking

---

## TL;DR

- Always specify language identifier: ` ```rust`, ` ```bash`, ` ```json`, never ` ```
- Use lowercase language names: ` ```rust` not ` ```Rust`
- For plain text examples, use ` ```text`
- JSON with comments must use ` ```jsonc`, not ` ```json`
- Only use ` ```bash` for actual valid shell script

---

## The Rule: Always Specify Language

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

---

## Common Language Identifiers

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

---

## Rust Code Block Attributes

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

**Attribute Formats:** comma-separated (` ```rust,ignore`), space-separated (` ```rust ignore`),
or multiple (` ```rust,ignore,no_run`). Both styles are valid.

---

## Case Sensitivity

Language identifiers should be lowercase for consistency. Both `rust` and `Rust` are valid
and work identically — use lowercase to avoid inconsistency.

---

## Pitfall: `json` vs `jsonc` Code Fence Tags

JSON with Comments (JSONC) uses `//` or `/* */` style comments. Standard JSON does
**not** allow comments. If a code block contains comments, use ` ```jsonc` instead
of ` ```json`. Using the wrong tag causes JSON validators to report syntax errors
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

| Content | Tag |
|---------|-----|
| Pure JSON (no comments) | `json` |
| JSON with `//` or `/* */` comments | `jsonc` |
| JSON with placeholder values like `[...]` | `jsonc` |

---

## Pitfall: Invalid Placeholders in JSON Code Blocks

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

---

## Pitfall: Bash Code Block Validation

Content tagged with ` ```bash` may be validated as bash syntax. Only use the
`bash` fence tag for content that is actually valid shell script.

**Common mistakes:**

1. **Angle bracket placeholders are invalid bash** — `<foo>` is parsed as
   redirection. Use `"$FOO"` (variable) or `your-foo` (literal) instead.

2. **Wrong fence tag for non-bash content** — Error messages, Rust compiler
   output, Dockerfile instructions, AWK scripts, and YAML fragments are not
   bash. Use `text`, `rust`, `dockerfile`, `awk`, or `yaml` respectively.

3. **Empty if-blocks** — A bash `if` or `else` branch with only a comment
   and no command is a syntax error. Use `:` (the colon no-op builtin) as a
   placeholder.

4. **AWK code with unmatched quotes** — AWK snippets inside single-quoted
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

---

## Pitfall: Mixed-Content Blocks Must Be Split

A single code block must contain only one language. When documentation shows a
sequence that spans multiple languages (e.g., shell commands that produce YAML
output, or a setup guide mixing bash and YAML), split the content into separate
fenced blocks with the correct tag for each.

**Rule of thumb:** If content switches languages mid-block, add a closing fence
and open a new block with the correct tag.

---

## Pitfall: MkDocs Material Tab Syntax vs MD046

MkDocs Material content tabs (`=== "Tab Name"`) require 4-space indented blocks
for the tab body. markdownlint MD046 (code-block-style: fenced) flags these as
indented code blocks. Wrap tabbed sections with
`<!-- markdownlint-disable MD046 -->` / `<!-- markdownlint-enable MD046 -->`.
Always re-enable after the section to avoid suppressing the rule for the rest
of the file.

---

## Pitfall: Code Block Fence Tracking in Nested Examples

Opening fences can have info strings (`` ```rust ``), but closing fences must be
bare (`` ``` ``). A naive toggle (flip `in_block` on every `` ``` `` line) breaks
when documentation contains nested fence examples. Always match closing fences with
an exact `/^```$/` pattern.

---

## Quick Reference

- Rust: `rust`
- Shell: `bash` or `sh`
- Plain text: `text`
- JSON/YAML/TOML: `json`, `jsonc` (with comments), `yaml`, `toml`

---

## Related Skills

- [markdown-best-practices-linting](./markdown-best-practices-linting.md) — MD040, MD041 rules and CI integration
- [markdown-best-practices-links](./markdown-best-practices-links.md) — Link validation patterns
- [markdown-best-practices-formatting](./markdown-best-practices-formatting.md) — Proper nouns, spell checking

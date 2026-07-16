# Markdown Code Block Best Practices

**Applies to**: When writing Markdown code blocks, fixing MD040 errors, or choosing the right language tag.

See also:

- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — Linting rules and CI/CD integration
- [Markdown Best Practices Links](./markdown-best-practices-links.md) — Link validation
- [Markdown Best Practices Formatting](./markdown-best-practices-formatting.md) — Proper nouns and spell checking
- [Markdown Best Practices Code Block Validation](./markdown-best-practices-code-block-validation.md)
  — Bash fence safety and mixed-language splitting

---

## TL;DR

- Always specify language identifier: ` ```rust`, ` ```bash`, ` ```json`, never ` ```
- Use lowercase language names: ` ```rust` not ` ```Rust`
- For plain text examples, use ` ```text`
- JSON with comments must use ` ```jsonc`, not ` ```json`
- Only use ` ```bash` for actual valid shell script
- For large/reusable examples, store canonical samples in `.agents/skills/*/references/` and link to them

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

## Pitfall: Overlong JSON Lines (MD013 in Code Fences)

Single-line JSON message examples can easily exceed markdownlint MD013
(`line_length`/`code_block_line_length`), and markdown auto-fix usually
cannot split these safely.

Prefer manual wrapping for nested objects:

````markdown
❌ WRONG: Single compact line (hard to keep under MD013 limits)
```json
{"type":"Authenticated","data":{"app_name":"game","org":"Acme","rate_limits":{"per_minute":60,"per_hour":3600}}}
```

✅ CORRECT: Wrap nested fields to keep each line <= 120
```json
{"type":"Authenticated","data":{"app_name":"my-game","organization":"Ambiguous Interactive",
"rate_limits":{"per_minute":60,"per_hour":3600,"per_day":86400}}}
```
````

---

## Pitfall: Bash + Mixed-Language Validation

Bash-fenced blocks are validated as shell syntax, and mixed-language content
must be split into separate fenced blocks. For full patterns and examples, see:
[Markdown Best Practices Code Block Validation](./markdown-best-practices-code-block-validation.md).

---

## Pattern: Canonical Sample Files for Reusable Examples

If the same example appears in multiple markdown files, or if the block is long
and drifts often, move it to `.agents/skills/*/references/` and reference it from docs.

Example references:

- `README.md` -> `.agents/skills/websocket-protocol/references/v2-client-messages.jsonl`
- `AGENTS.md` -> `code-samples/protocol/v2-client-messages.jsonl`

Benefits:

- One source of truth for shared examples
- Fewer markdownlint MD013/formatting regressions
- Easier consistency checks in scripts/tests

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

## Related References

- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — MD040, MD041 rules and CI integration
- [Markdown Best Practices Links](./markdown-best-practices-links.md) — Link validation patterns
- [Markdown Best Practices Formatting](./markdown-best-practices-formatting.md) — Proper nouns, spell checking
- [Markdown Best Practices Code Block Validation](./markdown-best-practices-code-block-validation.md)
  — Bash validation and mixed-content split patterns

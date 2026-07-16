# Markdown Code Block Validation Patterns

**Applies to**: When fixing markdown code-block validation failures where content and fence language do not match.

See also:

- [Markdown Best Practices Code Blocks](./markdown-best-practices-code-blocks.md) — Core fence language selection
- [Markdown Best Practices Linting](./markdown-best-practices-linting.md) — markdownlint + CI validation rules

---

## TL;DR

- Use `bash` only for syntactically valid shell script.
- Use `text` for logs/error output, not `bash`.
- Use `:` in intentionally empty bash branches.
- Split mixed-language examples into multiple fenced blocks.

---

## Bash Fence Validation

Markdown validation in this repository can execute bash syntax checks against
`bash` fenced blocks. If a fenced block is not valid shell script, the docs
check fails.

Use these language tags by content:

| Content | Fence |
|---------|-------|
| Shell commands/scripts | `bash` |
| Rust compiler output/logs/errors | `text` |
| AWK program source | `awk` |
| Dockerfile snippets | `dockerfile` |
| YAML fragments | `yaml` |

---

## Pitfall: Angle-Bracket Placeholders in Bash

Angle brackets are parsed as redirection in shell, so placeholders like
`<token>` break bash parsing.

````markdown
❌ WRONG
```bash
curl https://example.com/api/<your-token>
```

✅ CORRECT
```bash
curl "https://example.com/api/${YOUR_TOKEN}"
```
````

Use shell variables (`${TOKEN}`) or literal placeholders (`your-token`).

---

## Pitfall: Non-Shell Output Tagged as Bash

Compiler output and logs are not shell programs.

````markdown
❌ WRONG
```bash
error[E0308]: mismatched types
  --> src/main.rs:3:5
```

✅ CORRECT
```text
error[E0308]: mismatched types
  --> src/main.rs:3:5
```
````

---

## Pitfall: Empty Bash Branches

An `if`/`else` branch containing only a comment is invalid bash syntax.
Use `:` (no-op builtin) for intentionally empty branches.

````markdown
❌ WRONG
```bash
if [ -f "$file" ]; then
    process "$file"
else
    # nothing to do
fi
```

✅ CORRECT
```bash
if [ -f "$file" ]; then
    process "$file"
else
    : # nothing to do
fi
```
````

---

## Pitfall: AWK Embedded in Single-Quoted Bash Strings

AWK inside single-quoted shell strings fails when the AWK text contains
unescaped apostrophes (for example `won't`).

Preferred fix: use a dedicated `awk` fenced block for the AWK program, and keep
the shell invocation separate.

---

## Pitfall: Mixed-Language Blocks

A single fenced block must contain one language. If a sequence shows shell
commands and YAML output/config, split the sequence.

````markdown
❌ WRONG (mixed languages in one block)
```bash
cat <<'YAML' > config.yml
key: value
YAML
```

✅ CORRECT (split by language)
```bash
cat <<'YAML' > config.yml
key: value
YAML
```

```yaml
key: value
```
````

Rule of thumb: when syntax changes, close the current fence and open a new one
with the right tag.

---

## Related References

- [Markdown Best Practices Code Blocks](./markdown-best-practices-code-blocks.md)
- [Markdown Best Practices Linting](./markdown-best-practices-linting.md)

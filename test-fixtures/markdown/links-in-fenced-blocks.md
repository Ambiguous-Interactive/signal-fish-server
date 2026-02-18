# Link Checker Test: Links Inside Code Fences

This file tests that the link checker correctly skips
links inside fenced code blocks and inline code spans.

## Normal link (should be checked)

See the [README](../../README.md) for details.

## Link inside 3-backtick fence (should be skipped)

```markdown
See [nonexistent](../no-such-file.md) for details.
```

## Link inside 4-backtick fence (should be skipped)

````markdown
Check [another file](../also-missing.md) for info.
```json
{"link": "[nested](../deep-missing.md)"}
```
````

## Link inside inline code (should be skipped)

Use relative paths for internal docs: `[guide](../docs/guide.md)`

## Another normal link (should be checked)

See the [CHANGELOG](../../CHANGELOG.md) for history.

Expected: Only 2 links should be checked (README.md, CHANGELOG.md).
Links inside fences and inline code should be skipped.

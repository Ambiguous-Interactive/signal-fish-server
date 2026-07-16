---
name: shell-scripting
description: Write, review, and debug portable repository shell automation. Use for Bash, POSIX shell, AWK, quoting, pipelines, traps, strict mode, NUL-delimited data, cross-platform text processing, or ShellCheck failures outside GitHub Actions-specific workflow design.
---

<!-- markdownlint-disable MD013 -->

# Shell Scripting

Prefer simple, observable scripts with explicit input contracts. Test hostile filenames, empty input, pipeline failures, and platform-sensitive syntax.

- Read [shell-scripting-patterns.md](references/shell-scripting-patterns.md) for Bash structure, quoting, cleanup, and failure behavior.
- Read [awk-text-processing.md](references/awk-text-processing.md) for portable AWK and record-processing patterns.
- Invoke `$ci-troubleshooting` when the script runs inside GitHub Actions.
- Add fixture-driven tests for parsing or rewriting logic instead of validating only the happy path.

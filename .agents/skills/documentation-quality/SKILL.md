---
name: documentation-quality
description: Create, update, review, and validate Signal Fish documentation and changelog content. Use for Markdown, README, rustdoc, protocol examples, links, code fences, spelling, formatting, generated documentation, version synchronization, or deciding and writing user-visible changelog entries.
---

<!-- markdownlint-disable MD013 -->

# Documentation Quality

Keep documentation executable where possible: link to canonical samples, validate claims against source, and add drift guards for facts that can be checked mechanically.

## Route the task

- Read [documentation-standards.md](references/documentation-standards.md) and [doc-accuracy-guarantees.md](references/doc-accuracy-guarantees.md) for general documentation work.
- Read [project-docs-and-ci-pitfalls.md](references/project-docs-and-ci-pitfalls.md) for repository-specific pitfalls.
- Read the focused Markdown reference for [code blocks](references/markdown-best-practices-code-blocks.md), [code-block validation](references/markdown-best-practices-code-block-validation.md), [formatting and spelling](references/markdown-best-practices-formatting.md), [links](references/markdown-best-practices-links.md), or [lint integration](references/markdown-best-practices-linting.md).
- Read [classify-user-visible-changes.md](references/classify-user-visible-changes.md), then [update-changelog-keep-a-changelog.md](references/update-changelog-keep-a-changelog.md), when a change may require release notes.
- Read [review-changelog-entries.md](references/review-changelog-entries.md) for changelog review.
- Read [version-sync-and-changelog-gates.md](references/version-sync-and-changelog-gates.md) for version propagation and gates.

Run the focused documentation checks and link validation after editing.

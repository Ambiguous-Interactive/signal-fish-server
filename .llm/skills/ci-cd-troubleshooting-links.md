# Skill: CI/CD Troubleshooting - Link Checker & Lychee Patterns

<!--
  trigger: lychee failure, link checker, broken links, lychee regex, lychee config,
  lychee toml, exclude_path, lychee version bug, TOML before after example,
  cargo deny cvss
  | Patterns 10-20: Link checker failures, lychee configuration, regex pitfalls,
  version-specific bugs, TOML validation, cargo-deny CVSS | Infrastructure
-->

**Trigger**: When debugging link checker failures (lychee), `.lychee.toml` regex vs glob
confusion, lychee version-specific bugs, TOML before/after validation failures, or
cargo-deny CVSS parsing errors.

See also: [ci-cd-troubleshooting-ecosystem.md](./ci-cd-troubleshooting-ecosystem.md),
[ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md),
[ci-cd-troubleshooting-supply-chain.md](./ci-cd-troubleshooting-supply-chain.md),
[ci-cd-troubleshooting-categories.md](./ci-cd-troubleshooting-categories.md)

---

## TL;DR

- **Lychee regex vs glob**: `.lychee.toml` `exclude` field takes **regex**, not glob — escape
  `.` as `\\.`, use `.*` not `*`
- **Lychee scans itself**: Use `--exclude-path .lychee.toml` CLI flag; TOML `exclude_path` has bugs
- **`exclude_path` TOML bug**: Does not apply to glob-expanded files — always use CLI `--exclude-path` flags
- **TOML before/after examples**: Single block with duplicate table headers is invalid TOML — split into two blocks
- **cargo-deny CVSS 4.0**: Update to `EmbarkStudios/cargo-deny-action@... # v2.0.15`

---

## Pattern 10: Link Check Failures (lychee)

### Root Causes and Solutions

**A. Placeholder URLs in test fixtures:**

```toml
# .lychee.toml — exclude placeholder URL patterns (NOT globs — see Pattern 13)
exclude = [
    "^https://github\\.com/owner/repo/.*",
    "^https://example\\.com/.*",
    "^http://localhost",
]
```

**Why exclude by pattern, not file path:** Allows placeholder URLs in test fixtures
without excluding the entire file. Other links in the same file are still validated.

**B. Case sensitivity (Linux vs macOS/Windows):**

```bash
# Find actual filename case
find . -name "testing-core-patterns.md" -type f
# Fix link to match exactly
sed -i 's|Skills/testing-core-patterns.md|skills/testing-core-patterns.md|g' docs/*.md
```

**C/D. Fix or remove broken external links; fix relative path case** (`Skills/` vs `skills/`
on Linux). When a link is temporarily unavailable, add it to `.lychee.toml` `exclude`
temporarily and re-enable when the site returns.

### When to Exclude vs Fix

| Scenario | Action |
|----------|--------|
| Placeholder URL in test fixture | Exclude by pattern |
| Broken external link | Fix or replace |
| Temporarily unavailable site | Exclude temporarily |
| localhost/example.com URLs | Exclude permanently |
| Case mismatch | Fix link case |

---

## Pattern 11: cargo-deny CVSS 4.0 Parsing Issue

### Symptom

```text
Error: CVSS v4.0 vectors are not supported by this version
ERROR: cargo-deny-action v2.0.5 cannot parse CVSS 4.0 entries
```

### Root Cause

cargo-deny-action versions prior to v2.0.15 cannot parse CVSS 4.0 entries in the
RustSec advisory database.

### Solution

```yaml
# WRONG: Old version cannot parse CVSS 4.0
- uses: EmbarkStudios/cargo-deny-action@f20e90f289e90a40fd814d92ea2935d9db5da04f # v2.0.5

# CORRECT: v2.0.15+ includes rustsec 0.31 with CVSS 4.0 support
- uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14 # v2.0.15
  with:
    arguments: --all-features
```

### Related: cargo-deny Docker Container Toolchain Mismatch

```text
error: toolchain '1.88.0' is not installed
```

The action's Docker image has its own Rust toolchain, but `rust-toolchain.toml`
can force a different version inside the container.

```yaml
- name: Extract MSRV
  id: deny-msrv
  run: |
    MSRV=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "(.+)"/\1/')
    echo "version=$MSRV" >> "$GITHUB_OUTPUT"

- name: Run cargo-deny
  uses: EmbarkStudios/cargo-deny-action@<SHA> # v2.0.15
  with:
    arguments: --all-features
    rust-version: ${{ steps.deny-msrv.outputs.version }}
```

This is safe because cargo-deny inspects metadata and `Cargo.lock` — it does not
compile code, so the exact Rust version is irrelevant.

### Scheduled Security Audits

Add a daily `schedule: cron: '0 12 * * *'` to the dependency audit workflow to catch
new CVEs between commits, not just on push/PR.

---

## Pattern 13: Lychee Config Regex Pitfalls

### Symptom

```text
ERROR: regex parse error: \.github/test-fixtures/{bad-link}.md
ERROR: repetition quantifier expects a valid decimal
```

### Root Cause

**The `exclude` field in `.lychee.toml` takes regular expressions, NOT glob patterns.**

**Key metacharacters that need escaping:**

| Character | Glob meaning | Fix |
|-----------|-------------|-----|
| `.` | Literal dot | Escape: `\\.` |
| `{}` | Brace expansion | Escape: `\\{\\}` |
| `*` | Wildcard | Use `.*` for wildcard |
| `?` | Single char | Escape: `\\?` |
| `+` | Literal | Escape: `\\+` |

### Solution

```toml
# .lychee.toml — CORRECT: regex syntax with escaped metacharacters
exclude = [
    "^https://example\\.com/",          # Escaped dot
    "^https://github\\.com/owner/.*",   # Use .* not *
    "^https://github\\.com/%7B%7B%7D/.*",  # Escaped braces
    "^http://localhost",
    "^https?://example\\.",             # RFC 2606 reserved domain
]
```

### Regex Review Checklist for `.lychee.toml`

- [ ] Every `.` in a domain name or file extension is escaped as `\\.`
- [ ] Patterns use `^` anchors to avoid unintended substring matches
- [ ] Globs like `*` are replaced with regex `.*`
- [ ] Literal braces `{}` are escaped as `\\{\\}`
- [ ] Each exclusion has a comment explaining why it exists
- [ ] Test locally: `lychee --config .lychee.toml './**/*.md'`

---

## Pattern 17: Lychee Scans Its Own Config File

### Symptom

```text
✗ [404] https://lib/ | .lychee.toml:8:5
✗ [404] https://crates/ | .lychee.toml:12:5
```

### Root Cause

Lychee scans `*.toml` files and extracts partial URLs from regex patterns inside
`.lychee.toml` itself. Pattern `^https://lib\\.rs` causes lychee to extract and
check `https://lib/`.

### Solution

```toml
# Option A (recommended): Exclude via CLI flag
# lychee --exclude-path .lychee.toml ...

# Option B: Also exclude the partial URLs lychee extracts from its own patterns
exclude = [
    "^https://lib\\.rs",
    "^https://crates\\.io",
    # Self-referential: partial URLs extracted from patterns above
    "^https://lib/$",
    "^https://crates/$",
]
```

---

## Pattern 18: Config Test Assertions vs Regex Patterns

### Symptom

```text
assertion failed: content.contains("http://localhost")
  .lychee.toml should exclude localhost URLs
```

But `.lychee.toml` does exclude localhost — via `^https?://localhost`.

### Root Cause

Test assertions use substring matching (`contains()`) against config files that
contain regex patterns. The regex `^https?://localhost` does not contain the
substring `http://localhost`.

### Solution

Use `contains("localhost")` (matches the regex string), or compile the regex and
test its behavior:

```rust
// WRONG: substring "http://localhost" not in regex "^https?://localhost"
assert!(content.contains("http://localhost"));

// CORRECT: test the behavior the regex produces
let re = regex::Regex::new(r"^https?://localhost").unwrap();
assert!(re.is_match("http://localhost:8080"));
```

---

## Pattern 19: Lychee Version-Specific Bugs

**Bug A: Hidden File Matcher (lychee v0.21.0, #1936)** — Lychee scans dotfiles
even when disabled. **Fix:** Pin `lycheeVersion: v0.22.0` in the action config.

**Bug B: `exclude_path` TOML entries ignored for glob-expanded files** — Paths in
`.lychee.toml` `exclude_path` are not applied when lychee receives files via glob
expansion. **Fix:** Always use CLI `--exclude-path` flags instead; separate from
glob args with `--`:

```yaml
- name: Link Checker
  uses: lycheeverse/lychee-action@<SHA>
  with:
    args: >-
      --verbose --no-progress
      --exclude-path .lychee.toml
      --exclude-path target
      --exclude-path .github/test-fixtures
      --
      './**/*.md' './**/*.toml'
```

Never rely solely on `.lychee.toml` `exclude_path` — always duplicate critical
exclusions as CLI `--exclude-path` flags.

---

## Pattern 20: TOML Validation Fails on Before/After Example Blocks

### Symptom

```text
ERROR: TOML parse error in docs/migration.md
  duplicate key `dependencies` at line 12
```

### Root Cause

Documentation showing "before/after" TOML examples in a **single** fenced code block
creates invalid TOML because the block contains duplicate table headers.

### Solution

Split into separate fenced code blocks, each containing valid TOML (one "before"
block, one "after" block). Every `toml`-, `json`-, and `yaml`-tagged block is parsed
by the corresponding CI validator, so each block must independently pass parsing.

---

## Related Skills

- [ci-cd-troubleshooting-ecosystem.md](./ci-cd-troubleshooting-ecosystem.md) — Language mismatch, cache, toolchain
- [ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md) — Shell scripts, Miri, test filtering
- [ci-cd-troubleshooting-supply-chain.md](./ci-cd-troubleshooting-supply-chain.md) —
  SHA pinning, Dockerfile, stale scripts
- [ci-cd-troubleshooting-categories.md](./ci-cd-troubleshooting-categories.md) — Diagnostic workflow, quick reference
- [supply-chain-security](./supply-chain-audit-policy.md) — Security audits and vulnerability scanning

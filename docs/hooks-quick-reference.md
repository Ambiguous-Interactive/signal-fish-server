# Pre-Commit Hooks - Quick Reference

## Installation

```bash
./scripts/enable-hooks.sh
```

## What Runs on Commit

| Check | When | Fix Command |
|-------|------|-------------|
| 1. Formatting | Always | `cargo fmt` |
| 2. Clippy | When `.rs` staged | `cargo clippy --fix --allow-dirty --all-features` |
| 3. Panics | Always | Remove `.unwrap()`, use `?` instead |
| 4. MSRV | When config staged | `./scripts/check-msrv-consistency.sh` |
| 5. AWK | When workflows staged | `./scripts/validate-workflow-awk.sh` |
| 6. Markdown | When `.md` staged | `./scripts/check-markdown.sh fix` |
| 7. Links | When `.md` staged | Warning only |

## Common Fixes

### Formatting Error
```bash
cargo fmt
```

### Clippy Warnings
```bash
# Auto-fix
cargo clippy --fix --allow-dirty --all-features

# Or manually check
cargo clippy --all-features
```

### MSRV Mismatch
```bash
# Check consistency
./scripts/check-msrv-consistency.sh

# Update all files to match Cargo.toml
# Edit: rust-toolchain.toml, clippy.toml, Dockerfile
```

### Markdown Issues
```bash
./scripts/check-markdown.sh fix
```

## Run All CI Checks Locally

```bash
# Full CI (includes tests)
./scripts/run-local-ci.sh

# Fast mode (no tests)
./scripts/run-local-ci.sh --fast

# Auto-fix mode
./scripts/run-local-ci.sh --fix
```

## Bypass Hooks (Rare)

```bash
git commit --no-verify
```

**⚠️ Only use for:**
- WIP commits on feature branch
- Emergency hotfixes
- When hook incorrectly flags valid code

**Then run before merging:**
```bash
./scripts/run-local-ci.sh
```

## Common Issues

### Hook Doesn't Run
```bash
# Re-enable hooks
./scripts/enable-hooks.sh

# Verify
git config --local core.hooksPath
# Should output: .githooks
```

### Hook Too Slow
```bash
# Commit smaller changesets
git add src/specific_file.rs
git commit -m "Part 1"

# Or use --fast mode in local CI
./scripts/run-local-ci.sh --fast
```

### Clippy Fails But Not Locally
```bash
# Ensure same Rust version as CI
rustc --version
cat rust-toolchain.toml

# Update dependencies
cargo update
```

## Help

Full guide: `docs/git-hooks-guide.md`

Troubleshooting: `docs/git-hooks-guide.md#troubleshooting`

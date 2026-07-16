---
name: toolchain-management
description: Manage Signal Fish Rust toolchain pins and minimum supported Rust version. Use for rust-toolchain.toml, rust-version, MSRV tests, nightly-only CI tools, pinned nightly dates, Cargo compatibility, or synchronizing tool versions across CI and devcontainers.
---

<!-- markdownlint-disable MD013 -->

# Toolchain Management

Treat MSRV and nightly pins as repository-wide contracts. Update every source of truth and add or update drift tests in the same change.

1. Read [msrv-management.md](references/msrv-management.md) for MSRV and dependency compatibility.
2. Read [toolchain-pinning.md](references/toolchain-pinning.md) for stable toolchain pins and MSRV testing.
3. Read [toolchain-nightly.md](references/toolchain-nightly.md) for nightly-only tools and date updates.
4. Load [toolchain-nightly-example-update-script.md](references/toolchain-nightly-example-update-script.md) when implementing a pin-update script.
5. Run `bash scripts/check-tooling-parity.sh` after tooling changes, plus focused configuration tests and the mandatory Rust checks.

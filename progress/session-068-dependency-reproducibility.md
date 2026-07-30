# Session 068 — Standalone dependency reproducibility

**Branch:** `agent/session-068-dependency-reproducibility`
**Base:** `17f4ad0` (PR #227, session 067)

## Objective

Complete issue #225 and restore the red scheduled dependency update to a
reproducible, inventory-driven state.

## Starting evidence

- No open or draft pull request existed.
- The `main` push for PR #227 had all completed workflows green while its main
  CI workflow was still running.
- Dependabot run `30573261244` failed before resolution with
  `dependency_file_not_found`: `/third_party/rmp/Cargo.toml` did not exist.
- The repository tracked five Cargo packages, but Dependabot monitored only the
  root and the removed vendored path.
- `fuzz/Cargo.lock` existed only as an ignored local artifact; clean CI
  resolved that graph afresh and stable quick-check omitted `--locked`.

## Implementation

- Replace the dead `/third_party/rmp` Dependabot entry with standalone entries
  for `/clients/native` and `/fuzz`.
- Add a live-YAML, git-inventory guard requiring Dependabot coverage for every
  tracked package except the native and Godot/WASM Fortress exact-release
  fixtures. The test rejects stale, missing, duplicate, and noncanonical Cargo
  directories.
- Remove real stale vendoring claims from `README.md`, `.gitignore`, and the
  dependency-management skill while preserving synthetic regression fixtures.
- Commit a freshly resolved fuzz lockfile and enforce it through:
  - stable Rust 1.89 `cargo check --locked` for every fuzz target;
  - pinned-nightly `cargo metadata --locked` before cargo-fuzz;
  - nested-lock/current-server-version policy;
  - root cargo-deny and cargo-audit CI jobs scoped to the fuzz manifest/lock.
- Declare the fuzz package's MIT license, Rust 1.89 floor, and server path
  dependency version. Add the OSI-approved NCSA license required by
  `libfuzzer-sys` to the shared allowlist.
- Extend the MSRV consistency script and data-driven fixtures to cover the fuzz
  package as a required standalone manifest.

## Dependency evidence

- A Rust 1.89 compatible refresh advanced 69 entries in the new fuzz lockfile;
  the final SHA-256 is
  `58fe9aa94c4832af19a50f388076ec516f5f1f2215b673540b2c13c8aecdf656`.
- Rust 1.89 locked metadata and all-target compilation pass.
- Pinned-nightly locked metadata and an instrumented sanitizer build pass.
- Full cargo-deny policy passes for root, native, and fuzz graphs. The local
  environment does not install cargo-audit; CI installs it and now scans both
  the root and fuzz locks.
- Dry-run refreshes found no fuzz-specific hold. Root retains measured holds
  for base64 0.23, tokio-tungstenite 0.30, serial_test 4, and syn 3; native
  retains the applicable WebSocket and serial_test holds.
- The two Fortress packages remain intentional exact-release fixture
  exclusions with committed locks and dedicated cargo-deny/interop workflows.

## Changelog classification

No server runtime, protocol, API, configuration, or performance behavior
changes. The repository changelog gate nevertheless classifies the standalone
manifests/lock and README as non-internal, so the Unreleased `Changed` section
records the contributor-visible dependency reproducibility and security-policy
outcome.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-features`
- Full CI-config, workspace-lockfile, and MSRV consistency suites
- Rust 1.89 locked native and fuzz compilation
- Pinned-nightly locked fuzz metadata and sanitizer build
- Root, native, and fuzz cargo-deny advisory/license/ban/source policy
- Workflow hygiene, CI-config, tooling-parity, skill-layout, markdown, and LLM
  repository policy checks

## Adversarial review

Three independent read-only audits covered the Cargo-package inventory, fuzz
tool compatibility, and the complete patch. Their findings were resolved by:

- filtering dynamic inventory to real `[package]` manifests and rejecting
  duplicate, noncanonical, or undocumented exclusions;
- asserting the nightly locked preflight runs before cargo-fuzz;
- accepting legitimate automated cargo-deny-action version updates while
  retaining the repository's independent minimum-version gate;
- including fuzz manifest/lock changes in the dependency-only changelog
  classifier so automated refresh PRs stay green;
- extending the MSRV guard to the standalone fuzz package.

The final audit found no remaining implementation issue. The session record is
force-added in accordance with prior tracked session records; the intentionally
local and historically untracked `PLAN.md` remains ignored.

Hosted CI and reviewer evidence follow before the session is complete.

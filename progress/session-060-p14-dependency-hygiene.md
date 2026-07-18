# Session 060 — P14 dependency hygiene

## Trigger

Dependabot PR #179 was the repository's only live maintenance work, but its
base was ten commits behind `main` and its `tokio-tungstenite` 0.29→0.30 bump
would create two WebSocket stacks. Axum 0.8.9 still resolves
`tokio-tungstenite` and `tungstenite` 0.29, while 0.30 would be used only by
the direct test client.

## Change

- Refreshed Tokio 1.52.3→1.52.4, UUID 1.23.4→1.24.0, Clap 4.6.1→4.6.2,
  Rustls 0.23.41→0.23.42, Regex 1.13.0→1.13.1, Syn 2.0.118→2.0.119,
  Trybuild 1.0.117→1.0.118, and Saphyr 0.0.9→0.0.11.
- Removed `tokio-tungstenite` from normal dependencies because production code
  uses Axum's WebSocket surface; the direct raw client is test-only and remains
  a dev dependency.
- Deliberately retained `tokio-tungstenite` 0.29 so the server and tests share
  one Tungstenite implementation. The 0.30 upgrade remains deferred until the
  server framework can move in lockstep.
- The refreshed resolver also removed the otherwise-unused `windows-sys` 0.60
  family; Quinn's compatible Windows target now shares the existing 0.52
  family instead of retaining a third Windows bindings generation.
- The required full local supply-chain audit exposed a same-class tooling bug:
  `check-advisories.sh` placed cargo-deny's global `--all-features` option after
  the `check` subcommand, which cargo-deny 0.20.2 rejects. The invocation is now
  ordered correctly and the CI policy suite rejects the invalid form.

## Verification

- `cargo check --locked --all-features`
- targeted Clippy for `ci_config_tests` with warnings denied
- data-driven Saphyr validator behavior plus every live workflow parsed
- exact server-Ping/Pong WebSocket regression
- cargo metadata MSRV audit: all eight refreshed crates declare Rust 1.85 or
  lower against the project's Rust 1.89 floor
- `cargo tree`: one `tokio-tungstenite`/`tungstenite` version, with the root's
  direct dependency classified `dev`
- `scripts/check-advisories.sh --full`: no RustSec advisories; bans, licenses,
  and sources all pass
- ShellCheck, Bash syntax, CI config, documentation consistency, and workflow
  hygiene

## CI follow-up

The first exact-head WebRTC Interop run failed at its fast lockfile precheck,
before compilation: removing the root package's normal `tokio-tungstenite`
dependency changed the path package metadata recorded in
`clients/native/Cargo.lock`. Regenerating that fixture lock removed the stale
edge. A repository-wide `cargo metadata --locked --no-deps` sweep then passed
for the root, native, Fortress native, Fortress WASM, and fuzz manifests.

The implementation head `93971e637a63db1e63ec0afac98593cbe078a7c1`
then passed all 12 applicable pull-request workflows; the only non-success was
the intentional Dependabot auto-merge skip. Verification Nightly's first
attempt exposed a stochastic 1%-loss WebRTC N=8 mesh miss: 27 of 28 peer links
formed, one SCTP INIT/ACK exchange did not recover, and the test failed loudly
at its 360-second deadline. The isolated job retry passed without a source
change, alongside green clean/loss matrices, WebRTC/Browser/Fortress interop,
cross-platform nextest and lint, coverage, MSRV, Miri, AddressSanitizer, audit,
SBOM, and documentation checks.

Cursor Bugbot found no new issues on the exact implementation head. Copilot
was explicitly requested after each push but reported that the requester quota
was exhausted. No inline review threads were opened. PR:
<https://github.com/Ambiguous-Interactive/signal-fish-server/pull/191>.

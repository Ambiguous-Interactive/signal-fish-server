# Browser Reference Client on Real Headless Chromium

## Status

ADR-0005 - Accepted

## Context

[ADR-0004](0004-native-reference-client.md) closed the "real WebRTC stack" gap with the native Rust reference
client (webrtc-rs), proving native↔native sessions end to end. But the primary production consumer of a
browser-facing signaling server is a **browser**: `RTCPeerConnection` as shipped in Chromium, with behaviors no
native stack reproduces — mDNS host-candidate obfuscation (`.local` hostnames), browser SDP dialects, the
`ondatachannel` adoption path, page-lifecycle teardown. Full coverage requires browser↔native interop
matrix cells, including the mDNS `.local` trap. Until this ADR, the `clients/README.md` browser row was
"planned" and every browser cell in the matrix was pending.

The structural constraints mirror ADR-0004's: nothing may destabilize the server crate or the always-on native
interop suite; the conformance evidence must be multi-process, machine-checked, and loopback-only in CI; and the
client must follow the exact JSONL/exit-code contract the existing Rust harness asserts on.

## Decision

Build an **in-repo TypeScript browser reference client** (`clients/browser/`, npm package
`signal-fish-reference-browser`) that drives a **real Chromium `RTCPeerConnection`** (the
`chromium-headless-shell` build, launched via **playwright-core**), plus **feature-gated browser↔native interop
cells in the native crate's test harness** (`clients/native/tests/browser_interop_e2e.rs`, cargo feature
`browser-interop`), run by `scripts/run-browser-interop.sh` and `.github/workflows/browser-interop.yml`.

### A real browser, not a Node/wasm WebRTC stack

The point of this client is the _browser_ behavior. Alternatives that run "WebRTC in Node" (werift,
node-datachannel) or compile Rust to wasm (`matchbox_socket` + `web-sys`, which still needs a browser to host
it) would test someone else's ICE/DTLS/SCTP implementation, not the one games actually meet. Headless-shell
Chromium is the smallest distribution of the real engine, and a loopback probe proved RTCDataChannel handshakes
complete inside it in this repo's CI containers.

### Page/CLI split

The package builds two esbuild bundles from one strict-TypeScript source tree:

- **`dist/page.js`** (IIFE, injected into the page): WebSocket wire + the v3 protocol state machine + the
  `RTCPeerConnection` engine — a faithful port of the native client's orchestrator (same ready-barrier gating,
  server-owned initiator roles, Appendix G one-shot transport status, success criteria, and event ordering via a
  single serialized input chain).
- **`dist/cli.js`** (Node ESM, the process entrypoint): argv parsing (the native client's flag surface plus
  `--mdns-obfuscation`), Chromium launch, `page.exposeFunction` stdout bridge (pure JSONL; Chromium and
  Playwright noise stays on stderr), the `--max-runtime-secs` watchdog, and exit-code mapping identical to the
  native client (0/1/2/3/4 — local infrastructure failures such as a Chromium launch error map to 4, mirroring
  the native client's runtime-start failure path).

The JSONL event contract is byte-compatible with the native client's
([`clients/native/README.md`](../../clients/native/README.md) is canonical), which is what lets one Rust harness
assert over native and browser processes interchangeably.

### Chromium teardown as a first-class requirement

The harness kills clients with SIGKILL on drop, and headless Chromium **does not** exit just because its parent
died (verified empirically). The CLI therefore (1) tears the browser down on every catchable exit path
(normal exit, watchdog, SIGTERM/SIGINT/SIGHUP, uncaught exception) via a bounded `BrowserServer.close()`
escalating to `kill()`, and (2) launches via `launchServer` to obtain the Chromium pid and spawns a tiny
detached **reaper** process that SIGKILLs Chromium if the CLI dies unannounced (guarded against pid reuse by
re-checking `/proc/<pid>/stat` starttime before killing). Both paths are pinned by the automated
`browser_cli_signal_teardown_reaps_chromium` cell — SIGTERM must tear Chromium down and exit 143; a SIGKILLed
CLI's Chromium must be reaped within a bounded window — so CI leaks no Chromium processes even from
panicking tests.

### mDNS posture (the `.local` trap)

By default the CLI launches Chromium with `--disable-features=WebRtcHideLocalIpsWithMdns`, so loopback interop
uses plain host candidates. The `--mdns-obfuscation` flag leaves Chromium's default obfuscation ON: candidates
become opaque `<uuid>.local` names that the native side cannot resolve (CI runs no mDNS responder).
**Empirically pinned outcome** (and the assertion of the `mesh_n3_browser_mdns_obfuscation` cell): P2P still
establishes — the browser learns the native side's real host candidates from the relayed signals and initiates
ICE connectivity checks; the native agent answers and adopts the browser's transport address as a
peer-reflexive candidate. webrtc-rs accepts the `.local` candidate without erroring; every pair connects, all
members report `{webrtc, connected: true}`, and the fallback never engages. The cell does NOT depend on real
multicast mDNS resolution.

### Feature-gated cells in the native crate, not a third test crate

The proven harness (server spawn, JSONL readers, deadline-bounded assertions, scenario windows) lives in
`clients/native/tests/harness/`. The browser cells reuse it through additive harness support only — a
browser-client spawn path (`node <bundle>`, located via the environment variable documented in the client
README, beside the server-binary one) — behind the
`browser-interop` cargo feature with a `required-features` test target, so the default native suite never
compiles, links, or requires Node/Chromium. The feature-gated target adds lint surface that only the
browser-interop pipeline exercises: `scripts/run-browser-interop.sh` (and the workflow that runs it) gates on
`cargo fmt --check` plus `cargo clippy --locked --all-targets --features browser-interop -- -D warnings` —
still far cheaper than duplicating the harness in a third crate. Scenario-level
assertion helpers are deliberately copied, not shared, so the always-on native suite never needs edits for
browser-cell churn.

### Zero-external-network CI posture

Unchanged from ADR-0004: TURN disabled, zero STUN URLs, `ice_servers_count == 0` pinned, loopback host
candidates only. The single network fetch is the Chromium headless-shell download at install time — performed by
the lockfile-pinned `node_modules/.bin/playwright-core install` (never bare `npx`) and cached in CI keyed on the
playwright-core version.

### Dependency policy

Runtime dependency: `playwright-core` only (no full `playwright`, no test runner, no browser auto-download on
install). Dev dependencies: `typescript`, `esbuild`, `prettier`, `@types/node`. No eslint: the surface is two
small bundles enforced by `tsc --strict`, prettier, and a runner-script grep that forbids `console.log` (stdout
purity); an eslint toolchain would outweigh the code it lints.

## Consequences

### Positive

- **The browser matrix cells are real and CI-enforced:** mixed mesh, browser↔browser, host star with a browser
  client, crippled-ICE browser fallback, the mDNS `.local` trap, a pure-v2 browser flooring a room, the
  mid-handshake-close error contract (exactly one `error`, prompt exit 3), and SIGTERM/SIGKILL Chromium
  teardown reaping.
- **One harness, two client kinds:** identical JSONL/exit-code contracts make every existing assertion helper
  work unchanged across native and browser processes.
- **The `.local` behavior is pinned**, not assumed — the documented peer-reflexive outcome is an executable fact.
- **No new always-on cost:** the default native suite and the server crate's gates are untouched.

### Negative

- **A Node toolchain enters the repo** (one more lockfile and audit surface, `clients/browser/package-lock.json`).
- **The browser cells are slower per scenario** (Chromium launches) — bounded by serialization and the same
  watchdog discipline as the native suite.
- **The page engine hand-models the wire envelope** (TypeScript has no path dependency on the server crate);
  drift is caught at runtime by the interop suite rather than at compile time.

### Mitigations

- The workflow path-filters on `clients/**` plus all server sources and root manifests, so changes to either
  client or the server re-run the browser cells.
- `clients/browser/README.md` documents the CLI, contract deviations (`--mdns-obfuscation`, the run-window
  measurement, exit-4 launch failures), and the cells it adds; the native README stays the canonical contract.

## Alternatives Considered

### 1. Node WebRTC stacks (werift, node-datachannel) or a wasm build

**Rejected:** they validate a different WebRTC implementation. The browser-specific behaviors this client
exists to exercise (mDNS obfuscation, Chromium SDP/ICE, `ondatachannel` adoption) only exist in a browser.

### 2. Full `playwright` (test runner) instead of `playwright-core`

**Rejected:** the test runner, fixtures, and auto-download hooks are dead weight; the harness is the Rust
interop suite, and `playwright-core` keeps the dependency surface and install behavior minimal and explicit.

### 3. A third crate (or npm-side harness) for the browser cells

**Rejected:** would duplicate the proven server-spawn/JSONL/deadline machinery or rebuild it in TypeScript.
Feature-gating inside the native crate reuses it with zero impact on the default suite.

### 4. Driving the browser over raw CDP (chrome-remote-interface)

**Rejected:** playwright-core's lifecycle management (pipe transport, crash handling, `exposeFunction`
bindings with ordered delivery) is exactly the hard part; hand-rolling it buys nothing but bugs.

## References

- [Native Reference Client (ADR-0004)](0004-native-reference-client.md) — the contract this client mirrors
- [Protocol v3 Two-Axis (ADR-0001)](0001-protocol-v3-two-axis.md) — the negotiated surface
- [Matchbox Compatibility (ADR-0002)](0002-matchbox-compatibility.md) — the opaque signal payload convention
- [Browser reference client README](../../clients/browser/README.md) — CLI, deviations, local workflow
- [Native reference client README](../../clients/native/README.md) — the canonical JSONL event contract
- playwright-core: <https://playwright.dev/docs/library>

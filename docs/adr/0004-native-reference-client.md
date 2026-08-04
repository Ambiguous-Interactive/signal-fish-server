# Native Reference Client with a Real WebRTC Stack

## Status

ADR-0004 - Accepted (amended for webrtc-rs 0.20 in session 089)

## Context

Protocol v3 ([ADR-0001](0001-protocol-v3-two-axis.md)) gives the server a complete signaling story — capability
negotiation, the topology/transport ladder, authoritative per-recipient `SessionPlan`s, opaque `Signal` relay,
membership refreshes, and transport-status reporting — but until this ADR nothing in this repository ever ran that
story against a
**real WebRTC implementation**. The in-repo conformance suites drive real sockets and assert exact wire bytes, yet
their "clients" are test harness code: SDP strings are fabricated, ICE candidates are never gathered, and no DTLS
handshake or SCTP data channel ever opens. That leaves the most important claim — _a real client following
`docs/protocol.md` ends up with working peer-to-peer data channels_ — demonstrated nowhere.

A reference client has to make several structural choices carefully:

- it must not destabilize the server crate (pinned MSRV, locked dependency tree, zero-warning policy, supply-chain
  gates), and the server itself must stay free of any WebRTC dependency;
- [ADR-0002](0002-matchbox-compatibility.md) fixed the opaque signal payload to the matchbox `PeerSignal` shape
  specifically so matchbox-family clients interoperate — the reference client must stay on that wire shape;
- the conformance suites need machine-checkable, multi-process evidence (real OS processes, real UDP), not a demo.

## Decision

Build an **in-repo native Rust reference client** (`clients/native/`, package `signal-fish-reference-native`) on
**webrtc-rs 0.20 directly**, exercised by a **multi-process interop harness** that spawns the real server binary
plus N (≥ 3) client processes and asserts global properties over their JSONL stdout streams.

### In-repo standalone crate, not a workspace member

The crate lives in the repository (same review, CI, and drift pressure as the server) but is **not** a member of
the root package or any workspace: the root `Cargo.lock`, MSRV build, and coverage gates are untouched. Cargo
auto-excludes nested **packages** from `cargo package`, but plain files directly under `clients/` (such as
`clients/README.md`) belong to no nested package — the root `Cargo.toml` therefore carries an explicit
`exclude = ["clients/"]`, which is what actually keeps the published server crate byte-identical with or without
the client tree (verified: `cargo package --allow-dirty --list` at the root lists zero `clients/` files). The
root panic/timeout policy scans extend their source walk to the client crate, so the same discipline applies
without compiling it. The client pins the same `rust-version` as the server — enforced by the root
`scripts/check-msrv-consistency.sh` gate — and carries its own `Cargo.lock`.

### webrtc-rs directly, not a `matchbox_socket` adapter

The client drives `webrtc` 0.20 itself (one peer connection per planned peer, `reliable` +
`unreliable {ordered:false, max_retransmits:0}` data channels, trickle ICE). ADR-0002's compatibility is retained
**on the wire**: every signal payload is the matchbox `PeerSignal` shape, with `IceCandidate` carrying the JSON
serialization of webrtc-rs's `RTCIceCandidateInit` — exactly what `matchbox_socket` emits. Going direct rather
than through `matchbox_socket` is deliberate:

- the conformance harness needs full control over the v3 surface `matchbox_socket` does not model — Appendix G
  `TransportStatus` reporting, fallback engagement, per-recipient glare assertions, deterministic ICE crippling
  for fallback scenarios, protocol-version downshift to pure v2;
- compatibility with `matchbox_socket` remains a wire contract rather than a
  dependency-version contract: exact candidate JSON is pinned without inheriting
  matchbox's socket-level opinions.

### Protocol types via path dependency

The client consumes `ClientMessage` / `ServerMessage` straight from the server crate
through a local path dependency: the reference client can never drift from the server's wire contract,
and a protocol change that breaks clients fails this crate's build immediately. The trade-off — the client does
not independently prove the _documentation_ is implementable — is covered by the existing golden wire tests and
canonical JSONL samples (`.llm/code-samples/protocol/`), which pin the exact bytes against `docs/protocol.md`
for third-party implementers.

### Separate supply-chain scope

The client's independent dependency graph (the webrtc stack) gets its own `Cargo.lock` and its own
`clients/native/deny.toml` (mirroring the root policy; the only divergence is `allow-wildcard-paths = true` for
the version-less path dependency). CI audits it via a dedicated cargo-deny job in
`.github/workflows/webrtc-interop.yml`.

### Multi-process interop harness as the conformance vehicle

`clients/native/tests/interop_e2e.rs` spawns the **real server binary** (located via the environment variable
documented in the client README and exported by `scripts/run-webrtc-interop.sh`) and N real client processes per
scenario, then asserts over their drained JSONL event logs: glare-matrix antisymmetry, the
full per-channel message matrix, Appendix G status resolution and fan-out, relay-floor liveness during P2P,
crippled-ICE fallback, late-join (seat-fill) full-plan refreshes, and mixed v2/v3 relay-floor rooms. Everything is
deadline-bounded; there are no sleeps-as-synchronization.

### Zero-external-network CI posture

The interop server config disables TURN and configures an **empty** STUN list, so WebRTC plans carry zero ICE
servers and candidate gathering is host-interface (loopback) only: CI performs no external network access, and
the harness pins `ice_servers_count == 0` to keep it that way.

## Consequences

### Positive

- **End-to-end proof:** server-brokered offer/answer/trickle-ICE produces live DTLS+SCTP data channels between
  real processes — the end-to-end interop claim is now demonstrated and regression-tested in-repo.
- **Zero wire drift:** the path dependency makes the reference client track every protocol change at compile time.
- **Root package untouched:** MSRV, lockfile, coverage, and policy gates of the server crate are unaffected.
- **Matchbox-shape fidelity:** the candidate payload convention of ADR-0002 is exercised by a real ICE agent.
- **Deterministic fault injection:** `--cripple-ice` and seat-vacating flags make fallback and late-join scenarios
  reproducible without timing tricks.

### Negative

- **Second compilation graph:** the webrtc tree builds separately from the root (CI caches both; the crate is
  excluded from the root gates by design, so its checks must be wired explicitly — done in
  `webrtc-interop.yml`). The root cross-OS matrix likewise never sees it, so that workflow carries its own
  Linux/Windows/macOS compile + unit matrix; live WebRTC transport is proved on Linux only (see the client
  README's platform-coverage table).
- **Path-dep reuse means the client does not re-derive types from the docs:** third-party implementability rests
  on the golden wire tests, the canonical JSONL samples, and `docs/protocol.md` rather than on this client.
- **webrtc-rs quirks leak into the client** (async peer-event dispatch,
  poll-driven data-channel events, and local-only candidate metadata); they are
  isolated in `engine.rs` and documented in the client README.

### Mitigations

- The interop workflow path-filters on the client crate **and** every input the suite actually depends on: all
  server sources (`src/**` — the spawned binary is built from the whole tree), the root `Cargo.toml`,
  `Cargo.lock`, and `rust-toolchain.toml`, plus the runner script and the workflow itself. A server change that
  breaks real clients therefore fails CI even though the root gates never compile the client.
- `clients/native/README.md` documents the JSONL event contract, exit codes, payload conventions, and the
  scenario matrix, so the client doubles as executable documentation.

## Alternatives Considered

### 1. Build the reference client on `matchbox_socket`

**Rejected:** it would prove matchbox interop but cannot express the v3 conformance surface (transport-status
reports, fallback assertions, glare verification, v2 downshift, deterministic ICE crippling). Kept as a
complementary follow-up demo; the wire shape is already matchbox-compatible by construction.

### 2. Out-of-repo client repository

**Rejected:** an external repo drifts (no compile-time coupling to protocol changes, separate CI). In-repo with a
path dependency gives the strongest anti-drift guarantee at zero risk to the published crate.

### 3. Workspace membership for the client crate

**Rejected:** it would drag the webrtc dependency tree into the root lockfile, the MSRV gate, coverage, audits,
and every `cargo test` run. A standalone nested package keeps the boundary explicit; the root package's
`exclude = ["clients/"]` keeps the published artifact clean.

### 4. Hand-rolled SDP/ICE stubs instead of a real WebRTC stack

**Rejected:** that is exactly what the existing in-repo conformance suites already do; the gap being closed is
the real-stack proof (real candidate gathering, DTLS handshake, SCTP channels).

## References

- [Protocol v3 Two-Axis (ADR-0001)](0001-protocol-v3-two-axis.md) — the negotiated surface the client implements
- [Matchbox Compatibility (ADR-0002)](0002-matchbox-compatibility.md) — the opaque signal payload convention
- [Native reference client README](../../clients/native/README.md) — CLI, JSONL event contract, scenario matrix
- [Protocol Messages](../protocol.md) — the wire contract the client conforms to
- webrtc-rs: <https://github.com/webrtc-rs/webrtc>

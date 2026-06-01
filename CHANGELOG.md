# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added ADR-0001 (`docs/adr/0001-protocol-v3-two-axis.md`) locking the protocol v3 design:
  additive capability-gated versioning, relay-as-floor invariant, opaque-signal routing,
  deterministic glare avoidance, and the `{Relay, Host, Mesh}` topology / `{Relay, Direct, WebRtc}`
  transport sets.
- Added ADR-0002 (`docs/adr/0002-matchbox-compatibility.md`) deciding that native signal payloads
  follow the matchbox `PeerSignal` shape (`Offer` / `Answer` / `IceCandidate`) inside the opaque
  `signal` field while the server stays matchbox-decoupled.
- Added golden v2 wire snapshot tests (`tests/v2_wire_golden.rs`) freezing the current v2
  `ClientMessage` / `ServerMessage` JSON and MessagePack (`rmp_serde::to_vec_named`) wire format;
  any future diff is a breaking change and fails CI.
- Added protocol version and transport/topology capability negotiation (protocol v3 phase P1).
  `Authenticate` now accepts optional `protocol_version`, `supported_transports`
  (`relay` / `direct` / `webrtc`) and `supported_topologies` (`relay` / `host` / `mesh`) fields;
  the negotiated version + capabilities are persisted per connection. `ProtocolInfo` now advertises
  the negotiated `protocol_version` alongside the deployment's `min_protocol_version` /
  `max_protocol_version`. Added `protocol.min_protocol_version` (default 2) and
  `protocol.max_protocol_version` (default 3) config options with validation. Added a `/v3/ws`
  endpoint alias that shares the `/v2/ws` handler and defaults the protocol version to 3 when the
  client omits it. This change is fully backward compatible: clients that omit the new fields
  negotiate as pure v2 (relay-only) and observe byte-identical v2 behavior.
- Added targeted WebRTC signal relay (protocol v3 phase P2). `ClientMessage::Signal { to, signal }`
  relays an opaque, server-uninterpreted payload (matchbox-compatible `Offer` / `Answer` /
  `IceCandidate`) to a single peer in the same room, delivered as `ServerMessage::Signal { from, signal }`.
  On room join, existing v3 WebRTC peers and the joiner are paired via `ServerMessage::NewPeer
  { peer_id, you_initiate }`, where the deterministic glare rule (lesser UUID initiates) designates
  exactly one offerer per pair. Same-room enforcement, WebRTC-transport negotiation, and a
  per-connection signal rate limit (`rate_limit.max_signals`, default 600) are enforced, surfacing
  the new `CROSS_ROOM_SIGNAL`, `UNSUPPORTED_TRANSPORT`, `SIGNAL_TARGET_NOT_FOUND`, and
  `SIGNAL_RATE_LIMITED` error codes. All signaling is gated to v3 + WebRTC peers, so v2 clients never
  receive `Signal` or `NewPeer` and v2 wire behavior is byte-identical.

### Security

- Removed unmaintained `rustls-pemfile` dependency (RUSTSEC-2025-0134); PEM parsing now uses
  `rustls-pki-types` built-in `PemObject` trait.

### Fixed

- Fixed README protocol-reference formatting for the `Reconnect` row and typical session-flow diagram alignment.
- Fixed CI documentation failures by removing broken ADR links to an untracked local planning file and aligning ADR
  markdown with the repository lint rules.
- Fixed the production panic-policy violation in WebSocket capability deduplication by removing direct vector indexing
  and slicing.
- Hardened internal Markdown link validation so local checks fail when a link target exists locally but is not tracked
  by Git, matching clean CI checkout behavior.

### Changed

- Moved the optional feature compile matrix out of the default Rust test suite and into a single CI script step to avoid
  repeated nested Cargo builds in nextest, coverage, MSRV, Miri, and sanitizer jobs.
- Upgraded `sha2` from `0.10.9` to `0.11.0` and `hmac` from `0.12.1` to `0.13.0-rc.6` to align
  on `digest 0.11` and fix the `CoreProxy` trait bound build error.
- Bumped `hmac` from `0.13.0-rc.6` to `0.13.0` (stable release).
- Bumped `uuid` from `1.22.0` to `1.23.0`.
- Replaced `rustls-pemfile` with `rustls-pki-types` `PemObject` API for TLS certificate and
  private key loading, removing one dependency from the `tls` feature.
- Bumped `axum-test` dev-dependency from 18.7.0 to 19.1.1 (adapted test code for 19.x API changes).
- Bumped `tempfile` dev-dependency from 3.25.0 to 3.26.0.
- Bumped `tempfile` dev-dependency from 3.26.0 to 3.27.0.
- Bumped `tokio` from 1.49.0 to 1.50.0.
- Updated all transitive dependencies to latest compatible versions (`aws-lc-sys`, `cc`, `cmake`,
  `inventory`, `iri-string`, `mio`, `rustc-hash`, `unicode-segmentation`, `wasm-bindgen`,
  `zerocopy`, and others via `cargo update`).
- Normalized internal documentation link labels to human-readable text across
  troubleshooting and reference docs.

## [0.2.0] - 2026-02-24

### Changed

- Clarified protocol and guide documentation for player readying:
  `PlayerReady` is the canonical lobby-ready message, has no payload, and
  toggles ready/unready state per send.
- Updated MSRV from `1.87.0` to `1.88.0` and synchronized related configuration/documentation files.
- Updated production and development dependencies to latest compatible stable releases (verified 2026-02-15).
- Standardized dependency version requirements to minor-version form (for example, `1.0`) to allow safe patch updates.

### Added

- Added Architecture Decision Record (ADR) documentation scaffolding under `docs/adr/`.
- Added ADR index integration in `docs/README.md` and `docs/architecture.md`.

## [0.1.0] - 2026-02-15

### Added

- Initial release of Signal Fish Server.
- Core WebSocket signaling server with in-memory state.
- Room creation, joining, and leaving with room codes.
- Lobby state machine (`waiting` -> `lobby` -> `finalized`).
- Player ready-state and authority management.
- Spectator mode and reconnection with token-based event replay.
- In-memory rate limiting and Prometheus-compatible metrics endpoint.
- JSON config file + environment variable configuration.
- Docker image support.
- Optional TLS/mTLS support via `rustls` (`tls` feature).
- Optional legacy full-mesh mode (`legacy-fullmesh` feature).

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0

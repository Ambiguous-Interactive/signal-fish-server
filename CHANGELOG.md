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
  client omits it. The public router now mounts this alias only at the top level, avoiding a
  nested `/v2/v3/ws` route. This change is fully backward compatible: clients that omit the new
  fields on `/v2/ws` negotiate as pure v2 (relay-only) and observe byte-identical v2 behavior.
- Added per-room session plan / topology selection (protocol v3 phase P3). At lobby
  finalization (all players ready), the server now computes a single room-wide plan from the
  intersection of every member's negotiated capabilities and sends a per-recipient
  `ServerMessage::SessionPlan` (`{topology, transport, host?, peers, ice_servers?, fallback}`)
  to each v3-capable member, alongside the unchanged `GameStarting`. The selection ladder is
  `mesh+webrtc` → `host+webrtc` → `host+direct` → `relay` floor, where any member lacking the
  required capability (or a disabled transport) downgrades the whole room to relay; host election
  prefers the authority, else the earliest joiner (smaller UUID tie-break); each recipient's
  `peers[].initiate` is set by the deterministic glare rule (mesh: lesser UUID offers; host:
  clients offer to the host, the host offers to none). A room that resolves to the relay floor
  emits no `SessionPlan` and behaves byte-identically to v2 — v2 (and v3-relay-only) clients never
  receive one. Initial pairing is delivered exclusively by this `SessionPlan` at finalize; the
  late-join / reconnect `ServerMessage::NewPeer` path is now finalization-gated and transport-gated
  (it fires only for a join or reconnect into an already-`Finalized` room whose recomputed plan uses
  the WebRTC transport, then pairs per the plan's topology: mesh pairs the joiner with every other
  WebRTC peer, host pairs a client with the elected host only — clients never offer to each other —
  while a non-WebRTC plan, the relay floor _or_ a `host+direct` (LAN) session, emits no `NewPeer`).
  This supersedes the P2 behavior where `NewPeer` fired on every lobby-fill join. Added
  a `[session]` config block (`default_topology`, `game_topology_mappings`, `enable_webrtc`,
  `enable_direct`, `ice_servers`) with validation; the new `IceServer`, `SessionPeer`, and
  `SessionPlanPayload` types are additive over the frozen v2 wire format (`host`/`ice_servers`/
  credentials omitted when absent).
- Added ICE servers + ephemeral TURN credentials in the session plan (protocol v3 phase P4).
  A WebRTC `SessionPlan` now carries per-recipient `ice_servers`: the operator's static
  `session.ice_servers` (preserved verbatim) followed by the configured public STUN and, when
  `[turn].enabled` with `mode = "static_secret"`, a TURN entry whose `username` / `credential`
  are freshly minted **per recipient** via the coturn REST scheme — `username =
  "{expiry}:{player_id}"`, `credential = base64(HMAC-SHA1(static_auth_secret, username))` — so the
  static secret never reaches clients and each player receives distinct, time-limited credentials
  (all members of one finalize share a single `now + credential_ttl_secs` expiry). The HMAC is
  pinned to the RFC 2202 HMAC-SHA1 test vector. Non-WebRTC plans (`host+direct`, and the
  never-emitted relay floor) carry an empty `ice_servers` list, and a disabled `[turn]` block
  advertises only public STUN with no credentials. Added a `[turn]` config block (`enabled`,
  `mode` = `static_secret` | `managed`, `static_auth_secret`, `urls`, `stun_urls`,
  `credential_ttl_secs`, `managed_provider`, `managed_api_token`) with validation; `mode =
  "managed"` is a STUN-only stub in P4 (no outbound-HTTP dependency is added — provider minting
  is deferred). Added the `sha1` dependency. `Config.turn` is `#[serde(default)]`, so existing
  config files without a `[turn]` block still load and the v2 wire format is unchanged.
- Added the relay-fallback contract, transport status reporting, and transport metrics (protocol
  v3 phase P5). New optional, v3-only `ClientMessage::TransportStatus { transport, connected }`
  lets a client report its current data-path state
  (`{"type":"TransportStatus","data":{"transport":"webrtc","connected":true}}`); the server
  records it per connection and updates metrics, ignoring it from any non-v3 connection. A
  `connected:true` P2P transport (`direct`/`webrtc`) counts as a P2P establishment; `connected:false`
  counts as a relay fallback; `connected:true` with `relay` is "still on the floor" and moves no
  counter. The message is purely informational — the server relays `GameData` unconditionally, so
  the relay floor never closes regardless of what is reported. Added Prometheus counters for the v3
  transport surface: `signal_fish_transport_session_plans_emitted_total`, per-finalized-room
  topology (`signal_fish_transport_topology_{mesh,host,relay}_selected_total`) and transport
  (`signal_fish_transport_{webrtc,direct,relay}_selected_total`) selection,
  `signal_fish_transport_p2p_established_total`, `signal_fish_transport_relay_fallback_total`,
  `signal_fish_transport_signals_relayed_total`, and
  `signal_fish_transport_turn_credentials_issued_total`. Selection counters are recorded once per
  finalize in `emit_session_plan` (relay-resolved rooms included; the late-join path never counts,
  avoiding double-counting); `signals_relayed` counts a `Signal` at successful dispatch only;
  `turn_credentials_issued` counts each minted TURN credential. Documented the client
  transport/fallback state machine, the unconditional relay guarantee, the two-data-channel
  recommendation, and the metrics in `docs/architecture/transport-fallback.md`. The v2 wire format
  is unchanged — adding a `ClientMessage` variant leaves every existing variant byte-identical.
- Added targeted WebRTC signal relay (protocol v3 phase P2). `ClientMessage::Signal { to, signal }`
  relays an opaque, server-uninterpreted payload (matchbox-compatible `Offer` / `Answer` /
  `IceCandidate`) to a single peer in the same room, delivered as `ServerMessage::Signal { from, signal }`.
  On room join, existing v3 WebRTC peers and the joiner are paired via `ServerMessage::NewPeer
  { peer_id, you_initiate }`, where the deterministic glare rule (lesser UUID initiates) designates
  exactly one offerer per pair; P3's host topology later fixes this direction for star sessions
  (the client offers, the host answers). Same-room enforcement, WebRTC-transport negotiation, and a
  per-connection valid-signal rate limit (`rate_limit.max_signals`, default 600) are enforced.
  Rejected signal attempts use a separate `rate_limit.max_signal_errors` budget (default 60) so
  invalid targets and unsupported transports cannot bypass rate limiting or consume the valid ICE
  relay budget. These checks surface the new `CROSS_ROOM_SIGNAL`, `UNSUPPORTED_TRANSPORT`,
  `SIGNAL_TARGET_NOT_FOUND`, and `SIGNAL_RATE_LIMITED` error codes. All signaling is gated to v3 +
  WebRTC peers, so v2 clients never receive `Signal` or `NewPeer` and v2 wire behavior is
  byte-identical.
- Documented the protocol v3 wire contract and topology handoff (protocol v3 phase P6). Added a
  "Protocol v3 additions" section to `docs/protocol.md` (capability-negotiation handshake, the
  `Signal` / `NewPeer` / `SessionPlan` / `TransportStatus` messages, the `mesh+webrtc` →
  `host+webrtc` → `host+direct` → `relay` selection ladder, the late-join decision table, the
  glare/offerer rule, ICE/TURN credentials, and mesh + host sequence diagrams) and a new
  `docs/architecture/handoff-and-topologies.md` covering the finalization handoff seam and the three
  topologies. Added canonical v3 wire samples
  (`.llm/code-samples/protocol/v3-client-messages.jsonl`,
  `.llm/code-samples/protocol/v3-server-messages.jsonl`) referenced from `README.md`,
  `.llm/context.md`, and `.llm/context-protocol-and-scenarios.md`, plus a `tests/v3_protocol_samples.rs`
  test that deserializes every sample line into the real `ClientMessage` / `ServerMessage` types and
  asserts the `type` tag round-trips — the enforceable proof that the samples match the wire.

### Security

- Removed unmaintained `rustls-pemfile` dependency (RUSTSEC-2025-0134); PEM parsing now uses
  `rustls-pki-types` built-in `PemObject` trait.

### Fixed

- Fixed protocol v3 `TransportStatus` validation so reports for transports that
  were not negotiated by the connection are ignored and no longer update
  per-connection status or inflate P2P / relay-fallback metrics.
- Fixed protocol v3 `TransportStatus` metrics so duplicate reports of the same
  `(transport, connected)` state no longer inflate P2P-established or
  relay-fallback counters; counters now move only on a first report or a real
  per-connection state transition.
- Fixed local CI summary accounting so PowerShell-backed checks all report a
  failure when `pwsh` is unavailable, required local policy scripts fail closed
  when missing, and aggregate check helpers continue to the final summary after
  recording failures under `set -e`.
- Fixed in-memory room-operation lock cleanup so lobby transitions, authority changes,
  distributed room operations, and `PlayerReady` finalization release distributed locks
  immediately instead of relying on TTL expiry; protocol v3 session-plan e2e tests now
  document their receive timeout as a CI scheduling budget, not lock TTL compensation.
- Corrected `heartbeat_throttle_secs` documentation to describe throttled `last_seen`
  heartbeat writes rather than heartbeat logs.
- Hardened Rust CI policy detectors by replacing hand-rolled comment/string/char stripping with
  `syn`-based source analysis. The `bash_command` import/call-site check now handles ASCII,
  escaped, byte, and delimiter char literals, lifetimes, raw strings, comments, aliases, and both
  import/call cfg mismatch directions; the direct `Command::new("bash")` guard now ignores text in
  strings and comments while retaining line diagnostics. The no-panics pattern scan now also
  delegates Rust syntax classification to a parser-backed integration test instead of shell brace
  scanning.
- Fixed the protocol v3 session-plan selection ladder so a `desired` topology acts as a _ceiling_
  rather than an exact match: a `mesh`-preferring room that cannot run mesh now correctly falls back
  to `host+webrtc`, then `host+direct`, before the relay floor — instead of collapsing straight to
  relay (the previous code gated the host rungs on `desired == host`). This matches ADR-0001 §1 and
  the documented `mesh+webrtc → host+webrtc → host+direct → relay` ladder. The ladder is now expressed
  as a single data-driven constant (`UPGRADE_LADDER`) walked by topology-richness rank, the four legal
  `(topology, transport)` pairings are enforced by `is_valid_pair` plus a `debug_assert!`, and an
  exhaustive selection-invariant test guards the whole class of topology/transport drift.
- Fixed `handle_webrtc_late_join` emitting WebRTC `NewPeer` control messages for a non-WebRTC active
  session. Late-join pairing is now gated on the plan's _transport_
  (`SessionPlanDecision::uses_webrtc_signaling`, i.e. `transport == webrtc`) rather than its _topology_,
  so a `host+direct` (LAN) room — a non-relay topology whose transport is not WebRTC — no longer pushes
  clients into WebRTC negotiation. This mirrors `emit_session_plan`, which advertises ICE only for a
  WebRTC transport. The two emission gates (`is_relay` for `SessionPlan`, `uses_webrtc_signaling` for
  `NewPeer`/`Signal`) and their `host+direct` divergence are now pinned by a data-driven truth-table
  test, and the module/protocol doc comments corrected to describe the late-join gate as the WebRTC
  transport rather than a "non-relay" plan (a `host+direct` room is non-relay yet emits no `NewPeer`).
- Hardened `session.ice_servers` validation to reject any blank or whitespace-only URL (even alongside
  valid ones) and to report an empty `urls` list distinctly, instead of accepting a server as long as
  a single URL was non-blank. Blank URLs are propagated verbatim to clients and break `RTCIceServer`
  parsing, so they are now a configuration error with an index-pointed message.
- Fixed README protocol-reference formatting for the `Reconnect` row and typical session-flow diagram alignment.
- Fixed config-token drift by documenting canonical lowercase/snake_case values, making related
  config enum deserialization tolerate legacy mixed-case tokens, and adding doc/reference guards.
- Fixed `CoordinationConfig::default()` so `membership_snapshot_interval_secs` uses the documented
  30-second default instead of `0`.
- Fixed CI documentation failures by removing broken ADR links to an untracked local planning file and aligning ADR
  markdown with the repository lint rules.
- Fixed the production panic-policy violation in WebSocket capability deduplication by removing direct vector indexing
  and slicing.
- Hardened internal Markdown link validation so local checks fail when a link target exists locally but is not tracked
  by Git, matching clean CI checkout behavior.
- Fixed protocol v3 WebRTC reconnect behavior so restored peers are returned to room membership,
  receive fresh `NewPeer` pairing, and keep the reconnected player identity for subsequent WebSocket
  frames and disconnect cleanup.
- Fixed reconnection claim handling so failed restore attempts release the claim, roll back partial
  room restoration, and let clients retry with the same token until the reconnection window expires.
- Fixed room-join coordination cleanup so max-room-cap denials and room-count storage errors release
  distributed locks immediately instead of waiting for lock TTL expiry.
- Fixed cross-platform hook/link-checker issues: shared PowerShell native-process helpers now avoid
  synchronous stream deadlocks, Bash hook scripts avoid Bash 4-only features, and the fast link
  checker initializes empty file sets safely.
- Fixed the Rustdoc Validation CI failure by repairing two broken intra-doc links introduced by the
  protocol v3 work: the `config` module overview now uses explicit `crate::config::*` paths (a bare
  `` [`session`] `` did not resolve), and `config::session` no longer links to the private
  `server::session_policy` module.
- Fixed the Advanced Safety (Miri) CI failure: the protocol v3 session-policy tests reached
  `chrono::Utc::now()` through a fixture helper, which aborts under Miri's REALTIME-clock isolation.
  The pure-logic tests now build fixtures from a deterministic `base_time()` constant, so they run
  under Miri and no longer depend on real-clock skew for host-election tie-breaks.

### Changed

- Simplified the Miri (Advanced Safety) job to run with `MIRIFLAGS=-Zmiri-disable-isolation`,
  which lets the interpreter service wall-clock (`clock_gettime`), entropy (`getrandom`), and
  `getcwd` syscalls instead of aborting on them. This structurally eliminates the entire
  "test reached an isolated syscall" failure class, so the bespoke guard for it — a discovery
  scanner (`scripts/check-miri-compat.sh`), its parser-contract regression
  (`tests/miri_compat_gate_tests.rs`), the blocking `test_wall_clock_tests_ignored_under_miri`
  check, the CI pre-flight step, and ~30 per-test `#[cfg_attr(miri, ignore)]` annotations — was
  removed. Ordinary time/UUID-using unit tests — including `tokio::spawn` concurrency tests on the
  default current-thread runtime, a useful target for Miri's data-race detection — now run under
  Miri (increasing undefined-behavior coverage); only `proptest!` cases stay annotated for this
  reason (hundreds of generated cases are too slow under the interpreter). Pre-existing
  `#[cfg_attr(miri, ignore)]` annotations on async tests that need real I/O, timers, or multi-thread
  runtimes are unrelated and untouched. A new `test_ci_safety_runs_miri_with_isolation_disabled`
  pins the flag against regression.
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

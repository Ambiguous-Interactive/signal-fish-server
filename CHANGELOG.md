# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added ICE pre-gather on `RoomJoined` / `Reconnected` (PLAN §P4's deferred "RoomJoined ICE
  pre-gather" refinement): both payloads gain an optional `ice_servers` field carrying the same
  composed ICE list a WebRTC `SessionPlan` delivers — the operator's static `session.ice_servers`
  first, then the `[turn]` block's STUN entry, then a freshly minted per-player TURN credential
  (built by the single shared composition seam `composed_ice_servers_for`, so the two surfaces can
  never drift) — letting v3 WebRTC-capable clients start gathering ICE candidates during the lobby
  wait instead of adding that latency at game start. Strictly gated (the pure, exhaustively
  unit-tested `ice_pregather_eligible` predicate): the new `session.enable_ice_pregather` toggle
  (default `true`; the operator kill switch) AND `session.enable_webrtc` AND a non-relay desired
  topology for the game (a relay-desired game can never select a WebRTC plan, so minting for it
  would hand out credentials that can never be used) AND a non-`Finalized` room (a late join /
  reconnect into an active session receives its fresh ICE via the immediately following
  late-join `SessionPlan` — pre-gather is gated off there, so one logical join event never mints
  twice) AND a v3-negotiated recipient that advertised the `webrtc` transport AND the game's
  desired topology (the relay-desired argument applied per-recipient: the ladder seats a member
  on a rung only when it negotiated the rung's topology, so a relay-only-topology client can
  never appear in any WebRTC plan and its credential could never be used). In every other case
  the field is absent from the wire entirely (`skip_serializing_if`), so the v2 `RoomJoined` /
  `Reconnected` JSON and MessagePack bytes are untouched — all 44 v2 golden snapshots pass
  unchanged. The `SessionPlan` ICE list supersedes the pre-gather list (clients apply the most
  recent set; pre-gather credentials may expire during a long lobby). Observability: new
  `signal_fish_transport_ice_pregather_emitted_total` counter (one per payload that actually
  carried a non-empty list; an eligible joiner with no ICE configured emits no field and is not
  counted), and pre-gather-minted TURN credentials count on the existing
  `signal_fish_transport_turn_credentials_issued_total` total-issuance counter — for TURN capacity
  planning, issuance now scales with joins, not just finalizes (documented in
  `docs/deployment-turn.md`; `docs/protocol.md` and
  `docs/architecture/handoff-and-topologies.md` describe the wire field and gate). Covered end to
  end by the new `tests/v3_ice_pregather_e2e.rs` (composed list + per-player credential on join,
  raw-frame absence assertions for v2 / relay-only / kill-switch / WebRTC-disabled / relay-desired
  cases, STUN-only pre-gather with TURN disabled, late-join and reconnect single-mint invariants,
  and metrics deltas) plus a fully-populated `RoomJoined` line in the canonical v3 wire samples.
- Added the browser reference client (PLAN P7) as the in-repo standalone npm package
  `clients/browser/` (`signal-fish-reference-browser` — TypeScript, strict; NOT a crate, so every
  root cargo gate is untouched and `cargo package` still ships zero `clients/` files). The client
  drives a REAL Chromium `RTCPeerConnection` (the `chromium-headless-shell` build via
  `playwright-core` — actual browser ICE/DTLS/SCTP, not a Node WebRTC stack) through the full v3
  flow as two esbuild bundles: an IIFE page engine (WebSocket wire + protocol state machine +
  RTCPeerConnection engine, a faithful port of the native client's orchestrator) and a Node ESM
  CLI that launches Chromium, bridges page events to stdout JSONL, and reaps Chromium on every
  exit path (bounded close-then-kill on all catchable exits plus a detached reaper covering
  SIGKILL — headless Chromium does not exit on its own when its parent dies). The JSONL stdout
  event contract, flag surface, and exit codes are identical to the native reference client's,
  plus a browser-specific `--mdns-obfuscation` flag that leaves Chromium's `.local`
  host-candidate obfuscation ON; the empirically pinned outcome is that P2P still establishes via
  the peer-reflexive path (the native webrtc-rs agent learns the browser's transport address from
  the browser's connectivity checks and tolerates the unresolvable `.local` candidate). The
  browser↔native interop matrix cells live in the native crate's harness behind the new
  `browser-interop` cargo feature (`clients/native/tests/browser_interop_e2e.rs`, locating the
  built CLI via the new `SIGNAL_FISH_BROWSER_CLI` env var; the default native suite is
  unchanged): mixed mesh N=3 with the full glare/channel matrix, a browser↔browser mesh, a host
  star with the browser as a non-host client, a crippled-ICE browser relay fallback, the mDNS
  `.local` trap cell, a pure-v2 browser flooring a mesh-preferring room, a mid-handshake
  server-close probe (exactly one `error` event carrying the real close reason, prompt exit 3),
  and a SIGTERM/SIGKILL teardown cell pinning that Chromium never outlives the CLI (graceful
  teardown and the detached, pid-reuse-guarded orphan reaper respectively) — all over loopback
  with zero external network access (the cached Chromium download at install time is the only
  fetch). Wired into CI via `scripts/run-browser-interop.sh` (which also gates
  `cargo fmt --check` plus `cargo clippy --features browser-interop` over the feature-gated
  cells) and the path-filtered `.github/workflows/browser-interop.yml` (npm + Playwright-browser
  caching, lockfile-pinned `playwright-core install` — never bare `npx`). Recorded the design decisions in ADR-0005
  (`docs/adr/0005-browser-reference-client.md`) and documented the CLI, contract deviations, the
  mDNS posture, and the new matrix rows in `clients/browser/README.md` (the native README stays
  the canonical contract). Additive tooling/documentation only — no server runtime behavior or
  wire-format changes.
- Added the native Rust reference client (PLAN P7) as the in-repo standalone package
  `clients/native/` (`signal-fish-reference-native`, NOT a member of the root package — root
  lockfile/MSRV-build/coverage gates are untouched, `scripts/check-msrv-consistency.sh` pins the
  client's `rust-version` to the root MSRV, and the root `Cargo.toml` now carries
  `exclude = ["clients/"]` so `cargo package` ships no `clients/` files). The client
  drives a real WebRTC stack (webrtc-rs 0.17: actual ICE gathering, DTLS handshakes, SCTP data
  channels — one `reliable` + one `unreliable {ordered:false, max_retransmits:0}` channel per
  pair) through the full v3 flow, consuming the server crate's own protocol types via a path
  dependency (zero wire drift) and speaking the ADR-0002 matchbox `PeerSignal` payload shape with
  `IceCandidate` as the JSON-serialized `RTCIceCandidateInit`. stdout is a machine interface (one
  JSON event per line); flags drive room mode, ready barriers, channel exchange and relay-floor
  probes, deterministic ICE crippling, late-join gating, pure-v2 mode, and bounded run windows
  with documented exit codes. A multi-process interop harness
  (`clients/native/tests/interop_e2e.rs`) spawns the REAL server binary plus N≥3 client processes
  over loopback (TURN disabled, zero STUN URLs — no external network) and proves the native↔native
  interop matrix cells: mesh N=3 full WebRTC with a live relay floor, host star N=3, crippled-ICE
  relay fallback, late-join `NewPeer` seat-fill pairing, and mixed v2/v3 relay-floor rooms. Wired
  into CI via `scripts/run-webrtc-interop.sh` and the path-filtered
  `.github/workflows/webrtc-interop.yml` (interop suite + a cargo-deny audit of the client's
  independent dependency graph against `clients/native/deny.toml`). Recorded the design decisions
  in ADR-0004 (`docs/adr/0004-native-reference-client.md`) and documented the CLI, the JSONL event
  contract, transport-status semantics, and the scenario matrix in `clients/native/README.md`.
  Additive tooling/documentation only — no server runtime behavior or wire-format changes.
- Added the TURN relay deployment surface (P8 "deployment docs"). `docker-compose.yml` now ships
  an optional `coturn` service behind the `turn` compose profile
  (`docker compose --profile turn up`), pre-wired for the coturn REST-credential scheme
  (`--use-auth-secret`) with the shared secret and realm interpolated from the environment; a
  plain `docker compose up` is unchanged. The service refuses to start when
  `TURN_STATIC_AUTH_SECRET` is unset or empty — an entrypoint guard exits with a clear message
  instead of silently minting credentials from an empty HMAC key (an open relay). The guard is a
  runtime check because compose interpolates `${VAR:?}` file-wide even when the profile is
  inactive, which would break a plain `docker compose up`. Added `docs/deployment-turn.md`, the
  TURN deployment guide: when TURN is needed (~15–20% of real-world P2P connections), the coturn
  quick start against the compose profile, a walkthrough of the ephemeral credential scheme
  (`username = "{expiry}:{player_id}"`, `credential = base64(HMAC-SHA1(secret, username))`) and
  its operational consequences, zero-downtime rotation of the shared secret (coturn accepts
  multiple secrets at once), managed TURN alternatives (Cloudflare / Twilio / Metered) and the
  out-of-band-credential workaround for the current `mode = "managed"` STUN-only stub, why
  signaling must run over `wss://` (DTLS fingerprints travel in the SDP, so plaintext `ws://`
  allows a machine-in-the-middle of the WebRTC encryption itself), and capacity planning. Added
  `docs/architecture/scaling.md`, the multi-node scaling notes: what state a node actually holds,
  the room as the scaling unit (room affinity is the only constraint a multi-instance deployment
  must preserve), the cross-node seams already present in the code, and the `region_id` /
  room-code-prefix plumbing. `docs/deployment.md` gains the TURN-profile section, room-affinity
  scaling guidance, and a `wss://`-specific security-checklist item, all cross-linked; both new
  pages join the mkdocs nav. Documentation and compose-profile changes only — no server runtime
  behavior or wire-format changes.
- Added `ServerMessage::PeerTransportStatus { peer_id, transport, connected }` (protocol v3 only),
  the peer fan-out of an accepted `TransportStatus` report: when a v3 client's report records a
  real per-connection state change (the first report, or a `(transport, connected)` transition —
  duplicates fan out nothing), every other member of its current room that negotiated v3 is told
  the new state (for example, the host's WebRTC path died and relay-path traffic should be
  expected). The reporter is excluded; a room-less reporter's state is still recorded but fans out
  nothing; delivery is per-recipient v3-gated (a v2 member never observes it) but deliberately not
  gated on the recipient's own transport capabilities, since this is informational status about a
  peer rather than an instruction to use that transport. Purely informational — the relay floor
  never closes. Added the `signal_fish_transport_status_fanout_total` Prometheus counter (one per
  fan-out event, not per recipient), the canonical wire sample, and protocol/architecture docs.
  v2 wire bytes are unchanged (the message exists only on negotiated v3 connections).
- Added a formal-verification + property-testing layer for the protocol v3 session core. A TLA+
  specification (`formal/tla/SignalFishSession.tla`) models the v3 session lifecycle — finalize-time
  plan selection, per-recipient `SessionPlan` emission, late-join / seat-fill pairing, and
  host-failover re-planning — mirroring `src/server/session_policy.rs` and
  `src/server/signaling.rs` action-for-action, and TLC exhaustively model-checks it across four
  configurations (`desired = mesh`, `desired = host`, `host` with WebRTC disabled, and a
  relay-floor model with both upgrade transports disabled) for the named invariants (back-compat
  v2 gating, plan legality, the host-validity self-heal contract, the desired-topology ceiling,
  sticky topology/transport, mesh glare antisymmetry, host-star shape, two-sided peer-capability
  filtering, the no-qualifier re-plan drop, and the relay-floor enabled-gate denial). A pinned,
  SHA256-verified TLC runner (`scripts/run-tla-model-check.sh`) and a path-filtered CI workflow
  (`.github/workflows/formal-verification.yml`) run the check on changes to the spec or the modeled
  source. Added proptest invariant suites over the real code: selection / host-election / peer-list
  properties (`src/server/session_policy_tests.rs`), v3 wire round-trips for JSON and MessagePack
  plus TURN-credential determinism (`tests/v3_wire_properties.rs`), and parser fuzz-hardening
  (`tests/protocol_fuzz_hardening.rs`) that asserts the decoders never panic on arbitrary bytes,
  mutated samples, or deep-nesting bombs — including a release-profile-only probe enforcing the
  measured MessagePack depth-limit stack margin on a default 2 MiB worker stack. Documented the
  layered approach in
  `docs/architecture/formal-verification.md` and the tool-choice rationale in ADR-0003
  (`docs/adr/0003-formal-verification-and-fuzzing.md`). This is test/verification tooling only — no
  runtime behavior or wire format changes.
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
  avoiding double-counting); `signals_relayed` counts a `Signal` after validation, before
  best-effort dispatch; `turn_credentials_issued` counts each minted TURN credential. Documented the client
  transport/fallback state machine, the unconditional relay guarantee, the two-data-channel
  recommendation, and the metrics in `docs/architecture/transport-fallback.md`. The v2 wire format
  is unchanged — adding a `ClientMessage` variant leaves every existing variant byte-identical.
- Added targeted WebRTC signal relay (protocol v3 phase P2). `ClientMessage::Signal { to, signal }`
  relays an opaque, server-uninterpreted payload (matchbox-compatible `Offer` / `Answer` /
  `IceCandidate`) to a single peer in the same room, dispatched on the best-effort relay path as
  `ServerMessage::Signal { from, signal }`.
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
- Added mid-session re-planning for protocol v3 sessions (host failover + stored-plan consistency).
  Each non-relay finalize now records the room's _active session plan_ (topology, transport, host) —
  the single source of truth for the session the room is running — and removes it when the room
  empties or is cleaned up. Whenever a membership-touching event (a departure — explicit `LeaveRoom`
  or disconnect — or a late join/reconnect) finds a `host`-topology session whose stored host is no
  longer a member (or is seated but no longer capable of the session, after a reconnect that
  downgraded its negotiated capabilities), the server re-elects a host over the remaining members and re-issues a fresh
  per-recipient `SessionPlan` to every remaining v3 member — same sticky topology/transport, new
  `host`, freshly minted per-recipient TURN credentials for WebRTC — instead of leaving the room
  pointed at a dead host (the invalid-host trigger also self-heals wedge states such as a re-plan
  skipped by a transient storage error). Re-election is capability-aware: only members that
  negotiated v3 plus the sticky topology/transport pair are electable (the authority preference
  passes the same filter, so a seat-filling v2 or relay-only authority is never named host of a
  session it cannot run; otherwise authority preferred, else earliest joiner with a smaller-UUID
  tie-break), and when no member qualifies the stored plan is dropped with no emission — the
  session is over and the relay floor carries the room. Re-issued and late-join plan peer lists
  are capability-filtered on both sides by the same predicate: `peers[]` names only members that
  negotiated the session's sticky topology/transport, so a v3 member that did not (e.g. a
  relay-only seat-filler, or one with the WebRTC transport but not the session's topology)
  receives its (v3-gated) plan with an empty peer list and participates
  via the relay floor (`host` stays as elected, informational), and capable members never see it
  listed — and the late-join `NewPeer` gating applies this same full predicate to both ends of
  every announced pair, so clients are never instructed to attempt WebRTC pairs the plan itself
  excludes (or that `Signal` validation would reject); at finalize the filter is vacuous because
  plan selection requires every member to support the plan. A late joiner or reconnector entering
  an active non-relay session now receives its own tailored `SessionPlan` (current peers,
  glare-correct `initiate` flags, stored host, fresh ICE) and is no longer sent joiner-side
  `NewPeer`s; existing members still receive the
  additive `NewPeer` delta (mesh: every session-capable member; host: the star edge only), making the client
  contract uniform — the latest `SessionPlan` wins; `NewPeer` is an additive delta for existing
  members. Topology/transport are sticky for the session lifetime (the selection ladder runs once at
  finalize and is never re-run mid-session). A late join that itself heals an invalid host is served
  by the re-plan to every v3 member (joiner included) instead of the joiner-plan + `NewPeer` pair. No new
  message types and no wire-shape changes; all emission stays v3-gated. Added Prometheus counters
  `signal_fish_transport_session_replans_emitted_total` (one per host re-plan event — departure
  failover or late-join self-heal; not moved when no member qualifies and the plan is dropped)
  and `signal_fish_transport_session_plans_late_join_total` (one per joiner that received a
  late-join plan; a heal-served joiner counts on the re-plan event instead);
  `signal_fish_transport_session_plans_emitted_total` keeps meaning "finalized
  non-relay rooms", and TURN credentials minted by re-plans/late-join plans count toward the
  existing `signal_fish_transport_turn_credentials_issued_total`.
- Added a protocol v3 multi-peer (N≥3) signaling conformance suite
  (`tests/v3_multipeer_e2e.rs`): full-lobby flows over real WebSockets pinning the global mesh
  glare matrix at N=3/N=4 (every unordered pair has exactly one offerer — the smaller UUID — and
  pairwise opaque signals relay byte-identically across all ordered pairs), the strict N=4 host
  star property (clients offer only to the host and never appear in each other's plans), the mixed
  v2+v3 relay floor (`GameStarting` for everyone, no `SessionPlan`/`NewPeer` leakage to anyone),
  N=4 host-failover re-planning (one fresh star-correct plan per survivor naming the
  earliest-joined remaining member) plus an N=4 cascade variant (two consecutive host deaths, each
  wave re-electing and re-issuing from the surviving session state), seat-filling late join into a
  live mesh session (joiner plan +
  per-member `NewPeer` deltas), and the full wire reconnect flow (`Reconnected` + fresh plan,
  `PlayerReconnected` + `NewPeer`, post-reconnect signals under the restored player id).
- Added a true multi-process conformance suite (`tests/v3_multiprocess_e2e.rs`) that spawns the
  compiled `signal-fish-server` binary as a real child process (per-test temp config via
  `SIGNAL_FISH_CONFIG_PATH`, free-port reservation with spawn retries, `/v2/health` readiness
  polling, and a kill-on-drop child guard so no orphan survives a panicking test): a 3-peer mesh
  session over real TCP repeats the glare-matrix and pairwise-signal assertions across a genuine
  OS process boundary, and a SIGKILL + same-port restart proves clients observe the close, the
  in-memory reconnection registry dies with the process (old reconnect identities are rejected
  with `ReconnectionFailed`), and fresh sessions work against the new process. Enabled the
  tokio `process` feature for dev/test builds only.

### Security

- `--print-config` now redacts secrets (P8 security hardening, PLAN Appendix I). The printed JSON
  replaces every **set** secret value with the marker `<redacted>` while leaving unset (`null` /
  empty) secrets as-is, so operators can still tell "configured" apart from "missing". Redacted
  fields: `security.metrics_auth_token`, `security.authorized_apps[*].app_secret`,
  `session.ice_servers[*].credential` (static TURN credentials), `turn.static_auth_secret`, and
  `turn.managed_api_token`. TLS file _paths_ are locations, not secrets, and stay visible. Backed
  by a future-proofing test that sweeps the serialized output of a fully-populated config for any
  string field whose key looks credential-like (`*secret*`, `*token*`, `*password*`,
  `*credential*`, `*api_key*`) and fails if one survives redaction
  (`Config::redacted_for_display`, `src/config/types.rs`).
- Added a dedicated cap on the serialized size of the opaque v3 `Signal` payload (P8 / Appendix I
  "cap signal payload size"). New config key `security.max_signal_bytes` (default `16384` — 16 KiB,
  generously above any real SDP/ICE payload and well under the 64 KiB `security.max_message_size`
  frame cap; must be `> 0` and ≤ `max_message_size`, larger values are rejected at validation as
  dead config). `handle_signal` measures the canonical serialized JSON length of the `signal` value
  before any relay work: payloads exactly at the cap relay unchanged, payloads over it are rejected
  with the new `SIGNAL_TOO_LARGE` error code without consuming the sender's valid-signal rate
  budget. The cap runs as step 0 of `handle_signal`, before the sender's v3+WebRTC transport gate,
  so a malformed, oversized `Signal` from ANY client is rejected with `SIGNAL_TOO_LARGE` — including
  a misbehaving v2 client, which previously received `UNSUPPORTED_TRANSPORT` for it. Golden v2 wire
  behavior is unchanged for protocol-conforming v2 clients, which never send `Signal`.
- Added a post-authentication idle timeout on the WebSocket read path (P8 / Appendix I
  "idle-timeout"). New config key `websocket.idle_timeout_secs` (default `300`; `0` disables): an
  authenticated connection that produces no inbound frame of any kind (including Ping/Pong) for the
  configured window receives an `Error` with the new `CONNECTION_IDLE_TIMEOUT` code and is closed
  through the normal disconnect path, so the reconnection grace period still applies. The error is
  delivered on the connection's own outbound channel rather than through the message coordinator,
  because under production defaults the `server.ping_timeout` reaper (30s) has already unregistered
  a silent client from the coordinator long before the idle window (300s) elapses — a
  coordinator-routed error would be silently dropped. This closes a
  real gap: the existing `server.ping_timeout` reaper only removed server-side connection _state_
  (and only counts `Ping` as activity) but never closed the socket, leaving zombie TCP connections
  holding file descriptors indefinitely. The 300s default cannot affect healthy clients, which
  already must heartbeat every ~30s to survive the state reaper. The pre-auth handshake remains
  bounded by the stricter `websocket.auth_timeout_secs`.
- Added a prominent once-at-startup warning when TURN is enabled but built-in TLS is disabled
  (PLAN Appendix I: `wss://` for signaling in production — DTLS fingerprints travel in SDP, so
  plaintext `ws://` signaling allows man-in-the-middle of the WebRTC peer connections). Emitted
  after logging initialization via `tracing::warn!`; deliberately a warning and never a hard error
  because reverse-proxy TLS termination (where `security.transport.tls.enabled` stays `false`) is
  the common production deployment (`config::should_warn_missing_signaling_tls`).
- Removed unmaintained `rustls-pemfile` dependency (RUSTSEC-2025-0134); PEM parsing now uses
  `rustls-pki-types` built-in `PemObject` trait.

### Fixed

- Fixed CI reliability by normalizing all workflow `actions/checkout` pins to `v6.0.3`, making the
  browser interop Chromium teardown check tolerant of process-name/topology drift with better
  `/proc` diagnostics, and allowing doc-consistency version checks to read CRLF `.llm/context.md`
  lines correctly.
- Fixed the creator's stored `is_authority` flag so it matches `authority_player` in rooms created
  with `supports_authority: false`. `create_room` previously seeded the creator's `PlayerInfo` with
  `is_authority: true` unconditionally while correctly leaving `authority_player` unset, so the two
  wire surfaces derived from them could contradict each other: `RoomJoined.is_authority` (and the
  `Reconnected` payload) reported `false` while the v2 `current_players` list /
  `GameStarting`-adjacent peer metadata and v3 mesh `SessionPeer.is_authority` (copied from the
  stored flag) reported `true`. Both now derive from the same condition: in an authority-less room
  nobody — including the creator — is marked authority.
- Fixed a critical protocol v3 dead-session bug where the elected host disconnecting from a
  finalized `host`-topology room left every remaining client holding a `SessionPlan` that pointed at
  a dead host: authority was silently cleared, no host was re-elected, and no new plan was issued.
  Host departures now trigger the mid-session re-plan described under Added (re-election + fresh
  per-recipient `SessionPlan`s), hooked into `leave_room` so both explicit leaves and disconnects are
  covered.
- Fixed protocol v3 late-join/reconnect pairing recomputing the session plan over the current
  members instead of consulting the plan the room actually runs. A room that finalized to the relay
  floor (for example one v3 + one v2 member) could, after the v2 member departed and a v3+WebRTC
  player filled the seat, wrongly emit `NewPeer` and push clients of a relay session into WebRTC
  negotiation even though no `SessionPlan` was ever issued. Late join and reconnect now read the
  stored active session plan: no stored plan ⇒ no v3 emission at all; topology/transport are sticky.
- Fixed lobby finalization never persisting `lobby_state = "finalized"` to room storage (the
  in-memory coordinator only broadcast `GameStarting` and tracked ready state in its own map, so the
  stored room stayed `lobby`). A post-game departure therefore regressed the room to `waiting` and a
  refill replayed the whole lobby/ready/`GameStarting` cycle, and every `Finalized`-gated path
  (late-join/reconnect pairing, departure re-planning) was unreachable in production. The room
  coordinator now persists the finalized state (with player ready flags) under the room-operation
  lock before broadcasting `GameStarting`; a player joining a finalized non-full room now sees
  `lobby_state: "finalized"` (an existing v2 wire value) in `RoomJoined` instead of a spurious
  re-lobby cycle. Post-finalize `PlayerReady` toggles now receive `Error{error_code:
  INVALID_ROOM_STATE}` instead of mutating lobby state (matching the documented terminal
  `Finalized` state).
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
- Fixed `handle_active_session_late_join` emitting WebRTC `NewPeer` control messages for a non-WebRTC active
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

- Tightened ICE URL validation in the `[session]` and `[turn]` config blocks (closing the
  scheme/deduplication check deferred from P4): every `session.ice_servers[].urls` entry must now
  start with one of the four ICE schemes (`stun:`, `stuns:`, `turn:`, `turns:`), every `turn.urls`
  entry with `turn:`/`turns:`, and every `turn.stun_urls` entry with `stun:`/`stuns:` — matched
  case-insensitively (URI schemes are case-insensitive per RFC 3986 §3.1) and requiring a
  non-empty remainder after the colon (`turn:host:3478?transport=udp` and IPv6 literals like
  `turn:[2001:db8::1]:3478` remain valid; a bare `stun:` or a space inside the scheme is
  rejected). **This is intentional fail-fast behavior:** a config that previously started with a
  malformed scheme (e.g. `http://example.com` or a typo like `trun:`) now fails validation at
  startup with the existing indexed message style (`session.ice_servers[i].urls[j] …` /
  `turn.urls[i] …`) instead of propagating a URL clients' `RTCIceServer` parsing would choke on.
  Like the existing blank-URL hygiene, the scheme check applies regardless of `turn.enabled`. The
  check lives in one shared private helper (`src/config/ice_url.rs`) used by both blocks.
  Exact-duplicate URLs (within one server's list or across a block's full URL set) additionally
  log a deterministic `tracing::warn!` but deliberately stay non-fatal, mirroring the existing
  warn-but-succeed precedent for the disabled-P2P topology warning.
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

### Removed

- Removed the `managed` TURN mode (the third-party-cloud credential source) and
  its entire config surface — `turn.mode`, `turn.managed_provider`,
  `turn.managed_api_token` — along with the `TurnMode` type. TURN is now
  **self-hosted only**: when `turn.enabled`, the server self-mints coturn REST
  credentials locally from `turn.static_auth_secret` for a TURN server the
  operator runs, and never contacts a third-party cloud or uses external
  credentials. `managed` was only ever a STUN-only stub, so no working
  functionality is lost. The `[turn]` block is otherwise unchanged (`enabled`,
  `static_auth_secret`, `urls`, `stun_urls`, `credential_ttl_secs`), and because
  the config structs do not use `deny_unknown_fields`, a legacy config still
  carrying `mode` / `managed_*` keys continues to load (the stale keys are
  ignored). Updated `config.example.json`, `docs/configuration.md`, and
  `docs/deployment-turn.md` to describe the self-hosted-only model.

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

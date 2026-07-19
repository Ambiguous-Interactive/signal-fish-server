# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Native reference clients can now coordinate bounded rebuilds of an
  incomplete WebRTC pair before the P2P deadline, allowing homogeneous clients
  to recover when packet loss leaves ICE connected but the SCTP data-channel
  handshake stalled.

- Refresh the compatible runtime, TLS, parser, and test-tool dependency set
  while keeping Axum and the WebSocket test client on one aligned Tungstenite
  version. Remove the redundant production declaration of the test-only
  `tokio-tungstenite` client. Restore the local full supply-chain audit on
  current cargo-deny by passing its global feature option before the `check`
  subcommand.
- Extend the exact-release Fortress/Godot no-thread WASM interoperability gate
  with a weekly Firefox cell. Pull requests retain the deterministic Chromium
  gate, while the scheduled run applies the same healthy released-client and
  expected-busted negative-control oracles to a separately attested Firefox
  process pair.
- Complete the H6 reconnect-race falsification matrix with a deterministic,
  test-only teardown gate. The gate proves a reconnect rejected as
  `PlayerAlreadyConnected` after its record is armed does not consume the token,
  and that the same token succeeds as soon as the old connection is removed.
- Advance the Fortress/Godot no-thread WASM acceptance gate to the exact
  crates.io releases `signal-fish-client` 0.9.0 and
  `signal-fish-client-godot` 0.9.0. The primary two-browser cell now requires
  every P13 health invariant to pass, while the one-admission-per-callback
  negative control must remain explicitly `BUSTED`.

## [0.5.0] - 2026-07-18

### Fixed

- Make release publication retry-safe by deriving the manual version from the
  reviewed default-branch source, reusing only matching immutable annotated
  tags, validating crates.io state before tag and container mutation, keeping
  registry probes outside the checkout, and enforcing a clean source tree
  immediately before `cargo publish`.

## [0.4.1] - 2026-07-18

### Added

- Add a manual **Prepare Release** workflow with a `patch` / `minor` / `major`
  dropdown and optional dry run. It deterministically bumps the root crate and
  path-package lockfiles, synchronizes public version references, cuts the
  dated Keep a Changelog section and comparison links, validates the prepared
  tree, and opens a `release/vX.Y.Z` pull request using an installation token so
  normal PR CI runs. The existing **Release - Publish Crate** workflow remains
  the reviewed second phase and publishes crates.io, the canonical annotated
  GitHub release/tag, verified GHCR images, SBOM, and platform archives.
- Add the browser completion cell for the Fortress Rollback issue-242 gate.
  The exact released Fortress 0.10.0 and Signal Fish Rust client 0.8.0 graph now
  runs inside two independent Godot 4.5 no-thread WASM exports in Chromium,
  exercising one poll per real Rust game callback and reporting exact
  cross-peer relay ledgers. The released graph produces complete reports at
  confirmed frame 607 as `BUSTED`: although the adapter admits multiple
  messages per callback, the client completes about one send per callback and
  misses the healthy throughput, callback-cadence, wait, stall, queue-age, and
  wall-time gates. CI
  requires a complete 600-frame characterization with no unrelated failure,
  alongside an expected-busted one-admission control, and P13 remains open.
  This result is Chromium-specific and does not generalize to every browser.
- Add a pinned Fortress Rollback issue-242 interoperability gate. Two real game
  processes use `fortress-rollback` 0.10.0 and the released Signal Fish Rust
  client 0.8.0 to advance 600 confirmed frames through a server built from the
  current checkout. CI now rejects relay stalls, wait recommendations, loss,
  overflow, excessive rollback/confirmation lag, low throughput, incomplete
  sends, old queued frames, or mismatched state checksums.
- Add an opt-in delivery trace-refinement pilot for the reliable v2 FIFO.
  Feature-gated per-connection JSONL capture now replays producer, writer,
  queue-close, and finalization transitions against TLA+, rejects unsupported
  projections and mismatched write phases, and retains deterministic nightly
  evidence. The feature is inert in default builds and does not change the
  public protocol or normal runtime behavior.
- Add data-backed relay-floor sizing guidance for fan-out bandwidth, queue-fill
  and fail-loud timing, batching latency, and directional partitions. A nightly
  exact-ledger 16-player rate sweep reports the GitHub runner's achieved
  ingress, delivery throughput, latency quantiles, backpressure, and RSS so
  operators can compare their own deployment benchmark without treating CI
  hardware as a portable capacity guarantee.
- Document bidirectional WebSocket liveness under symmetric and one-way
  partitions. The client and operator guides now distinguish reliable-delivery
  close `4002 slow_consumer` from transport-liveness close `4003
  activity_timeout`, describe the default protocol-Ping detection bound, and
  require clients to reconnect instead of treating one-way progress as a
  healthy connection. Real-socket fault experiments pin both close mechanisms
  and prove room relay recovery after each eviction.
- Add a harness-only native reference-client success barrier and a nightly
  16-player WebRTC mesh experiment. The client can emit
  `success_criteria_met` and remain connected until a release file appears,
  allowing the experiment to prove the simultaneous 120-edge graph, exact
  reliable/unreliable channel ledgers, relay-floor delivery, and production
  600-signal budget without teardown races. The barrier waits for every current
  peer connection's terminal ICE-gathering marker before freezing the signal
  ledger. Normal client exit behavior is unchanged when the new flag is
  omitted, the soft run deadline does not break an active barrier hold, and
  a release observed after that deadline still honors the post-success linger.
  Held clients sleep until that linger expires instead of spinning on an
  elapsed soft deadline, and the nightly harness keeps explicit watchdog
  headroom for its barrier assertions before coordinated release.
  Once success is reported, the release hold remains authoritative even if
  criteria temporarily regress; criteria are revalidated after release.
  Release-file metadata polling runs off the async networking executor.
  Hard-watchdog diagnostics name the exact release path. Release-path metadata
  errors fail immediately with the underlying I/O diagnostic instead of
  masquerading as an absent file. The same nightly suite now includes a
  16-player partial-partition variant: one deterministically ICE-crippled
  client must fall back exactly once while the other fifteen preserve their
  exact 105-edge WebRTC submesh, and all sixteen preserve the complete
  WebSocket relay-floor ledger. Terminal peer-connection generations count as
  signal-settled for the harness barrier because they can emit no later ICE
  candidates; live generations still require their end-of-gathering marker.
  The same held real-process harness now covers the complete clean
  `{mesh, host} x {2, 8, 16}` topology/size grid with exact plan, signaling,
  channel, room-wide transport-status, and relay-floor ledgers. The signal
  budget guard accounts for the accepted `TransportStatus` report that shares
  the 600-message control-plane bucket with WebRTC signals. Four additional
  `{mesh, host} x {2, 8}` cells form every planned pair under verified 1%
  loopback packet loss, then lift the fault before releasing the exact channel
  exchange. The native client's new harness-only exchange release file emits
  `exchange_ready` after pair formation and ICE gathering; ordinary exchange
  behavior remains unchanged when the flag is omitted. Loss cells use a
  harness-only `--disable-mdns` mode so netem faults unicast ICE rather than the
  local candidate-discovery control plane; normal mDNS candidate privacy is
  unchanged. A final N=3 pairwise partition cell reciprocally drops ICE
  candidates on exactly one planned edge, proves the other two WebRTC edges and
  the all-to-all WebSocket relay floor remain complete, and records every
  injected drop in the client's JSONL event stream. Normal candidate handling
  is unchanged unless a harness-only fault flag is supplied.
- Add canonical release identity across the annotated Git tag, crates.io,
  GitHub Release, and multi-architecture GHCR image. Manual releases now invoke
  container publication directly, publish matching `vX.Y.Z`, `X.Y.Z`, and
  immutable `sha-*` tags from one verified digest, record that digest and source
  revision in the Release notes, fail closed on identity drift, and support safe
  retry completion plus tagged historical backfills.
- Document the single-instance deployment contract and prove its unsupported
  multi-process failure modes with a nightly two-binary H5 experiment. Startup
  logs now state that room/reconnect state is in-memory, room affinity is
  required, and session handoff is unavailable. The scaling/deployment docs no
  longer imply that generic sticky sessions provide horizontal safety. Run-mode,
  pre-deployment, configuration, API, and metrics descriptions now consistently
  identify shipped coordination as process-local and remote coordination as a
  future extension seam. Added ADR-0006 for the completed protocol-v3 delivery-reliability revision
  (delivery classes, epoch, reconnect watermarks, drain, and WebSocket pings),
  including the rejected/deferred feature cut list.

### Fixed

- Make historical GHCR backfills load publication helpers from the exact
  workflow revision while building only the tagged source checkout. Releases
  that predate the helper scripts no longer fail before the container build,
  and current workflow tooling cannot alter the historical build context.
- Fixed negotiated lossy delivery under a slow-but-draining TCP downstream.
  A bounded, configurable TCP send buffer (`websocket.socket_send_buffer_bytes`,
  default 65536; `0` restores the platform default) prevents megabytes of data
  already accepted by the kernel from burying later WebSocket Pings and exact
  `DeliveryReport` frames. Writer sojourn deadlines are now class-aware:
  reliable traffic retains its oldest reliable queue-plus-write ceiling,
  control traffic owns its enqueue deadline, and latest/volatile traffic gets a
  bounded write-progress deadline without inheriting unrelated queue age. The
  same progress deadline now covers exact reports for deterministic
  writer-discovered format failures: those payloads have already reached an
  accounted unsupported terminal outcome and no longer inherit the age of
  unresolved reliable data. Valid cross-encoding fallbacks reuse the preflight
  decode rather than parsing the payload twice. The
  nightly 256-kbps asymmetric experiment now keeps a 90-KiB/s volatile stream
  connected for 60 seconds with production Pings enabled and every observed
  sequence gap covered by a causally prior exact report; reliable traffic still
  fails loudly with `4002 slow_consumer` and reconnects with rotated wire tokens.
  Registered shutdown waiting also includes scheduling margin after its three
  bounded close writes, so a handler completing at the final write deadline is
  not mistaken for a leaked socket under sanitizer or production shutdown load.
  Server transport probes are now idle-only: decoded non-Pong traffic is
  published before awaited application processing, skips an unnecessary probe,
  or cancels an outstanding one. Exact matching Pongs still record RTT, stale
  Pongs still cannot satisfy a probe, and genuine silence still closes `4003`.

### Changed

- Rate-limit protocol-v3 unsupported-format advisory errors per sender and
  recipient while preserving an exact `DeliveryReport` for every omitted
  sequence. Mixed-encoding malformed payloads can no longer double each relay
  into an unbounded report/error stream; later advisories include the number
  suppressed since the prior notice.

## [0.4.0] - 2026-07-12

### Added

- Add server-initiated RFC 6455 liveness probes (P10.E4). By default each
  connection receives a transport-level Ping every 10 seconds and must return
  the matching Pong within 5 seconds or close with `4003 activity_timeout`.
  The probes bypass application delivery queues, require no client protocol
  change, bound Ping socket writes independently of the Pong deadline and map
  failures to `4003 activity_timeout`, use unpredictable nonzero probe nonces,
  reject Pongs observed before the socket write begins, accept matching Pongs
  through the exact deadline, preserve the first matching reply against later
  unsolicited Pongs, record probe receipt before asynchronous activity refresh,
  and publish timeout and round-trip latency metrics measured at Pong receipt.
  Operators can set
  `websocket.server_ping_interval_secs` to `0` to disable them. Timeout logs are
  emitted only when activity timeout wins the connection close race; the
  ping-timeout counter increments only for a missed Pong deadline that wins that
  race, never for observation-channel shutdown. Documentation names the exported
  Prometheus series and consistently distinguishes these WebSocket probes from
  application pings, and reflects the 24-connection default in examples and
  deployment guidance.

- Added protocol-v3 delivery classes and exact gap accountability (P10.E2).
  JSON `GameData` now supports `reliable` (the default), keyed `latest`, and
  `volatile`; raw binary game data remains reliable. Per-connection data and
  generation-scoped priority control queues enforce class-specific
  conservation, while `DeliveryReport` publishes cumulative per-class outcomes
  plus up to 256 exact `(sender, epoch, sequence range, reason)` omissions per
  frame before later data can expose a gap. Additional ranges roll into further
  reports. Recipient room/spectator transitions remain ordering barriers.
  Reliable capacity timeout, maximum outbound/write sojourn, and inability to
  preserve exact accountability all fail closed with `4002 slow_consumer`. The
  frozen v2 wire shape and reliable FIFO behavior are unchanged. Operators gain
  raw JSON `connections.delivery_by_class` snapshots and Prometheus
  `signal_fish_websocket_delivery_class_outcomes_total{class,outcome}` totals.
  Every v3 binary delivery now carries the same mandatory MessagePack
  `from_player` / `encoding` / opaque `payload` / `seq` / `epoch` envelope,
  including JSON and reserved rkyv payload paths; frozen v2 bytes are unchanged.
- Added graceful shutdown drain (P10.E3). `SIGTERM`/Ctrl-C now stop new WebSocket
  upgrades, reject room-creating joins with `SERVER_DRAINING`, send v3 clients a
  best-effort `GoingAway { deadline_ms, retry_after_secs }` advisory, then close
  remaining sockets with `4000 server_shutdown` after `server.drain_grace_secs`
  (default 30; `0` closes immediately). Shutdown-drain closes do not arm
  reconnection tokens because the single instance is exiting; clients should
  create or join a fresh room on another healthy instance.
- Added v3-only `Reconnected.sender_watermarks` (P10.E5). A v3 reconnect now
  carries every current room member's authoritative `(epoch, seq)` relay tail,
  allowing clients to re-baseline after missed `GameData` that is deliberately
  not replayed. The field is omitted for negotiated-v2 reconnects, preserving
  the frozen v2 wire shape.
- Added the v3-only `ProtocolInfo.transports` capability array (P10.E8). A
  negotiated-v3 handshake now advertises the current server message lane as
  `["websocket"]`, reserving the protocol seam for a future relay transport
  without another negotiation redesign. Negotiated-v2 `ProtocolInfo` frames still
  omit the field, so the frozen v2 JSON and MessagePack wire snapshots stay
  byte-identical. The contract is documented in `docs/protocol.md`,
  `docs/concepts/protocol-versions.md`, the Rust client guide, the AsyncAPI
  schema, and the v3 canonical samples.
- Added the protocol-v3 incarnation `epoch` (P10.E1) beside the relay `seq`.
  Every relayed `GameData` / `GameDataBinary` to a v3 recipient now carries an
  `epoch: u32` — a monotonic per-sender counter (tracked per connection, not
  reset on a room switch) that increments once per incarnation of the sender's
  membership (its first-ever incarnation is 1; each join-after-leave or reconnect
  increments it) while `seq` restarts at 1 within each epoch. The pair
  `(epoch, seq)` is lexicographically increasing in data-lane order, making a
  `seq` reset **self-describing** (GAP-8). Priority peer-lifecycle control may
  overtake an already-queued old-epoch tail; clients account for that tail but
  suppress it from application state after the lifecycle change. Each member's
  current epoch is also carried on the room snapshots
  (`RoomJoined.current_players[].epoch`,
  `SpectatorJoined.current_players[].epoch`, `PlayerJoined.player.epoch`,
  `PlayerReconnected.epoch`, and the `Reconnected` member snapshot) so a v3
  recipient can baseline a sender before its first relayed frame. The epoch is
  captured into the reconnection record at disconnect and resumed at
  `last_epoch + 1` on reconnect, so it stays strictly increasing across a
  sender's absence for a recipient that never left. Like `seq`, `epoch` is
  stripped for pre-v3 recipients — the v2 wire stays byte-identical (golden
  snapshots unchanged). Covered by `tests/v3_reliability_wire_golden.rs`,
  `tests/v3_game_data_sequencing_e2e.rs`, and the `src/websocket/sending.rs` unit
  tests; documented in `docs/protocol.md` and
  `spec/signal-fish-protocol.asyncapi.yaml`.
- Added the flagship `EndToEndGapAccountability` TLA+ model
  (`formal/tla/EndToEndGapAccountability.tla` + `_Small.cfg` + `_Sim.cfg`) —
  P10.D4. It composes three previously separate contracts (`SequencedRelay`
  per-`(sender, room)` stamping, `ReconnectReplay`'s bounded replay ring +
  eviction watermark, and `ConnectionTeardown`'s slow-consumer eviction) into
  one behavior and proves the client-facing promise: driven only by what the
  server puts on its socket, a client can classify every sequence discontinuity
  (`ClientCanClassify`), no evicted frame ever resurfaces out of order
  (`DroppedNeverObserved`), and the reconnection snapshot heals the tail dropped
  at eviction (`MembershipEventuallyHonest`). It is the executable proof that
  the protocol-v3 P10.E5 per-sender `(epoch, seq)` watermarks
  (`Reconnected.sender_watermarks`) are necessary — a reconnecting client
  cannot otherwise tell its own outage gap from relay loss. Three seeded bugs
  prove non-vacuity (`SingleFlagBug` and `NoBaselineResetBug` violate
  `ClientCanClassify`; `NoSnapshotReconcileBug` violates
  `MembershipEventuallyHonest`), all pinned `FALSE` in the checked configs.
- Added a bounded-simulation runner mode to `scripts/run-tla-model-check.sh`: a
  configuration whose basename ends `_Sim` is checked by `tlc -simulate`
  (`num=20000 -depth 80`) instead of exhaustive enumeration, for a state space
  deliberately too large to enumerate. It shares the module's invariants and
  still fails the run on a violation (TLC exits non-zero under `-simulate`);
  everything else stays exhaustive and CI-gating.

- Added the `DeliveryClasses` TLA+ model (`formal/tla/DeliveryClasses.tla` +
  `_Small.cfg`) — spec-first for the protocol-v3 P10.E2 delivery classes
  (reliable / latest / volatile). It pins the per-class accounting contract:
  `ReliableConservation` (reliable never coalesced), `LatestConservation` /
  `VolatileConservation` (each class only in its legitimate buckets),
  `AccountedSupersession` (every coalesced-away id is ledgered — held always,
  since the ledger write is atomic with the supersession), `LatestValueLastWrite`
  (≤1 queued latest per key; the queued rep is newest), and `ReportHonest` (the
  out-of-band `DeliveryReport` never overstates). Four seeded bugs prove
  non-vacuity — `SilentSupersedeBug`, `CoalesceReliableBug`, `MisdropLatestBug`,
  `ReportOverstateBug` (each violating its intended invariant) — all pinned
  `FALSE` in the checked config (green in the auto-globbed suite). `latest` never
  backpressures (a new-key send on a full queue drops the oldest volatile value
  or the latest arrival). The later `ScalarInPlaceBug` seed formally disproved
  the proposed per-successor `supersedes_from` scalar: interleaved A1, B2, A3
  would falsely report live B2 and could write A3 before B2. The implementation
  instead appends the successor, preserves global queue order, and publishes an
  exact `DeliveryReport` gap on the priority control lane.

- Added the `ControlPriorityDelivery` TLA+ model
  (`formal/tla/ControlPriorityDelivery.tla` + `_Small.cfg`) — spec-first for the
  protocol-v3 P10.E2 delivery revision (merged before the code). It pins the two
  properties the queue split must satisfy, composing with the #131
  `DeliveryContract` substrate: `ControlAgeBounded` (within an active recipient
  generation, control rides a separate queue drained strictly before data --
  never starved behind a data backlog) and
  the `DeliveryEventuallyResolves` liveness (a frame that sits too long triggers
  a sojourn close, so `enqueued ~> written ∨ closed` holds even against a peer
  that pings but never reads — resting only on `WF(Tick, SojournEvict,
  CloseFinish)`, never on writer fairness). Two seeded bugs prove non-vacuity:
  `SingleQueueBug` (control misrouted onto the data FIFO) violates
  `ControlAgeBounded`, and `NoSojournEvictionBug` violates the liveness. Both are
  pinned `FALSE` in the checked config (green in the auto-globbed suite);
  `PerClassConservation`, `CtrlDropsAreLoud`, and `StalenessBounded` are also
  checked.

- Added the `SenderPacingReaper` TLA+ model
  (`formal/tla/SenderPacingReaper.tla` + `_Small.cfg` / `_Boundary.cfg`) — the
  repo's first discrete-time (`now` + `Tick`) spec — formalizing BUG-2, the
  timeout inversion the config
  cross-field check prevents: a healthy sender parked on the broadcast
  `join_all` while a slow recipient drains must never be evicted by the activity
  reaper (`HealthySenderNeverReaped`). A `TimeoutInversionBug` seeded constant
  (effective grace = `ping_timeout`, the `slow = ping` boundary) reproduces the
  healthy-sender eviction for non-vacuity; the checked configs pin it `FALSE`
  and are green in the auto-globbed `scripts/run-tla-model-check.sh` suite. By
  modeling the pre-park delay (the `maybe_update_last_seen` DB write + `rooms`
  lock between the activity record and the park), the model derives that
  `slow_consumer_timeout_ms >= ping_timeout * 1000` is unsafe (same units — ms;
  `ping_timeout` is seconds) — exactly the region `validate_config_security`
  rejects — so the strict `<` is the necessary floor
  (documented in `formal/README.md` and the check's comment, with the
  not-proven-sufficient margin caveat). No behavior change to the A2 check.

- Added the `RoomLifecycleGC` TLA+ model (`formal/tla/RoomLifecycleGC.tla` +
  `_Small.cfg` / `_WindowBoundary.cfg`) formalizing the room garbage-collection
  contract behind the BUG-1 fix: a room whose members are active is never reaped
  (`ActiveRoomNeverReaped`) and a room holding an unexpired reconnection record
  is never reaped (`ReconnectWindowRespected`). A `StaleActivityBug` seeded
  constant reproduces the pre-fix behavior (both invariants violated) for
  non-vacuity; the checked configs pin it `FALSE` and are green in the
  auto-globbed `scripts/run-tla-model-check.sh` suite.

- Added split-brain seeded-bug constants to two v3 TLA+ models, making the
  single-instance boundary of the relay/reconnect contracts (ARCH-10)
  executable: `SplitBrainStampBug` in `formal/tla/SequencedRelay.tla` (a second
  instance stamps the same sender's stream from an independent counter →
  `GapAccountable` violated) and `SplitBrainCounterBug` in
  `formal/tla/ReconnectReplay.tla` (a reconnect served by a second instance that
  join-created the room fresh → `ReplayFaithful` / `StatusHonest` violated). Both
  are pinned `FALSE` in the checked configs (state spaces unchanged, still green
  in the auto-globbed suite); each spec header carries its minimal
  counterexample trace. A new "Single-instance theorems (split brain / ARCH-10)"
  section in `formal/README.md` catalogs which invariants are single-instance
  theorems and states the LB room-affinity requirement.

- Added protocol v3 (strictly additive; clamp `protocol.max_protocol_version` back to `2` —
  pure v2 — to disable, since v3 is now the current version): relayed `GameData` /
  `GameDataBinary` delivered to a v3 recipient carry a
  server-stamped per-`(sender, room)` `seq` starting at `1` and strictly increasing per sender.
  Exact prior `DeliveryReport` ranges account for intentional same-epoch gaps; pre-v3 recipients
  receive byte-identical frames with no `seq` key. Documented in
  `docs/protocol.md` ("Protocol v3 delivery reliability") and the
  AsyncAPI spec; pinned by `tests/v3_reliability_wire_golden.rs` and `tests/v3_game_data_sequencing_e2e.rs`.
- Added the v3-only `RelayStats` server message (config-gated by
  `websocket.delivery_stats_interval_secs`, default `0` = disabled, must be ≤ 3600): periodic
  per-connection cumulative delivery accounting (`sent_to_you`, `dropped_for_you`,
  `backpressure_events`) for aggregate diagnostics. These counters do not identify or
  authorize an exact sequence gap; `DeliveryReport` owns that contract.
- Added semantic WebSocket close codes (RFC 6455 private range, stable assignments): `4001
  auth_timeout`, `4002 slow_consumer`, `4003 activity_timeout`, `4004 idle_timeout` (`4000
  server_shutdown` reserved; plain unregistration closes with a normal `1000`). The code rides
  the close frame itself, so a client that never receives the best-effort farewell `Error` can
  still attribute the disconnect. Documented in `docs/protocol.md` ("Close codes"); pinned by
  `tests/close_code_semantics_e2e.rs`.
- Added `RoomJoined.reconnection_token` and `Reconnected.reconnection_token` (v3+ recipients
  only; absent on the v2 wire): the reconnection token is now minted at room join and rotated on
  every join/successful reconnect, making reconnection usable by real clients (previously the
  token was minted only after the socket closed and never reached any wire path). The token's
  claim window is still armed at disconnect (`server.reconnection_window` from the disconnect),
  so pre-issuing does not widen it.
- Added an honest `Reconnected.replay` status (`complete` | `truncated` | `unavailable`, v3+
  only) and made event replay real: room-uniform control events broadcast while a reconnection
  is pending are now captured (previously `missed_events` was always empty) and a ring-eviction
  watermark reports truncation honestly. `GameData` and the per-recipient `GameStarting` are
  deliberately never replayed. New `signal_fish_reconnection_events_evicted_total` counter.
- Added transport-layer WebSocket frame/message caps derived as `2 ×
  security.max_message_size`: grossly oversized frames now terminate the connection before the
  server buffers them (previously the library defaults allowed ~16 MiB of buffering per frame
  before the application-level check ran); messages just over the limit keep the polite
  `MessageTooLarge` error frame. Config validation now rejects `security.max_message_size = 0`
  with a direct diagnostic.
- Added delivery-conservation counters (`signal_fish_websocket_delivery_attempts_total`,
  `..._deliveries_enqueued_total`, `..._deliveries_channel_closed_total`,
  `..._deliveries_canceled_total`) carrying the invariant `enqueued + channel_closed + canceled ≤
  attempts ≤ enqueued + channel_closed + canceled + dropped`, asserted by every relay-touching e2e
  test.
- Added an extensive delivery verification stack: real-socket wedged-write/backpressure tests, a
  delivery ledger (zero-loss-or-loud-disconnect as a machine-checked predicate), a chaos TCP
  proxy (pause/throttle/fragment/RST), rate-controlled soaks, a reconnect-churn leak check, a
  concurrency schedule-explorer, real-world scenario suites, a multi-process suite (SIGSTOP /
  SIGKILL against real client and server processes), a starved-runtime conformance matrix in the
  native reference client (`--runtime`, `--tick-stall-ms`), four TLA+ models with seeded-bug
  non-vacuity counterexamples, five new Z3 proof sets, two stateful fuzz targets, and
  model-based proptests — plus a `verification-nightly.yml` CI lane.
- Added the mandatory `Ping` keepalive (with a `Pong` deadline) to both reference clients, a
  `bufferedAmount` guard and a distinct `SLOW_CONSUMER` arm to the browser reference client.

- Added two WebSocket delivery config fields: `websocket.send_queue_capacity` (default `1024`
  messages; must be ≥ 1) bounds the per-connection outbound message queue — previously
  hard-derived as `batch_size * 4` (40) — and `websocket.slow_consumer_timeout_ms` (default
  `5000`; must be > 0 and ≤ 600000) bounds how long delivery may wait on a full queue before the
  recipient is disconnected as a slow consumer. Both are documented in `docs/configuration.md`
  and pinned by the config-reference drift guards in `tests/config_and_endpoints_tests.rs`.
- Added the `SLOW_CONSUMER` error code (connection lifecycle): sent best-effort as a final
  `Error` frame before the server closes a connection whose outbound queue stayed full past
  `websocket.slow_consumer_timeout_ms`, so clients can distinguish "you could not keep up" from
  other disconnects.
- Added the `ACTIVITY_TIMEOUT` error code (connection lifecycle): sent best-effort by the
  activity reaper before it evicts a connection that produced no messages within
  `server.ping_timeout`, keeping it distinct from the socket-level `CONNECTION_IDLE_TIMEOUT`
  close.
- Added two Prometheus counters for delivery health:
  `signal_fish_websocket_backpressure_events_total` (times a full outbound queue forced delivery
  to wait for capacity; the message was still delivered) and
  `signal_fish_websocket_slow_consumer_disconnects_total` (connections force-closed because
  their outbound queue stayed full past the timeout). The existing
  `signal_fish_websocket_messages_dropped_total` help text now clarifies it counts server
  messages abandoned together with a closed or closing connection.
- Cross-platform prebuilt release binaries: `release.yml` now builds a standalone
  `signal-fish-server` executable for Linux (x86_64, aarch64), macOS (x86_64, Apple
  Silicon), and Windows (x86_64, aarch64) and attaches each as a SHA-256-checksummed
  archive to the GitHub Release, so Windows / macOS / ARM users no longer need a Rust
  toolchain or Docker. The supported target list is pinned by `REQUIRED_RELEASE_TARGETS`
  in `tests/ci_config_tests.rs`.
- Multi-architecture drift guards in `tests/ci_config_tests.rs`
  (`test_docker_publish_builds_multi_arch_manifest`,
  `test_dockerfile_cross_compiles_for_target_platform`,
  `test_release_workflow_builds_all_platform_binaries`,
  `test_release_workflow_attaches_binaries_with_checksums`) so the container manifest and
  release-binary matrix can never silently regress to a single platform again.

### Fixed

- Fixed a room-lifecycle GC bug where every room was deleted a fixed interval after **creation**
  regardless of activity (default `inactive_room_timeout` = 1 h) — `Room.last_activity` was
  written only at creation and never refreshed (both refresher methods had zero call sites), so a
  session over an hour old was reaped with players still in it, and a long-lived room that emptied
  was deleted immediately, collapsing the reconnection window (reconnects failed `RoomNotFound`
  with still-valid tokens). `last_activity` is now refreshed on join, on a real leave/disconnect,
  and once per inbound message on the throttled liveness path — covering pings, relayed `GameData`
  (JSON and binary), and WebRTC `Signal` traffic uniformly; the empty-room clock keys off
  `last_activity` (not `created_at`) so both cleanup paths agree; and room GC never deletes a room
  that still holds an unexpired reconnection record
  (`ReconnectionManager::rooms_with_active_reconnections`). Startup validation additionally rejects
  `server.heartbeat_throttle_secs >= server.inactive_room_timeout`, so the throttled refresh can
  never lag the reaper and re-open the bug by misconfiguration.
- Fixed a config timeout-inversion where a legal combination evicted a **healthy** sender: with
  `websocket.slow_consumer_timeout_ms ≥ server.ping_timeout · 1000`, a sender parked on the
  broadcast fan-out for a slow recipient could outlast the activity-reaper deadline and be closed
  `4003` before its slow recipient was ever disconnected. Startup validation now rejects that
  combination (cross-field check, guarded on `ping_timeout > 0`).
- Fixed `game_data_messages` (`signal_fish_game_data_messages_total`) reading a permanent `0`: the
  metric was exported to Prometheus but never incremented. It is now counted once per relayed
  `GameData` message at the relay funnel.
- Fixed an orphaned reconnection replay buffer (found by the new `fuzz_reconnect_tokens` fuzz
  target): a player re-registering a disconnect from a NEW room replaced its pending record and
  left the old room's replay ring alive forever — capturing control events and replaying ghosts
  for a room with nobody pending.
- Fixed a send-task race (found by the close-code e2e suite) where a connection's write loop
  ending in the same instant a close reason was requested could lose the reason — and with it
  the semantic close code — because the unbiased `select!` took the loop-ended arm; terminal
  paths now re-check the close listener non-blockingly.
- Fixed [#131](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/131): the relay
  silently dropped `GameData` under burst — each connection's outbound queue was a small bounded
  channel (`batch_size * 4` = 40 messages) written with a fire-and-forget `try_send`, so a burst
  that outpaced one recipient's drain rate discarded messages with only a metric to show for it.
  The server no longer silently drops reliable delivery: the fast path is a
  non-waiting try-enqueue, but a full reliable queue makes delivery wait (true backpressure) for up to
  `websocket.slow_consumer_timeout_ms`, and only a recipient that stays full past that timeout is
  disconnected as a slow consumer (metrics + warning log + best-effort `SLOW_CONSUMER` error
  frame, then the close). Room senders are paced to their slowest healthy recipient; a dead
  recipient costs reliable senders at most one timeout window before it is evicted. Protocol-v3
  `latest` / `volatile` omissions are instead explicit in `DeliveryReport`. Covered end to end by
  `tests/relay_backpressure_e2e.rs`; delivery semantics are documented in `docs/protocol.md`.
- Undeliverable binary game data is no longer silently dropped by the send path: a payload that
  cannot be converted for a recipient (e.g. an internal binary encoding relayed to a JSON-only
  client) now surfaces an explicit `Error` frame with code `UNSUPPORTED_GAME_DATA_FORMAT` to that
  recipient in place of each undeliverable payload. A v3 recipient first receives an exact
  `DeliveryReport` gap; the aggregate drop metric still increments.
- Hardened release and documentation validation: `release.yml` now skips binary
  attachment with a clear diagnostic when no `release-binary-*` artifacts exist,
  uses an existing `actions/download-artifact` tag, and the workflow hygiene job
  can verify pinned GitHub Action tags exist upstream. Documentation version
  scans now prune vendored `third_party/` Markdown while rejecting unsyncable
  versionless `signal-fish-server` dependency examples.
- Fixed [#122](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/122): the
  published `ghcr.io/ambiguous-interactive/signal-fish-server` image was built only for
  `linux/amd64`, so pulling it on ARM64 Linux (AWS Graviton, Ampere, Raspberry Pi, Apple Silicon
  under Docker) failed with `no matching manifest for linux/arm64/v8`. `docker-publish.yml` now
  builds a single multi-architecture manifest covering `linux/amd64`, `linux/arm64`, and
  `linux/arm/v7`. The Rust binary is cross-compiled natively on the build runner (the heavy
  compile never runs under emulation; only the trivial runtime-image setup does), and the
  `Dockerfile` resolves the cross toolchain per target arch (build-host-agnostic via
  `$BUILDARCH`).

### Changed

- **Consolidated the pre-release protocol into a single v3.** The
  delivery-reliability features that were briefly developed behind a separate
  `protocol_version: 4` (server-stamped `GameData.seq` + incarnation `epoch`, and
  the opt-in `RelayStats` frame) now negotiate under **v3** — there is no v4. v3
  is the single unshipped/mutable "current" version (WebRTC signaling AND
  delivery reliability), additive over the frozen v2 floor.
  `SERVER_MAX_PROTOCOL_VERSION` and the `protocol.max_protocol_version` default
  are now **3**; a client that still advertises `4`/`5` is clamped to 3. v2
  remains byte-frozen (the pre-v3 forms are unchanged). Net effect for consumers:
  a client obtains the reliability surface by negotiating v3 rather than v4. The
  `[Unreleased]` entries that referenced "v4" describe these same features as
  they now ship — under v3.
- The activity reaper (`ConnectionManager::collect_expired_clients`) and the heartbeat-update
  throttle (`ConnectionManager::should_update_last_seen`) now read the Tokio runtime clock
  (`tokio::time::Instant`) instead of `std::time::Instant`. Production behavior is unchanged —
  outside a paused runtime `tokio::time::Instant` wraps the same monotonic std clock — but tests
  can now drive these windows deterministically with `tokio::time::advance()` under
  `#[tokio::test(start_paused = true)]` at zero wall-clock cost. The `heartbeat.rs` reaper test
  that previously relied on a real 25 ms sleep is now paused-clock and instant.
- Raised `security.max_connections_per_ip` default `10` → `24`. Ten silently refused the 11th
  concurrent connection from one IP, so a 16-player session behind a single NAT (LAN party,
  office, venue) could not connect. `24` covers 16 players plus spectators and reconnect churn.
  (The per-room player count is bounded separately by that room's `max_players`, default `8`.)
- SDK platform/version enforcement (`protocol.sdk_compatibility.enforce`) now defaults to
  `false` (opt-in): with the old default the prepopulated platform list made a default-config
  server reject every client that did not claim to be a known engine — including
  `platform: None` and custom/Rust clients. Deployments shipping engine SDKs re-enable it
  explicitly.
- `protocol.max_protocol_version` now defaults to `4`.
- The `PlayerReconnected` notification fan-out is concurrent (bounded by the slowest recipient)
  instead of serial per recipient.

## [0.3.0] - 2026-06-20

### Added

- Added `tests/docs_site_consistency.rs`, a documentation-accuracy regression guard that ties the
  published docs to source: it asserts `docs/reference/error-codes.md` documents every `ErrorCode`
  variant, `docs/protocol.md` documents every `ClientMessage` / `ServerMessage` variant and the
  user-facing wire enum tokens (all parsed from `src/protocol/`), the public docs carry no
  stale/removed protocol tokens (including the `relay_type: "WebRTC"` value drift), and every
  intra-`docs/` `#anchor` link resolves identically on both GitHub and the MkDocs Pages site — the
  two renderers slugify headings containing `/` differently, so an anchor hand-written for one
  silently 404s on the other.
- Added a [Platform Integration Guide](docs/guides/platform-integration.md):
  per-platform WebRTC-stack guidance for browser, native desktop, mobile, Steam,
  Godot, Unity, and Unreal, plus the universal v3 client contract (relay floor, opaque
  matchbox-shaped signal payloads, the two-channel data layout, stateless glare resolution) and
  the cross-stack interop traps (Chrome/Safari `.local` mDNS candidates, SCTP `a=sctp-port` vs
  legacy `sctpmap`, DTLS/BUNDLE, the no-raw-UDP-in-browsers constraint). Wired into the MkDocs
  nav, the docs landing page, and the Handoff & Topologies "See also". The browser and native
  rows are demonstrated end to end by the in-repo reference clients; the mobile, Steam, and
  engine rows are integration notes for out-of-repo builds.
- Added ICE pre-gather on `RoomJoined` / `Reconnected` (the deferred "RoomJoined ICE
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
- Added the browser reference client as the in-repo standalone npm package
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
- Added the native Rust reference client as the in-repo standalone package
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
  relay fallback, late-join full-plan seat-fill refreshes, and mixed v2/v3 relay-floor rooms. Wired
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
  to each v3-capable member after the unchanged `GameStarting`. The selection ladder is
  `mesh+webrtc` → `host+webrtc` → `host+direct` → `relay` floor, where any member lacking the
  required capability (or a disabled transport) downgrades the whole room to relay; host election
  prefers the authority, else the earliest joiner (smaller UUID tie-break); each recipient's
  `peers[].initiate` is set by the deterministic glare rule (mesh: lesser UUID offers; host:
  clients offer to the host, the host offers to none). A room that resolves to the relay floor
  sends every v3 member an explicit no-peer `relay`/`relay` plan; v2 members receive no plan and
  remain byte-identical. Initial pairing is delivered exclusively by `SessionPlan` at finalize.
  A join or reconnect into an already-`Finalized` room refreshes every current v3 member with a
  complete per-recipient plan while retaining the room's sticky topology and transport. `NewPeer`
  remains a decodable compatibility wire shape but is not emitted as the current membership-delta
  contract. This supersedes the P2 behavior where `NewPeer` fired on every lobby-fill join. Added
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
  counter. The message is purely informational -- reported P2P state never disables `GameData`
  relay, although delivery failures can still close the physical socket loudly. Added Prometheus counters for the v3
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
  `ServerMessage::Signal { from, signal }`. The additive
  `ServerMessage::NewPeer { peer_id, you_initiate }` compatibility shape was introduced with this
  phase; current finalized-room membership changes use complete `SessionPlan` refreshes instead.
  The deterministic glare rule (lesser UUID initiates) designates exactly one offerer per mesh
  pair; P3's host topology fixes this direction for star sessions (the client offers, the host
  answers). Same-room enforcement, WebRTC-transport negotiation, and a
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
  tie-break), and when no member qualifies the stored plan is dropped — the session is over and
  the relay floor carries the room. A join/reconnect membership event still publishes an explicit
  relay plan to every current v3 member; a departure alone emits no replacement plan. Re-issued
  membership-refresh plan peer lists
  are capability-filtered on both sides by the same predicate: `peers[]` names only members that
  negotiated the session's sticky topology/transport, so a v3 member that did not (e.g. a
  relay-only seat-filler, or one with the WebRTC transport but not the session's topology)
  receives its (v3-gated) plan with an empty peer list and participates
  via the relay floor (`host` stays as elected, informational), and capable members never see it
  listed, so clients are never instructed to attempt pairs the plan itself excludes (or that
  `Signal` validation would reject); at finalize the filter is vacuous because plan selection
  requires every member to support the plan. After a late join or reconnect, every current v3
  member receives its own complete tailored `SessionPlan` (current peers, glare-correct `initiate`
  flags, stored host, fresh ICE). The latest plan authoritatively replaces prior peer state;
  `NewPeer` is retained only as a compatibility wire shape. Topology/transport are sticky for the
  session lifetime (the selection ladder runs once at finalize and is never re-run mid-session). A
  membership event that heals an invalid host is served by the re-plan to every v3 member. No new
  message types and no wire-shape changes; all emission stays v3-gated. Added Prometheus counters
  `signal_fish_transport_session_replans_emitted_total` (one per host re-plan event — departure
  failover or late-join self-heal; not moved when no member qualifies and the plan is dropped)
  and `signal_fish_transport_session_plans_late_join_total` (one per joiner that received a
  late-join plan; a heal-served joiner counts on the re-plan event instead);
  `signal_fish_transport_session_plans_emitted_total` means one finalization publication for a
  room with v3 recipients, including the relay floor, and TURN credentials minted by
  re-plans/late-join plans count toward the
  existing `signal_fish_transport_turn_credentials_issued_total`.
- Added a protocol v3 multi-peer (N≥3) signaling conformance suite
  (`tests/v3_multipeer_e2e.rs`): full-lobby flows over real WebSockets pinning the global mesh
  glare matrix at N=3/N=4 (every unordered pair has exactly one offerer — the smaller UUID — and
  pairwise opaque signals relay byte-identically across all ordered pairs), the strict N=4 host
  star property (clients offer only to the host and never appear in each other's plans), the mixed
  v2+v3 relay floor (`GameStarting` for everyone, explicit relay plans for v3 only),
  N=4 host-failover re-planning (one fresh star-correct plan per survivor naming the
  earliest-joined remaining member) plus an N=4 cascade variant (two consecutive host deaths, each
  wave re-electing and re-issuing from the surviving session state), seat-filling late join into a
  live mesh session (complete plan refreshes for the joiner and every v3 incumbent), and the full
  wire reconnect flow (`Reconnected` / `PlayerReconnected` + complete plan refreshes,
  post-reconnect signals under the restored player id).
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

- `--print-config` now redacts secrets (security hardening). The printed JSON
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
  (`wss://` for signaling in production — DTLS fingerprints travel in SDP, so
  plaintext `ws://` signaling allows man-in-the-middle of the WebRTC peer connections). Emitted
  after logging initialization via `tracing::warn!`; deliberately a warning and never a hard error
  because reverse-proxy TLS termination (where `security.transport.tls.enabled` stays `false`) is
  the common production deployment (`config::should_warn_missing_signaling_tls`).
- Removed unmaintained `rustls-pemfile` dependency (RUSTSEC-2025-0134); PEM parsing now uses
  `rustls-pki-types` built-in `PemObject` trait.
- **Security review — three hardening fixes from an adversarial audit of the
  v3 signaling surface:**
- Bounded the v3 `TransportStatus` → `PeerTransportStatus` room fan-out. An
  accepted `TransportStatus` state change fans out 1→N to the reporter's room; it was the one
  client-triggered v3 control-plane emit path with no rate limit (the dedup gate is trivially
  defeated by alternating `connected`), unlike the targeted `Signal` relay. The accepted-change
  fan-out now consumes the same per-connection WebRTC control-plane budget as `Signal`
  (`rate_limit.max_signals`); over-budget changes are dropped silently (the message is informational
  and defines no error reply). The gate sits after the p2p/relay observability counters, the no-room
  early return, and recipient resolution, so accurate metrics are never suppressed and a room-less
  reporter, failed room-member lookup, or status report with no v3 room peers spends no budget;
  the per-connection transport state is always recorded regardless of the budget. (The dominant
  relay-floor `GameData` fan-out is intentionally bounded by other means — `max_message_size`,
  connection/room caps, best-effort sends — so this only closes the control-plane consistency gap
  with `Signal`.) Covered by `transport_status_fanout_is_bounded_by_signal_budget`.
- Reject zero-valued background-task interval configs at startup instead of silently killing the
  task at runtime (resource-exhaustion). `server.room_cleanup_interval`,
  `rate_limit.time_window`, and (when `websocket.enable_batching` is true) `websocket.batch_interval_ms`
  each become the period of a `tokio::time::interval`, which **panics** on a zero period — previously
  this killed the spawned task while the process kept serving, so a one-line operator typo silently
  disabled the maintenance sweep (unbounded room/client/token/lock growth), the rate-limiter cleanup
  (and the rate-limit windows themselves), or every connection's batch flush. `validate_config_security`
  now rejects these at startup with a field-named error. **Behavior change:** a config with one of
  these set to `0` that previously started (degrading silently) now fails fast at startup. As
  defense-in-depth for direct library construction (the server is part of the public API and may be
  built without running config validation), the three interval use sites also clamp to a non-zero
  floor, mirroring the existing dashboard-cache `.max(..)` zero-guard.
- Consolidated all secret comparison into a single constant-time helper and closed two
  non-constant-time compares. New crate-internal `security::constant_time_eq`
  (over `subtle`) is now the sole secret-comparison implementation, replacing two prior copies
  (`auth::middleware` and `security::token_binding`). The reconnection-token check
  (`reconnection::{validate,claim}_reconnection`) and the metrics bearer-token check
  (`websocket::metrics`) previously used short-circuiting `String`/`&str` `==`/`!=`, leaking via
  timing how many leading bytes matched; both now route through the constant-time helper, consistent
  with the app-secret path. (Exploitation was already impractical — the reconnect token is a
  122-bit v4 UUID — so this is a consistency/defense-in-depth hardening, not a fixed vulnerability.)

### Fixed

- Fixed a class of CI failure where a root version bump silently invalidated the committed
  `clients/native` lockfile: it kept pinning the previous `signal-fish-server` version, so the
  `--locked` Browser Interop and WebRTC Interop builds failed with a cryptic "lock file needs to be
  updated" error after a multi-minute cold build. The lockfile is regenerated, and a new offline
  guard (`tests/workspace_lockfile_consistency.rs`) now fails fast in the always-on test suite if
  any git-tracked lockfile drifts from the root crate version — replacing that late, confusing
  failure with an instant, actionable one.
- Hardened the timing-sensitive end-to-end tests against CPU starvation on oversubscribed CI
  runners (the rare flake the `nextest`/`msrv`/`coverage` lanes hit then passed on re-run): the
  multi-process restart spawn now retries its fixed port with exponential backoff, the
  idle-timeout tests scale their window via `SIGNAL_FISH_TEST_TIMEOUT_MULTIPLIER` on the
  non-isolated `msrv`/`coverage` lanes, and redundant fixed startup sleeps were removed from the
  in-process server harnesses (also trimming suite wall-clock).
- Fixed the noisy `##[error]ENOENT` annotations that every full-test-suite CI job emitted (the
  `nextest`, `msrv`, and `coverage` jobs in `ci.yml`, plus `asan` in `ci-safety.yml`). Those jobs
  compile the `trybuild` UI test, which materializes a nested `<target>/tests` cargo workspace at
  test-run time; `Swatinem/rust-cache`'s restore-time cleanup then probes `<target>/tests/target`
  (a path trybuild never creates — it only writes `<target>/tests/trybuild`) in a non-awaited call,
  so the `opendir` rejection surfaces as an unhandled-promise `##[error]ENOENT` whenever a restored
  cache contains that directory. Each such job now drops `<target>/tests` before the post-run cache
  save, keeping the trigger out of the cache. This replaces the prior ineffective workaround (a
  `ci-safety` target-dir relocation and cache-epoch bump that never removed the trigger, so the
  noise persisted) and corrects the comment and test that falsely credited that epoch with
  preventing it. A new structural guard,
  `test_jobs_running_trybuild_under_rust_cache_drop_nested_target_dir`, parses every workflow and
  fails if any job that combines a `Swatinem/rust-cache` step with a full-suite test run omits the
  cleanup, so the whole class cannot silently regress as jobs are added.
- Fixed `JoinRoom` validation to report dedicated error codes for player-name and capacity failures:
  an invalid `player_name` now returns `INVALID_PLAYER_NAME` and an invalid `max_players` now returns
  `INVALID_MAX_PLAYERS`, instead of the generic `INVALID_INPUT`. Both codes were already defined and
  documented but never emitted, so a client switching on `error_code` could not distinguish a bad
  player name or capacity from any other malformed input (the sibling `game_name` / `room_code`
  validators already used their dedicated codes). Surfaced by a new red-green edge-case suite
  (`tests/v3_edge_cases_e2e.rs`).
- Fixed the Rust client guide's `GameDataEncoding` examples to match `ProtocolInfo.game_data_formats`:
  `rkyv` remains reserved/internal and is not advertised or negotiated by the server.
- Fixed widespread protocol-documentation drift found while reconciling the v2/v3 docs against
  source (a 52-finding audit). The embedded-server examples in `README.md` and
  `docs/library-usage.md` now call `EnhancedGameServer::new` with its real 11-argument signature
  (the v3 `session_config` / `turn_config` were missing) and read `ServerConfig` fields from the
  correct config sub-structs (`cfg.rate_limit` / `cfg.security` / `cfg.websocket`), so they
  compile. Corrected the `/metrics` response to its actual camelCase nested shape and documented
  metrics auth accurately (a single shared `security.metrics_auth_token` bearer token — and a hard
  startup error when `require_metrics_auth` is set without it — not an `app_id:app_secret` pair).
  Fixed the `supports_authority` default (it is `true`, so a freshly created room shows the creator
  as the authority) and the matching `is_authority` flags across the `RoomJoined` / `Reconnected` /
  `GameStarting` examples; corrected `relay_type` example values to the real default `"matchbox"`
  (v3 transport is carried by `SessionPlan.transport`, not `relay_type`); the `GameData` example to
  the double-nested `{"type":"GameData","data":{"data":...}}` envelope; the clean room-code alphabet
  to 32 characters; the per-app rate limit as per-`app_id` (not per-IP); the failed-`Authenticate`
  handler to the `AuthenticationError` message and `INVALID_APP_ID` code; and the spectator,
  reconnection-token, and event-buffer descriptions to match server behavior. Added the Protocol v3
  surface (transports/topologies, `SessionPlan`, targeted `Signal` relay, ICE pre-gather,
  self-hosted TURN credential minting, idle timeout) to the `docs/features.md` and
  `docs/architecture.md` overviews, and fixed cross-references that resolved on GitHub but 404'd on
  the MkDocs site by rewording the `TURN / STUN` and `ICE / TURN` headings so both renderers produce
  the same slug.
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
  player filled the seat, wrongly push clients of a relay session into WebRTC negotiation. Late
  join and reconnect now read the stored active session plan: an absent stored plan means the
  sticky relay floor, so every current v3 member receives an explicit no-peer relay plan while
  v2 members receive no plan; topology/transport are never re-selected mid-session.
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
- Fixed finalized-room membership updates deriving incremental WebRTC pairing from topology alone.
  A join/reconnect now republishes one complete per-recipient `SessionPlan` to every current v3
  member, so `host+direct` carries no ICE or WebRTC instructions, relay-floor rooms explicitly reset
  stale peer state, and WebRTC peer lists use the same capability predicate as `Signal` validation.
  `NewPeer` remains decodable for compatibility but is no longer the production membership delta.
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
  every current v3 member receives a fresh authoritative `SessionPlan`, and the reconnector keeps
  its player identity for subsequent WebSocket frames and disconnect cleanup.
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

- Upgraded the dependency tree to current releases. The only direct major bump is `tower-http`
  0.6 → 0.7 (the CORS/trace middleware the server mounts); `bytes` moved 1.11 → 1.12 and every
  other crate advanced to its latest semver-compatible version via a lockfile refresh. The root and
  reference-client lockfiles were regenerated together so the `--locked` CI, interop, and fuzz
  builds all resolve the same graph.
- **Lobby start is now explicit and `max_players` is a ceiling, not a required count.** In both
  protocol v2 and v3 the game no longer starts automatically when every player is ready, and a room
  no longer needs to be full to start. Players may `PlayerReady` / unready at any time while the room
  is open (any non-`Finalized` state), and the room is finalized only by a new explicit
  [`StartGame`](docs/protocol.md) client message. `StartGame` is accepted only when **every current
  player is ready** and the sender is authorized: the room's authority player if the room has one,
  otherwise **any** member. A single ready player may start (solo is allowed). On success the server
  broadcasts the unchanged `GameStarting` followed by a per-recipient `SessionPlan` for every v3
  member (including an explicit relay-floor plan) — only the trigger changed. A departure no longer regresses a
  partially-full lobby back to `Waiting`; the remaining players keep their readiness and can still
  start. Rejected `StartGame`s return the new `GAME_START_NOT_READY` / `GAME_START_FORBIDDEN` error
  codes (an already-started room returns `INVALID_ROOM_STATE`).
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

## [0.1.1] - 2026-02-23

### Changed

- Updated locked dependency patch releases and expanded CI, release, hook,
  and repository-policy validation.

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

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0

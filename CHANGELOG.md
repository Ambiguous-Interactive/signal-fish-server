# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Public Rust API: expose `server::run_drain_choreography`, the full
  post-signal drain sequence (begin, `GoingAway` fan-out, grace wait with an
  idle fast path, coded `4000` closes, handler settle), so embedded servers
  running their own accept loop reuse the exact shutdown choreography of the
  shipped binary instead of dropping clients abruptly. Also public in
  support: `EnhancedGameServer::has_active_socket_tasks` and
  `config::defaults::RELAY_ENVELOPE_HEADROOM_BYTES` (issue #396).
- Reference clients: add a creator-only `--max-players` flag (native and
  browser, mirrored) that decouples room capacity from the `--peers` ready
  barrier. Capacity above the barrier leaves open seats after the room
  finalizes, so a late joiner can seat-fill the running session without any
  prior departure; the flag is rejected in join mode and below `--peers`
  (the ready barrier could never be reached). New interop scenarios exercise
  the open-capacity seat fill end to end on the v2 relay floor — native
  drivers, and the browser as the creating driver — pinning that the joiner
  enters the `finalized` room, `GameStarting` is never re-broadcast, and no
  `Error` frame surfaces, guarding the issue #449 stale-latch class: any
  post-finalize `StartGame` re-issue would draw `INVALID_ROOM_STATE` and
  appear there (issue #451).
- Pin the host-election/readiness residuals of the issue #396 sweep with
  deterministic, mutation-verified tests: the coordinator's `StartGame`
  rejections (`NotReady`/`Forbidden`/`AlreadyStarted`) reach the wire with
  their exact, distinct error codes and mutate nothing; in-flight
  `PlayerReady`/`StartGame` handlers parked on a lifecycle renamed away by a
  reconnect reclaim are silent no-ops; an aborted host re-plan publication
  retains the sticky plan naming the departed host until one subsequent
  departure heals it; and refreshed finalized-join plans after a mid-game
  authority departure name no phantom authority (issue #447).
- Document that `all_ready` in `LobbyStateChanged` is a toggle-driven advisory
  snapshot: a player who joins later is always unready and triggers no
  corrective broadcast, so a cached `all_ready: true` is stale after any
  `PlayerJoined`; the authoritative readiness gate is the `StartGame` re-check
  (its `GAME_START_NOT_READY` rejection is exact), and clients re-issue
  `StartGame` when every current player is ready again. Pin the staleness,
  rejection, and both recovery paths (latecomer readiness toggle, latecomer
  departure) end to end (issue #447).

- Pin four gameplay-seam guards surfaced by the issue #396 sweep with
  deterministic, mutation-verified tests: reconnection expiry sweeps never
  remove a record whose reconnection is claimed mid-flight; an interleaved
  control frame never consumes an armed protocol v3 `Latest` acceleration
  window; the documented spectator-cap boundary stays strict (`<`, a full room
  refuses without mutating its roster, and a departure frees exactly one
  seat); and spectator maintenance retries void stale rollback rows for
  republished identities instead of deleting live roster entries. Clarify that
  a future spectator cap would refuse before any room mutation through the
  terminal `SpectatorJoinFailed` response carrying `TOO_MANY_SPECTATORS`
  (verified behavior; no functional change) (issue #396).
- Pin two eviction-scan guards of the v3 data lane with deterministic
  regression tests: coalescing matches the full
  `(from_player, room_id, key)` composition, so an equal application key from
  another player or room never supersedes another stream's queued value, and
  both the supersede search and the volatile-victim search see only
  current-generation rows after a room transition, so stale frames can neither
  be superseded nor evicted and a saturated lane reports
  `LatestDroppedFull` instead (verified behavior; no functional change)
  (issue #396).
- Add a running-session capability gate for seat-fill joins into finalized
  rooms: a joiner that did not negotiate the room's sticky session pair
  (protocol v3 plus its topology **and** transport) is rejected with a new
  `ROOM_SESSION_INCOMPATIBLE` wire code instead of being seated on a silently
  split data path, where its WebSocket-relayed traffic reaches the room while
  peer-to-peer traffic between capable members never reaches it. Relay-floored
  rooms stay open to everyone (the floor never closes), reconnects of seated
  incumbents are unchanged, and every plan publication now observes residual
  mixed-path memberships (a drifted reconnect) via the new
  `seat_fills_rejected_incompatible` and `mixed_path_members_observed`
  transport metrics plus a warn log (issue #421).
- Pin the teardown-behind-an-abandoned-write wire contract with an end-to-end
  regression test on a real upgraded socket: a cancelled mid-flush write must
  still deliver every already-buffered byte whole, keep later frames on clean
  frame boundaries, and preserve the coded close handshake (verified behavior;
  no functional change) (issue #415).
- Add a startup warning when token binding is enabled but optional without an
  effective built-in TLS listener: over plaintext `ws://` the v2 connection
  key is publicly derivable, so proofs provide replay ordering only, not
  authentication. Required mode already refused to start in that configuration.
- Add a configurable aggregate outbound WebSocket payload limit, discoverable
  before connection through the CORS-enabled `/v2/client-config` and
  `/v3/client-config` endpoints and also advertised in the HTTP upgrade response
  and negotiated-v3 `ProtocolInfo`. Bounded serialization rejects oversized
  text, binary, snapshot, and replay messages whole and closes the affected
  connection with RFC 6455 code `1009` instead of truncating them; binary
  fallback decoding is separately bounded against compact-tree allocation
  amplification (issue #399).
- Add negotiated protocol-v3 room-operation correlation. Clients that request
  and receive the `room_operation_ids` capability can wrap join, leave,
  reconnect, spectator-join, and spectator-leave commands with a UUID and
  receive exactly matched terminal results without changing legacy v2/0.4/0.7
  wire shapes. Unexpected owned-task failures also return a correlated internal
  failure while the connection remains deliverable (issue #395).
- Pin two previously unpinned game-data guards with deterministic tests: a
  pre-v3 sender supplying delivery metadata (`class`/`key`) is rejected with
  `INVALID_DELIVERY_CLASS`, and every game-data write from an unseated
  connection is answered — nothing can regress to silent vanishing without
  failing these (verified behavior plus the coded rejections above)
  (issue #396).

### Changed

- Truthful rate-limit documentation (issue #396 session-187 audit): the
  `Authenticated.rate_limits` projection and its rustdoc now state that
  `per_minute` handshake limiting is enforced only for allowlist entries that
  configure an explicit `rate_limit_per_minute` — omitting the field is the
  "unlimited" configuration, the advertised 1000/60000/1440000 numbers are
  projections only, and unknown-ID rejections consume no budget. Public docs
  were already qualified ("enforced when configured"); the internal rustdoc
  overclaimed and is corrected. A paired data-driven test pins both halves of
  the contract (advertised projection AND no enforcement) so a future change
  to either side must consciously update it.
- Reject contradictory message-cap pairings at startup: a
  `security.max_message_size` above `security.max_outbound_message_size`
  previously started and then admitted relayed game data that could not be
  re-emitted, closing every recipient with `1009 outbound_message_too_large`
  — silent total rejection for exactly the traffic the deployment appeared to
  accept. The pairing now fails both top-level config validation and direct
  library construction of `EnhancedGameServer` (which likewise enforces the
  dead-config `max_signal_bytes ≤ max_message_size` pairing), naming the
  contradictory knobs (issue #396).
- Reject game data from unseated connections with `NOT_IN_ROOM` instead of
  dropping it silently: JSON `GameData` and raw binary game data sent before
  joining, while spectating (spectator connections are never seated), or
  after losing seat during teardown now surface the same coded error as
  every other command surface, so clients can distinguish "relayed"
  from "dropped" and retry meaningfully. Per-frame validation still runs
  first (`INVALID_DELIVERY_CLASS` / `MESSAGE_TOO_LARGE`).
- Accept-but-drop honesty for legacy peer metadata:
  `ProvideConnectionInfo` now returns `INTERNAL_ERROR` when its durable
  write lands on a membership row that vanished mid-flight instead of
  silently pretending success, matching the treatment of any other failed
  persistence; the room-creator rename path likewise mirrors its database
  result into in-memory state only on confirmed success.
- **Breaking:** Remove never-wired metric exports that always reported a
  healthier-than-real server: `signal_fish_queries_total`, the
  `signal_fish_room_creation_latency`/`signal_fish_room_join_latency`/
  `signal_fish_query_latency` histogram families, and the
  `signal_fish_errors_*` family had no production increment sites since
  inception, so they permanently exported zeros/nulls (and the JSON
  `performance`/`errors` objects did the same), making error-rate alerts and
  `created − deleted` style invariants impossible to satisfy honestly. The dead
  `ServerMetrics::health_status()`/`OperationTimer` APIs, whose failure-rate
  math was built from those never-incremented counters, are removed with them.

- **Breaking:** Close the remaining exported-but-unwired metric families
  (issue #434, completing the #396 metrics-truthfulness sweep). The
  distributed-lock cleanup counters (`signal_fish_distributed_lock_cleanup_runs_total`,
  `signal_fish_distributed_lock_cleanup_removed_total`) are now wired to the
  real maintenance sweep, and release failures
  (`signal_fish_distributed_lock_release_failures_total`) now count every stale
  or failed lock release on the admission paths instead of being discarded.
  The public `DistributedLock::release` result is now defined as `true` only
  when it removes an active matching-token lease; the in-memory backend
  reclaims an expired matching entry but returns `false`, so callers must not
  interpret expiry cleanup as a successful owned-lease release.
  Removed series whose producers cannot exist in this product:
  `signal_fish_distributed_lock_extend_failures_total` (no production lease
  extension path), `signal_fish_relay_client_id_reuse_total` and
  `signal_fish_relay_client_id_exhaustion_total` (the relay server was removed),
  the JSON-only `relay_health` snapshot object including its unwired
  `session_timeouts` field, and the cross-instance membership-cache pair
  `signal_fish_cross_instance_membership_cache_{hits,misses}_total` (no such
  cache exists in the shipped in-memory backend; the remaining reserved
  remote-coordination seam series keep their explicit "Reserved" labeling).
  Raw JSON consumers must also drop `distributed_lock.extend_failures`,
  `cross_instance.membership_cache_hits`,
  `cross_instance.membership_cache_misses`, and `relay_health`; the matching
  public `ServerMetrics` fields/increment methods, snapshot fields, and
  `RelayHealthMetrics` type are removed.

- **Breaking:** Startup validation now rejects the remaining "zero silently
  kills an enabled feature" configuration seams (issue #431, following the
  #430 cap sweep): `server.reconnection_window = 0` issued every reconnection
  token already expired, silently disabling reconnection while
  `server.enable_reconnection` read true, and `websocket.batch_size = 0` with
  `websocket.enable_batching = true` was clamped to one by the receive path,
  flushing on every message and silently disabling the enabled batching.
  Both now fail startup with a direct diagnostic naming the field; deliberate
  disable keeps its dedicated switch (`server.enable_reconnection=false`,
  `websocket.enable_batching=false`). Default configurations are unaffected.
- **Breaking:** Startup validation now rejects zero-valued total-rejection
  caps with a direct diagnostic instead of silently admitting no one:
  `security.max_connections_per_ip = 0` rejected every WebSocket registration
  with `IpLimitExceeded` while reading like the conventional "unlimited" value
  (issue #430). The same failure class is closed for `server.max_rooms_per_game`,
  `rate_limit.max_room_creations`, `rate_limit.max_join_attempts`,
  `rate_limit.max_signals`, `protocol.max_game_name_length`,
  `protocol.max_player_name_length`, and `protocol.max_players_limit`, and for
  per-app allowlist overrides (`security.allowed_apps[*].max_rooms`,
  `.max_players_per_room`, `.rate_limit_per_minute`) where zero silently
  rejects every creation, join, or authentication for exactly that app; each
  field's documentation now states the `> 0` requirement. Default
  configurations are unaffected.
- **Breaking:** Remove the never-wired cross-instance deduplication seam. The
  `DedupCache` had zero production callers, its `coordination.dedup_cache`
  configuration block (and `SIGNAL_FISH__COORDINATION__DEDUP_CACHE__*`
  environment variables) was parsed but consumed by nothing, and the
  `signal_fish_cross_instance_dedup_{hits,misses,evictions}_total` Prometheus
  series plus the `MetricsSnapshot.cross_instance.dedup_cache_*` fields
  exported permanent zeros. The module, its configuration types, the
  always-zero counters, and the `lru` dependency that existed only for it are
  gone. Config files carrying a `dedup_cache` block still parse (unknown
  fields are ignored); embedders referencing the removed public types must
  drop those references.
- **Breaking:** The public coordination delivery entry point
  `deliver_or_disconnect` now takes `&Arc<ServerMetrics>` instead of
  `&ServerMetrics` (the `_in_room` variants are crate-internal and changed
  identically). Embedders already holding the server's metrics Arc pass it
  through unchanged; drop-path accounting needs the owned handle so a
  cancelled parked delivery resolves the same counters as ordinary outcomes
  even on queues without an embedded metrics handle.
- **Breaking:** Bind the `EncryptedSecret` metadata (`key_id`, `created_at`) of
  `EnvelopeEncryptor` as AES-256-GCM associated data. Bundles encrypted by a
  previous version no longer decrypt — they carry no authenticated metadata —
  so re-encrypt any persisted secrets when upgrading (no in-repo caller
  persists bundles today; this is an embedder-facing surface). Tampering with
  either metadata field now fails decryption instead of succeeding with
  attacker-chosen labels.
- **Breaking:** Remove `MetricsQuery`'s parsed-but-ignored `timeRange`
  parameter and stop echoing it in the `/metrics` response body. Every
  reported metric was already a lifetime-cumulative total, so the echoed
  window string could only mislead window-expecting dashboards; clients can
  filter `dashboardCache.history` samples by `fetchedAt` client-side.
  Unknown query parameters remain accepted and ignored.
- **Breaking:** Remove the dead public `EnhancedGameServer::admin_user_exists`
  wrapper and its always-false `GameDatabase::admin_user_exists` trait
  method, which suggested an admin-account seam that does not exist and
  hard-coded `false` in every trait implementation.
- **Breaking:** Change `RaceConditionMetrics.retry_success_rate` and
  `room_code_retry_success_rate` to report `null` while zero attempts have
  been recorded instead of a fabricated `1.0` (100%), which alert thresholds
  like `< 0.9` previously read as healthy for servers that never retried.
  Strict clients typing these fields as non-optional numbers must accept
  `null`.
- **Breaking:** Wire `MetricsSnapshot.dashboard_cache.refresh_count` to the
  real successful refresh counter and remove the hardcoded-zero
  `refresh_errors` stub field beside the live `refresh_failures` counter it
  contradicted.
- Route the conventional top-level `/health` path to the real health handler
  in the production router instead of falling into the 200-OK catch-all
  banner that ignored backend state; `/v2/health` remains equivalent.
- Advertise player/game name length limits as UTF-8 bytes everywhere:
  validation error messages now say "bytes", and `PlayerNameRulesPayload`
  documentation states the unit, matching the byte-measured server checks.
  An 11-character CJK name (33 bytes) still exceeds the default 32-byte
  limit, as it always has; only the wording changed.
- Document the deliberate absence of spectator-name uniqueness: spectator
  names are non-authoritative display metadata on an unbounded admission
  surface, so enforced uniqueness would enable name-squatting denial of
  service against spectators (and, across roles, pre-claiming that blocks
  real players from joining).
- Throttle unauthorized-metrics-access warnings to one per 60 seconds with a
  suppressed-repeat count, so anonymous request loops against `/metrics*`
  can no longer amplify operator log-disk volume before any credential guess
  matters.
- Throttle rejected-WebSocket-upgrade warnings to one per source address and
  outcome per 60 seconds with a suppressed-repeat count, so anonymous request
  loops against the unauthenticated `/v2/ws` and `/v3/ws` upgrades can no
  longer amplify operator log-disk volume. The first warning for a source
  keeps its exact previous field set (`request_id`, `peer_ip`, `outcome`,
  `http_status`), distinct addresses keep warning independently, tracked
  sources stay bounded under rotated-address floods, and response headers and
  status codes are unchanged (issue #411).
- **Breaking:** Add `max_outbound_message_size` to the public Rust
  `SecurityConfig`, `ServerConfig`, and `ProtocolInfoPayload` structs.
  Downstream struct literals must supply the field or use struct update syntax.
  `ServerMessage::ProtocolInfo` now stores its payload in a `Box`, so downstream
  constructors must use `ServerMessage::ProtocolInfo(Box::new(payload))` and
  moved pattern matches receive `Box<ProtocolInfoPayload>`.
  Valid configured values are `1..=67108864` bytes so the advertised integer is
  portable across JavaScript and 32-bit clients.
- **Breaking:** Add `requested_capabilities` to the public Rust
  `ClientMessage::Authenticate` variant and correlated room-operation variants
  to the public client/server message enums. Downstream struct-variant
  constructors and exhaustive matches must handle the new fields and variants.
- **Breaking:** Reject app IDs containing control characters (newlines, ANSI
  escapes) or exceeding 256 bytes with `INVALID_APP_ID` in every app-ID policy
  mode, and refuse to start when a configured allowlist entry fails the same
  gate. Previously an open policy accepted any string verbatim into log lines,
  where newlines or ANSI escapes could forge operator-facing logs.
  The structured log fields recorded before validation or the app-ID gate —
  the pre-validation `room.join` span's `game_name` and `requested_room_code`
  and the anomalous post-handshake `Authenticate` warning's `app_id` — now
  render as Debug values (quoted with escapes) instead of raw Display strings;
  all other `app_id` log sites keep their previous shape.
- Refresh compatible Rust dependencies across the server, fuzz, and native
  reference-client lockfiles, including `uuid` 1.24.1, `saphyr` 0.0.12, and
  `webrtc`/`rtc` 0.20.3, update `taiki-e/install-action` to 2.86.3, and update
  `docker/setup-buildx-action` to 4.3.0.
- Reduce the crates.io source archive from 93 files and 3.3 MiB to 86 files
  and 2.6 MiB by excluding eight standalone test-only modules while retaining
  every runtime source and package-verification check (issue #397).
- Delete the unreachable protocol-v3 `Latest` coalescing arm from the legacy
  (pre-v3 compatibility) pop path of the outbound queue and make its classing
  invariant load-bearing. A legacy-lane row can only be forced-`Reliable`
  data, class-less control messages, or transition barriers, so both the
  unbatched and batched consumers now treat an impossible `Some(Latest)`
  front as a terminal accountability breach and stop serving the queue rather
  than guessing FIFO semantics for it (issue #444). In the exotic mid-flight
  protocol-downgrade or stale legacy-permit race those rows release
  immediately without consuming slots of an acceleration window armed before
  renegotiation, matching the control-lane precedent.

### Fixed

- Close the reaper-vs-reconnect identity-swap race surfaced by the session-187
  seam sweep (issue #396): a reconnect claim that reached the identity swap
  while its claiming socket already carried a per-socket close pin (activity
  reaper eviction, idle/slow-consumer close, oversized-outbound or teardown
  close) used to adopt the pinned signal into the restored identity — killing
  the freshly reconnected player with a stale close code (for example
  `4003 activity_timeout`) right after peers saw `PlayerReconnected`, and
  tearing its restored membership back out. The identity swap now refuses
  atomically when the transient entry carries such a pin: the pending close
  tears down only the transient socket, the one-time token is not consumed,
  and a retry from a fresh connection reconnects normally while the window is
  open (`ReconnectionFailed` with `RECONNECTION_FAILED`; the precise cause
  rides on the close frame that follows). `Shutdown` and `RoomInactive`
  closes still cross the swap — a drain must close restored connections, and
  a room-inactive pin reflects the room the claim just verified. Pinned
  red-first end to end and at the manager level.

- Close the shutdown-drain admission gaps surfaced by the session-186 seam
  sweep (issue #396):
  - A reconnect attempt delivered inside the drain grace window (only possible
    from a socket upgraded before the drain flipped) is now refused with
    `ReconnectionFailed` carrying `SERVER_DRAINING` instead of being admitted,
    force-closed with `4000`, and stripped of its restored membership — which
    also consumed the one-time reconnection token. The refusal fires before
    any claim, so the token stays spendable on a healthy instance.
  - A spectator join delivered in the same window is now refused with
    `SpectatorJoinFailed` carrying `SERVER_DRAINING` instead of publishing a
    role the drain teardown detaches at unregister. The join path's existing
    contract (new room creation refused; existing-room seat-fills allowed) is
    unchanged.
  - `discard_pending_reconnection` now skips records claimed by an in-flight
    reconnect transaction, matching the invariant every expiry surface already
    enforced; previously a drain-race discard could remove a claimed record and
    strand the reconnect (its completion would silently fail while the player
    observed success). Pinned red-first at the manager level.
  - `SERVER_DRAINING` docs and client guidance now name reconnection and
    spectator admission alongside new room creation.

- Close the last actionable issue #454 residuals from the session-182/185
  seam audits (issues #454, #396):
  - A client that exhausts its rejection-detail budget (`max_signal_errors`)
    now receives truthful guidance — "Too many rejected signaling messages;
    further rejections are reported without detail until the window resets.
    Valid signals are still relayed" — instead of "Signaling error rate limit
    exceeded", which wrongly implied a healthy client's signaling was
    throttled. The budget stays fail-closed; only the rejection detail is
    suppressed, and valid signals always relay (pinned end to end).
  - The feature-gated legacy full-mesh server's shutdown semantics are
    documented as deliberate in `main.rs`: `matchbox_signaling` 0.14 exposes
    no graceful-shutdown API and the legacy protocol has no close-code
    contract, so it stops with the process.
- Spectator join panic-repair parity (issue #396): a panic between the
  durable spectator admission and the local role publication used to leave
  a ghost database row that consumed spectator capacity and was invisible
  to both maintenance sweeps (`prune_missing_rooms`,
  `retry_disconnected_detaches`). The owned join transaction now catches the
  unwind, rolls the unpublished admission back, and still delivers the
  client's terminal `SpectatorJoinFailed`. Also pinned: the broadcast-side
  drain gate for spectator detaches (a routed member observes no
  `SpectatorDisconnected` under an active drain while persistence still
  converges), and the issue-#241 TOCTOU invariant is documented at its
  enforcement site.
- Close the actionable issue #454 residuals from the session-182 seam audits
  (issues #454, #396):
  - The protocol-v3 text relay carriers (JSON `GameData` and the
    `GameDataBinary` text fallback projection) now reject zero delivery
    stamps exactly like the binary encoders, so both carriers share one
    complete, non-zero stamp contract — a zero stamp can no longer ride the
    format-mismatch fallback projection that previously bypassed the binary
    gate. Binary-direct zero stamps keep their fail-closed outcome but are
    now classified as an invalid v3 stamp instead of a serialization failure.
  - An observer of a shutdown drain whose stored deadline overflowed to the
    `u64::MAX` sentinel is now bounded by one grace window instead of
    sleeping ~`u64::MAX` milliseconds before forcing the coded closes; the
    same bound absorbs backwards wall-clock steps.
  - The room rate limiter's fixed-window semantics (all budgets reset
    together, so a boundary burst can spend up to twice the configured
    count) are documented in the code and configuration reference,
    distinguished from the handshake limiter's sliding window.
- Shutdown drain: `finish_background_shutdown` no longer aborts a drain that
  has already begun. The drain task flips the process watch only after the
  drain begins and the `GoingAway` fan-out completes, so a serve future that
  returned inside that window used to abort the committed drain — dropping
  every connected client with no `GoingAway` advisory and no coded `4000`
  close frame. The join-or-abort decision now follows the server's drain
  state. The grace wait before the shutdown closes is also skipped when no
  socket handler is live: during a drain new registrations are refused, so
  an empty grace was pure restart delay against the operator's termination
  budget (issue #396).
- Configuration: `security.max_outbound_message_size` must now exceed
  `security.max_message_size` by at least 256 bytes of relay envelope
  headroom (constructor-enforced for direct library builds too). The relayed
  form of an admitted frame is strictly larger than the frame itself — the
  relay projection attaches the sender id and delivery stamps — so an equal
  or barely-larger outbound cap admitted frames whose fixed envelope alone
  overflowed the outbound cap, closing innocent recipients with `1009
  outbound_message_too_large`; a sender could sever the whole room. Tight
  pairings are now rejected at startup, and the 1009-close end-to-end
  scenario pins the close contract through a legal aggregate (`RoomJoined`
  roster growth) trigger. **Upgrade note:** deployments running equal or
  near-equal caps must raise `max_outbound_message_size` (or lower
  `max_message_size`) before upgrading; such pairings previously failed at
  runtime instead. The headroom covers the fixed envelope; value-level
  re-serialization growth (JSON number normalization, MessagePack→JSON
  fallback escaping) can still exceed it and remains enforced fail-closed
  at write (issue #396).
- Docs: the rate-limit scenario no longer claims the connection always
  stays open. Room and spectator admission refusals arrive on
  `RoomJoinFailed` / `SpectatorJoinFailed` and leave the connection open,
  but an over-budget per-app handshake is refused with `AuthenticationError`
  carrying `RATE_LIMIT_EXCEEDED` and the connection is closed — back-off
  applies to the next connection attempt against the same shared app-wide
  budget (issue #396).
- Scope targeted cross-instance bus delivery to the recipient's room: the
  dormant multi-instance bus path routed targeted messages through the
  unscoped sender, so a server-stamped relay `GameData` (which classifies only
  with its room context) would fail closed and loud-close an innocent
  recipient. No production path reaches the dormant seam today; the fix
  removes the landmine before any multi-instance re-enable and is pinned by a
  red-first unit test (issue #446).
- Fix dropped `EnhancedGameServer` instances retaining their dashboard cache,
  database, metrics, and refresh task until the Tokio runtime shut down. The
  cache refresh loop now releases its owners between samples, stops promptly
  when the cache is dropped, and starts only after server construction has
  completed successfully. The public `run_server` helper now also scopes its
  room-cleanup task to the serving future, so bind failure, server return, or
  caller cancellation cannot detach a task that retains the whole server;
  unexpected cleanup exit now terminates `run_server` instead of silently
  serving without maintenance (issue #396).
- Count room deletions truthfully: the exported
  `signal_fish_rooms_deleted_total` counter (and the `rooms.deleted` JSON
  field) had no production data path and stayed at zero forever while rooms
  were really deleted on the empty-room/inactive-room sweeps and on
  rolled-back room creations. Every deletion path now increments it exactly
  once; note that a creation rolled back by the shutdown-drain race counts as
  a deletion even though its never-published creation was never counted, so
  brief negative `created − deleted` excursions are possible during drain.
- Client-initiated RFC 6455 Ping keepalives now refresh activity-reaper
  liveness. Previously each inbound protocol Ping suppressed the server's own
  liveness probe (inbound-activity guard) without refreshing the reaper's
  `last_ping` stamp, so a fully healthy client that kept the connection alive
  only with standard WebSocket pings was deterministically evicted with close
  code `4003 activity_timeout` — contradicting the documented reaper contract
  ("observed no inbound traffic") and the socket-level idle timeout, which
  already counts every inbound frame. A last-seen persistence update fired by
  a teardown-racing frame from an already-unregistered player is now suppressed
  even when the heartbeat throttle is disabled (`heartbeat_throttle = 0`),
  restoring the once-per-player-per-window throttle contract on every path.
  Residual semantics: a client flooding protocol Pings keeps its reaper liveness
  refreshed even if the server→client direction is dead — matching the
  socket-level idle timeout's "any inbound frame counts" rule and still
  backstopped by the slow-consumer disconnect.
- Fix protocol-v3 text game data silently delivering without a complete relay
  stamp. The binary carrier already failed closed with `InvalidV3Stamp` when a
  v3 recipient would have observed a partial `(seq, epoch)` stamp, but the text
  carrier serialized the frame as-is, breaking recipient-side gap
  accountability without any signal. Both carriers now enforce the same
  fail-closed stamp contract for v3 cohorts (production senders stamp or
  suppress, so no legitimate flow changes), and the text failure path gained
  per-error diagnostics instead of a generic serialization message.
- Fix readiness snapshots fabricating stale state when room membership cannot
  be loaded: `current_ready_players` fell back to the unfiltered recorded set
  on a storage error, reporting departed members as still ready in
  `RoomJoined` / `Reconnected` / `SpectatorJoined` exactly when the server knew
  its view was stale. Failed membership reads now fail closed to an empty
  snapshot (with a warning) and recover normally once storage responds;
  start-game gating is unaffected (it validates membership independently and
  already surfaced such failures as infrastructure errors).
- Fix a reconnecting sender's incarnation epoch passing through a provisional
  value between connection reassignment and its override: readers of the
  reassigned entry could observe the transient socket's epoch before the real
  `last_epoch + 1` was applied. The resumed epoch is now part of the
  reassignment itself — atomically visible from the first metadata read — and
  the unchecked standalone epoch-overwrite seam that could regress a sender's
  `(epoch, seq)` stream is gone.
- Fix a stale terminal unroute being able to publish a foreign room's live
  relay stamp as a phantom `PlayerLeft` terminal watermark:
  `clear_room_assignment_with_tail` now verifies the player is still assigned
  to the expected room under the same entry lock and refuses mismatches
  untouched instead of capturing whatever assignment happens to be present.
- Fix unregistered player ids bypassing the heartbeat persistence throttle:
  in-flight frames racing a teardown took the "unknown client" branch on every
  message, inflating the `heartbeat_updates` metric and retrying a futile
  last-seen write per frame instead of at most once per player per throttle
  window. Unknown ids are now suppressed; any live seated player or spectator
  keeps an entry, so legitimate refreshes are unchanged.

- Fix a `1009 outbound_message_too_large` teardown silently discarding still-
  writable coalesced unsupported-format omission reports. A queued
  `DeliveryReport` carrier pops with its post-write flush deliberately still
  ahead of it, so when the carrier itself exceeded the configured outbound
  message-size limit, the finalize branch abandoned the queue and closed
  without ever flushing pending ranges — leaving omissions the recipient had
  observed unreported even though the socket remained writable, and breaking
  the documented "a closing connection flushes them too" promise. Every close
  path now flushes coalesced omission reports (the report never advances the
  data sequence, so it cannot open a hole of its own).
- Fix overflowed duration configurations inverting into already-expired
  deadlines on three WebSocket seams (ping write bound, Pong probe deadline,
  authentication window) and on the reconnection-window deadline: an absurd
  configured timeout previously closed every fresh connection immediately with
  an authentication/activity code instead of behaving as effectively
  unbounded, contradicting the repository doctrine that an overflow must never
  become immediate expiry. All such seams now saturate to the platform's
  maximal representable instant via a shared canonical helper.

- Fix the last published member's departure deleting a finalized room's sticky
  session decision while storage still holds an admitted member whose route
  has not committed (a failed finalized publication spawns its teardown only
  after releasing the room mutation gate). Deleting the plan in that window
  downgraded that member's whole room generation to the relay floor on its
  next committed route, contradicting sticky topology/transport for the
  session lifetime; the decision now survives until storage empties too, so
  the pending member's publication repairs the session (re-electing a capable
  host) instead.
- Fix outbound capacity-release witnesses discarding their own evidence after
  a protocol upgrade while parked: `CapacityReleaseWitness::permits_locked`
  required lane identity between the lane frozen when Full was observed and
  the caller's freshly recomputed target lane. A v2→v3 negotiation completing
  mid-backpressure changed that lane, so genuine pre-deadline release evidence
  was discarded and a fully drained, on-rate recipient was evicted as a slow
  consumer purely for upgrading. Release evidence is now evaluated on the
  witness's own lane; actual space on the current target lane remains enforced
  by the separate capacity check (issue #416).
- Fix inactive-room garbage collection deciding liveness from the wall clock
  while every other reaper (client pings, reconnect windows) uses monotonic
  time: an NTP correction, manual clock change, or host suspend/resume that
  stepped the wall clock past `server.inactive_room_timeout` deleted occupied,
  actively-playing rooms (farewell `RoomInactive`, close code `4005`), and a
  backward step kept idle rooms alive past their timeout. GC decisions now key
  off a per-room monotonic activity stamp refreshed in lockstep by every
  production activity path (create, player/spectator join, departure, explicit
  refresh); the wall-clock `last_activity` remains the observability record.
- Fix a parked backpressured delivery vanishing without any accounting when
  the `BackpressuredDelivery` value itself is dropped unresolved (the owning
  broadcast future is cancelled while parked): the attempt counter never
  resolved, no drop was counted, and the recipient's queue ledger never
  learned of the loss. Dropping that value now resolves the attempt as an
  accounted drop (global counter, per-connection ledger, and the queue's
  attempted+abandoned pairing), records a trace-validation event, and leaves
  the healthy recipient connected.
- Fix the remaining cancellation windows in the delivery pipeline: cancelling
  the parked-delivery wait itself (`finish_backpressured_delivery_in_room`)
  used to defuse the drop guard before its select, and the three conditional
  waits (`reserve_initial_transition`, `deliver_to_one_if`, `reserve_one_if`)
  had no cancellation accounting at all. A cancelled wait now resolves its
  attempt exactly once as an accounted drop with attempted+abandoned queue-
  ledger pairing, without requesting a close of the healthy recipient
  (issue #417).
- Fix room-operation coordination failing with `INTERNAL_ERROR` behind a
  leaked lock key: the per-operation distributed keys could not coordinate
  server processes while every coordinator critical section already holds the
  TTL-free per-room mutation gate first, so the keys excluded nobody while a
  leaked key burned the full ~22.5 s retry storm before failing. The
  coordinator-level locks are removed (the mutation gate is the single
  serializer); for the join-path locks that remain, acquisition backoff is
  now provably bounded strictly inside the lease TTL it contends on via
  `RetryConfig::worst_case_total_backoff` / `clamped_to_total_backoff`
  (issue #414).
- Fix the `CircuitBreaker` open-state window restarting on every late
  closed-era straggler failure while already open, which could starve the
  half-open probe indefinitely; the window is now anchored at the transition
  into Open. `reset()` also invalidates outcomes from calls admitted before
  it, so a stale probe failing after an administrative reset can no longer
  reopen the freshly cleared circuit, and open-window timing uses the
  monotonic clock like the rest of the server's liveness decisions.
- Fix a join racing inactive-room deletion reporting
  `ROOM_CREATION_FAILED` instead of `ROOM_NOT_FOUND`: when the room row
  disappears between the join path's recheck and its membership write (GC
  winning the race), the failure is now classified by a fresh existence check
  so clients see the accurate wire code.
- Fix TURN credential minting silently emitting an empty (always-rejected)
  credential if HMAC initialization ever failed: the ICE builder now refuses
  the whole TURN entry (fail closed, STUN/static entries unaffected) rather
  than advertising an unusable pair.
- Fix player-name uniqueness accepting canonically decomposed (NFD) spellings
  of an already-taken name: comparison now composes both sides to NFC before
  case folding, so byte-distinct spellings of one visible name (Hangul
  syllables vs jamo sequences, or decomposed spellings under configurations
  that allow combining marks) can no longer coexist in a room.
- Fix the public `CircuitBreaker` opening on isolated failures: a successful
  closed-state call resets the failure streak so only genuinely consecutive
  failures trip the threshold, half-open recovery admits exactly one probe and
  rejects concurrent calls without executing their operations, only the
  admitted probe resolves the half-open state so straggler calls cannot steal
  its transition, and an abandoned probe admits a fresh probe instead of
  wedging half-open admission (issue #403).
- Fix in-memory distributed lock leases starting before local lock contention
  completed, which could make a newly acquired or extended lease immediately
  expire and allow overlapping room-capacity or game-start mutations.
- Fix room and spectator cleanup races: room garbage collection now preserves
  reconnectable rooms when a disconnect overlaps a sweep, and failed rollback
  of an unpublished spectator admission is retained for maintenance repair.
- Fix rejected oversized binary game-data frames refreshing client and room
  liveness, so invalid traffic cannot indefinitely postpone inactivity cleanup.
- Correct the protocol contract for `JoinRoom.relay_transport`: it is a
  compatibility-only hint that the server ignores, so every accepted value and
  omission retain the same authenticated WebSocket relay path. New clients
  should omit it; removal is reserved for a future breaking protocol version.
  Also clarify that `relay_type` and `ConnectionInfo.relay` are informational,
  client-untrusted legacy metadata rather than routing authority (issue #393).
- Re-issue `StartGame` in both maintained reference clients (native and
  browser) after a readiness invalidation instead of latching after one send:
  the room creator recomputes all-ready from the authoritative `ready_players`
  snapshots over the current membership (mirroring the server's gate), latches
  against duplicate all-ready broadcasts, and re-arms on a join (always
  unready, no corrective broadcast), a departure (which can restore all-ready
  with no readiness broadcast), or an authoritative `GAME_START_NOT_READY`
  rejection; a `RoomJoined`/`LobbyStateChanged`/`Reconnected` snapshot
  refreshes the readiness baseline without re-arming the latch (so a
  reconnect can never duplicate an in-flight send), and a `RoomLeft` resets
  it. A stale one-shot latch stalled lobbies exactly when the server's
  readiness re-check mattered (issue #449).
- Bump the yanked `chacha20` 0.10.1 lockfile pin to 0.10.2 across the server,
  native-client, and fuzz workspaces (pulled in via `rand` 0.10; no API or
  behavior change).

### Security

- Update `h2` to 0.4.16 to address RUSTSEC-2026-0258.

## [0.7.0] - 2026-08-16

### Added

- Add deterministic stalled-join and Linux process-pause delivery evidence.
  Fresh admission now proves its exact baseline before a stalled incumbent's
  eviction, targeted retry preserves every healthy peer, and an ignored
  real-server `SIGSTOP`/`SIGCONT` scenario accepts only a causally complete
  protocol-v3 stream before clean teardown. The native JSONL contract now
  exposes validated delivery stamps and reports, semantic negative controls
  falsify silent holes and sequence/lifecycle regressions, and nightly
  diagnostics are retained as artifacts (issue #374).
- Add exhaustive formal verification for legacy application-owner rollback
  composition. The production-shaped model reaches multiple pending durable
  detaches while proving that at most one carries owner-rollback provenance,
  then checks cleanup, adoption, reconnect takeover and rejection, deletion,
  and the authoritative quota-counting corollary. Five independent expected
  failures pin stale snapshots, duplicate provenance, a skipped live-route
  re-read, cleanup overtaking a delayed requeue, and deleted-room resurrection
  (issue #220).
- Add a strict, payload-free protocol-v3 sequencing trace for production-shaped
  receiver observations. Focused real-socket scenarios record anonymous
  watermarks, exact classified gaps, lifecycle epochs, and reconnect snapshots;
  a bounded fail-closed compiler replays them against a TLA+ receiver-view
  refinement and proves four independently seeded divergences non-vacuous
  (issue #220).
- Add end-to-end WebSocket upgrade diagnostics for operators. Every
  application-handled acceptance or deliberate HTTP rejection now returns and
  logs one correlation ID, exact outcome, status, and transport peer address
  (not an inferred forwarded client address), while JSON/Prometheus counters
  conserve attempts across accepted, Origin, drain, and token-binding results.
  A tested concurrent probe uses fresh RFC 6455 keys, verifies each complete
  handshake, and emits a non-secret client attempt ID for proxy-log correlation;
  it ignores default curl configuration files, validates only the final
  response header block, excludes non-allowlisted response headers and raw
  transport stderr from failure evidence, and preserves response time after the
  DNS/TCP/TLS connection budget. A scheduled public-path workflow distinguishes
  a healthy HTTP route from working TLS/proxy WebSocket admission (issue #367).
- Add exhaustive connection-generation-aware coverage for a finalized v3 mesh
  member reconnecting on v2. The fresh v2 socket receives no v3-only session
  plan, capable incumbents receive exact peer refreshes that exclude it, and a
  real-WebSocket regression preserves frozen-v2 reconnect fields plus relay
  gameplay. Three semantic expected-failure models keep wire gating, fresh
  capability use, and complete incumbent publication independently non-vacuous
  (issue #220).
- Add exhaustive session-model coverage for capability-downgrading reconnects.
  The ordinary disconnect/failover/reconnect composition must publish against
  the fresh relay-only profile while restoring original join priority and
  preserving a live successor authority. Four semantic expected-failure models
  keep complete publication, fresh-profile use, ordering, and authority
  guarantees independently non-vacuous, with a Rust regression pinning exact
  `connected_at` restoration (issue #220).
- Add exhaustive formal verification for the atomic reconnection-claim
  lifecycle, including duplicate claimants, invalid identity, stale claim
  handles, expiry cleanup, restore failure, and one-time consumption. Four
  expected-failure models keep the individual safety properties non-vacuous,
  with a deterministic Rust regression pinning stale-handle rejection (issue
  #220).
- Add formal verification and deterministic concurrency coverage for exact
  two-phase room publication transactions and the process-local room-event
  mutation handoff. Publication checks cover solo, empty-member, one-phase,
  phase-one-only, and full batches; pin complete pre-hook reservation,
  canceled-attempt retry, final route-generation validation (including
  zero-frame members), zero publication on hook rejection or error, phase
  ordering, exactly-once degraded-delivery callbacks, and complete permit and
  failure accounting. Room-event checks pin same-room mutation and owned-job
  execution ordering, caller detachment, error and panic isolation,
  independent lanes, drain-empty handoff, and weak-registry replacement safety
  (issue #220).

### Changed

- Simplify the README and public documentation around a clear start-to-build-to-deploy
  path. Concise onboarding and capability pages now point to the authoritative
  protocol, configuration, and operator references instead of duplicating their
  detailed tables and examples, and contributor-only material no longer appears
  in the primary documentation navigation (issue #383).
- Adopt the accessible Vector design system across the README and documentation
  site, including the approved fish mark, favicon, responsive wordmark,
  self-hosted typography, contrast-safe light and dark themes, and preserved
  Material component styling (issue #204).
- Refresh compatible dependencies across every tracked Rust lockfile, remove
  the unused direct `once_cell` development dependency, and pin plus verify the
  CI `cargo-audit` version. The no-thread Godot fixture remains unified on its
  exact 0.4.5 binding stack.
- Reduce the published crate from 652 archive entries (11.1 MiB unpacked) to
  92 runtime-source, operator-example, license, and Cargo-metadata entries
  (3.1 MiB unpacked),
  excluding development benchmarks, repository-only CI, tests, extended
  documentation, formal models, planning records, and reference clients while
  retaining the complete operator configuration example (issue #355).
- Reduce GitHub Actions runner allocations by removing the obsolete unused-
  features status alias and a no-op documentation job, and by limiting three
  interop-local dependency audits to manual dispatch. Central CI continues to
  audit every tracked Cargo graph on pushes, pull requests, and its daily
  schedule; manually selected unmerged interop refs retain local audit coverage
  (issue #345).
- Reduce representative Rust pre-commit latency by overlapping worktree
  discovery with hook setup and loading the line-aware panic scanner only for
  added panic macros or removed test-context guards. Untracked Rust files and
  Git errors still fail closed, and the hook remains PowerShell-and-Git-only
  (issue #318).
- Reduce relay allocation overhead whenever projection work repeats across a
  relay's recipients (issue #207). In addition to the shared frame cache's
  reduction from 680 to 472 bytes, newly built production relays now co-own
  that cache with the message envelope. Eight- and 16-player JSON and binary
  ingress fall from three to two allocation operations and from 1,120 to 1,104
  allocated bytes per relay; two-player ingress and the public compatibility
  handoff remain unchanged. On the checked-in representative workload, JSON
  text projection now pre-sizes its relay frame, eliminating three growth
  reallocations and reducing allocation operations from 7/8 to 4/5 in
  2-/8-/16-player rooms, with 30–36% fewer allocated bytes. The healthy
  production handoff now also avoids allocating its async completion state,
  reducing both JSON and binary ingress from two allocations to one and saving
  another 352 allocated bytes per relay at every room size. Healthy fan-out now
  also borrows registered delivery handles, removing one queue-handle clone and
  its synchronization per recipient per relay; ownership is retained only for
  exceptional backpressure and slow-consumer cleanup. Routing contention
  retains the existing async fallback, while backpressured delivery retains its
  existing ordering, deadline, and cleanup semantics. Codec work and exact wire
  output are preserved.

### Fixed

- Fix automated release preparation so transformation validation, rollback,
  scope checks, and Git staging share one complete file inventory, including
  dynamically discovered lockfiles and the versioned getting-started URL.
  Live runs now prefer the auto-commit GitHub App client ID while remaining
  compatible with the existing App ID secret, credential-free dry runs remain
  available, and recovery capture can no longer mask an earlier failure.
  Preparation attempts now retain their queue position, pin the dispatched
  source, accept only canonical one-commit recovery branches, and reconcile
  ambiguous branch or pull-request mutations without repeating a write.
- Require release publication preflight to prove successful required-workflow
  push runs for the exact retained default-branch source. Every manual retry or
  human tag must resolve to the unique first-parent commit that introduced its
  package version, so missing Documentation Validation runs fail closed without
  approximating a historical push's changed-file range. Release attempts remain
  queued, and human tag publication follows the same gated path as manual
  publication.
- Prevent delayed GHCR publications and historical backfills from rolling
  mutable image aliases backward. `latest` now moves only for the current
  default-branch head, and `X.Y` only for the highest canonical annotated
  `vX.Y.Z` tag and a non-newer verified registry alias, while immutable release
  and commit aliases remain repairable. Manual backfills must be dispatched
  from the default branch and use the unique first-parent version-introduction
  commit.
- Require operators to configure the scheduled public WebSocket probe endpoint
  through the repository instead of shipping a deployment-specific hostname.
  Manual dispatch can still override the endpoint, and an unconfigured monitor
  now fails with an explicit diagnostic before probing.
- Decide reconnect eligibility from one monotonic deadline captured at the
  genuine disconnect. A host clock adjustment — an NTP step, a manual
  correction, or a suspend/resume — can no longer expire a live reconnector
  early or keep an elapsed one claimable, and validation, claiming, expiry
  cleanup, and room-GC protection can no longer disagree at the boundary. A
  reconnect attempt after the window elapses now reports the documented
  `RECONNECTION_EXPIRED`; previously the token's own wall-clock expiry, armed
  to the same instant, masked it as `RECONNECTION_TOKEN_INVALID`, so clients
  could not tell a closed window from a bad credential. Reported
  `disconnected_at` and token timestamps are unchanged (issue #373).
- Keep duplicate same-room disconnect registration from extending a reconnect
  deadline, reopening an active single-use claim, or consuming the replacement
  connection's next token, including when late teardown overlaps an already
  reserved reconnect (issue #372).
- Prevent slow or contended Miri safety runs from spuriously failing rate-limit
  preflight accounting when the 100-millisecond unit fixture expires between
  assertions. Asynchronous rate-limit assertion sequences now use paused virtual
  time, while explicit expiry coverage and production behavior remain unchanged
  (issue #364).
- Keep accepted native and browser reference-client run, WebRTC, handshake,
  and watchdog durations from expiring immediately when their absolute
  deadline exceeds the platform clock or timer range. Browser numeric flags
  now reject imprecise integers and long host timers advance in bounded
  chunks, while ordinary and zero-duration behavior remains unchanged (issue
  #360).
- Make spectator admission validate and case-normalize room codes, enforce
  configured game/room naming rules, and share the room-admission attempt budget.
  Also keep extreme valid idle timeouts and unrepresentably distant internal
  deadlines later than the process can represent instead of turning overflow
  into immediate idle, slow-consumer, batch-flush, or shutdown expiry (issue
  #358).
- Terminally reconcile connected players when inactive-room maintenance
  deletes their room. Cleanup now generation-fences relay routing, clears local
  assignments and reconnect state, closes the sockets with the distinct `4005
  room_inactive` reason, and retries convergence after an interrupted sweep
  instead of leaving a deleted ghost room able to relay gameplay.
- Keep extreme configured reconnection windows valid instead of narrowing
  them into negative durations that expire tokens immediately. Numeric
  conversions that can truncate, wrap, or lose a sign are now rejected by the
  root package's all-target Clippy policy (issue #213).
- Isolate room-local routing fences so a join, reconnect baseline, replay hook,
  or exact session publication waiting in one room no longer blocks routing and
  relay progress in unrelated rooms. Same-room baseline, replay, watermark, and
  exact-membership ordering remain atomic (issues #220, #329).
- Wake a classified WebSocket receiver when its final reserved control is
  canceled by a concurrent room-generation transition. The receiver now
  observes terminal disconnect instead of remaining parked after every sender
  and permit has gone (issue #220).
- Keep selected ICE candidate-pair evidence as a native reference-client clean
  exit requirement after both data channels open. Gameplay exchange now starts
  immediately while an eventually consistent statistics snapshot is pending;
  the client retries that evidence under its existing run deadline and cannot
  exit successfully without the concrete selected path. A selected remote
  candidate that rtc 0.20 omits from its registry is reported as the
  peer-reflexive path the ICE agent dynamically learned, with its unavailable
  address left redacted instead of polling forever. The cross-platform
  no-STUN/no-TURN smoke gate now accepts that direct host/peer-reflexive shape
  only when both clients advertised non-empty host-only UDP candidate sets;
  IPv6 and TURN gates retain exact host/host and relay/relay requirements.
  Failures include both clients' selected pairs, advertised candidates, exit
  codes, and stderr (issues #301, #337).
- Preserve the reviewed release-preparation source when documentation or
  workflow changes reach the default branch before publication. Manual releases
  now select the unique first-parent commit that introduced the package version
  and reject shallow history or reused version boundaries instead of publishing
  a later tree under the prepared version.
- Prevent both reference clients from busy-spinning while an outstanding Ping
  is in its Pong drain grace. A connected peer's exact exchange debt now
  survives later departure or transport loss instead of becoming vacuously
  successful. Native, browser, and TURN interoperability runs also hold every
  successful client at a shared release barrier. The native barrier waits for
  completed gameplay criteria without coupling success to unrelated ICE
  gathering after a selected path is already connected; signal-ledger tests
  can opt into that stricter freeze independently. Browser runs preserve their
  full post-success linger even when barrier release occurs after the soft run
  deadline.

### Security

- **Breaking for token-binding clients:** Replace client-chosen token-binding
  keys with a server-fresh v2 challenge,
  HKDF-SHA-256 connection keys, one monotonic proof sequence across JSON and
  binary frames, and RFC 8785 canonical JSON. Token-bound MessagePack now uses
  an authenticated versioned envelope, and certificate-bound reconnect tokens
  can be claimed only by the rustls-authenticated identity that received them;
  a mismatch is checked atomically without consuming the valid token (issue
  #347).

- **Breaking:** Fail startup and `--validate-config` when TLS is enabled in
  configuration but the server binary was compiled without the `tls` Cargo
  feature, instead of silently serving plaintext HTTP. Newly prepared official
  release archives and container images now compile built-in TLS explicitly,
  while historical release retries preserve their source-owned feature set.
  TURN's plaintext-signaling warning uses effective runtime TLS rather than
  trusting configuration alone.
- **Breaking:** Stop treating client-controlled forwarding headers as verified
  mTLS fingerprints. The built-in listener now derives a lowercase hexadecimal
  SHA-256 fingerprint from the authenticated rustls leaf certificate and
  supports `require_client_fingerprint=true` when token binding is mandatory,
  built-in TLS is active, and client authentication is optional or required.
  Missing certificates and subprotocol opt-outs are rejected, conflicting
  request headers cannot override transport identity, and rotated certificates
  invalidate proofs bound to the previous fingerprint. Fingerprint-bound
  connections initially advertised JSON game data only and rejected unsigned
  binary frames. Replay-resistant channel keys and certificate-bound reconnect
  tokens were intentionally out of scope for issue #344 and are addressed by
  the issue #347 entry above.
- Enforce `security.cors_origins` on browser WebSocket upgrades at both
  `/v2/ws` and `/v3/ws`, using the same parsed policy as HTTP CORS responses.
  Disallowed origins now receive HTTP 403 and malformed allowlists fail
  configuration validation; origin-less native clients remain compatible
  (issue #319).

## [0.6.0] - 2026-08-08

### Added

- Add exhaustive formal verification for classified outbound queue
  capacity-deadline arbitration (issues #220, #290), proving that continuous
  pre-deadline progress survives scheduler delay, refills invalidate stale
  evidence, and deadline validation plus admission remain atomic. A seeded
  model of the former timer-first defect must still produce the exact false
  `SlowConsumer` counterexample.
- Report every advertised local ICE candidate on the client's JSONL event
  stream as `local_candidate` (`peer`, `candidate_type`, `address`, `port`,
  `protocol`), emitted after the relay so the set describes what the remote
  side can actually see (issues #275, #276). The relay-only TURN cells now
  require the positive control to advertise relay candidates and only relay
  candidates, and the mismatched-secret control to advertise none; the IPv6
  cell requires no IPv4 entry in the advertised set, closing the gap where a
  `--ip-family` regressed to a no-op could still pass on a dual-stack host.
- Prove the native reference client on every supported desktop platform and over
  IPv6 (issue #271). A locked Windows/macOS matrix now builds, lints, and
  unit-tests `clients/native` on the exact repository MSRV, alongside the
  pre-existing Linux interop job — neither the root CI matrix nor those cells
  built that standalone crate off Linux. A
  new interoperability cell drives two real client processes with the new
  `--ip-family ipv6` selector, so only IPv6 host candidates can be advertised,
  and requires a host/host pair of concrete dialable IPv6 addresses plus the
  exact reliable and unreliable exchange. The client now reports each selected candidate's address
  alongside its type, and a runner without IPv6 loopback fails the cell with an
  actionable message instead of skipping it. Windows and macOS now also build
  the real server and run the smallest complete live mesh: two client processes
  must select a direct host/host pair and exchange exact traffic on both SCTP
  data channels. The larger topology, fault-injection, TURN, browser, and IPv6
  matrices remain Linux-specific evidence (issue #275).
- Add a deterministic TURN-only WebRTC interoperability gate (issue #239).
  Two native clients must select relay candidates through a digest-pinned local
  coturn, exchange exact reliable and unreliable data, and retain a live
  WebSocket relay floor; a mismatched-secret control must fail WebRTC and use
  that fallback for the asserted coturn authentication-rejection reason. After
  explicit dependency provisioning, the proof runs offline and does not
  validate production TURN infrastructure.

### Changed

- Advance both pinned Fortress Rollback compatibility fixtures to 0.12.0
  (issue #309), preserving the native multiprocess and Godot no-thread WASM
  gameplay oracles while replacing the unmaintained `bincode` graph with
  `bincode-next` and removing the obsolete advisory exceptions. The daily and
  pull-request security jobs now apply both Cargo policy and RustSec scans to
  every tracked Cargo lockfile, including exact-release fixtures.
- Make live-reference-client interoperability failures actionable (issue
  #301). A missing native WebRTC candidate pair now reports the client's exit
  code, stderr, full event progression, and advertised ICE candidates. The
  hosted Fortress/Godot WASM gate permits one recovered prediction-window
  denial but fails on any repetition; its zero-wait, progress, throughput,
  queue, rollback, checksum, conservation, and error oracles remain unchanged.
  The expanded acceptance-threshold report and browser configuration use schema
  version 3, so stale v2 exports fail closed instead of silently omitting the
  new stall boundary.
- Refresh the browser reference client's runtime and development toolchain:
  Playwright Core 1.62.1, TypeScript 7.0.2, Prettier 3.9.6, and Node.js 26 type
  definitions. The supported Node.js floor remains 20.
- Exempt macOS from the 16-player matrix's wall-clock p99 relay-latency
  ceiling (issue #274); Linux and Windows still gate it. On hosted macOS the
  same `message_pack-16p-clean-30hz` cell measured 322,391us and
  261,754us on two runs while the `json-16p` cell in the same process measured
  26,019us and Linux measures ~7,000us: a 12x intra-run spread that tracks
  runner tenancy, not relay behavior. Every correctness oracle — exact
  delivery ledgers, conformance audit, zero backpressure, zero slow-consumer
  eviction — still runs on all platforms, and every cell's `p99_us` stays in
  its per-cell diagnostic line on every platform.
- Raise the exact Rust MSRV from 1.89 to 1.91 and migrate the standalone native
  reference client from webrtc-rs 0.17 to 0.20 (issue #268). The port preserves
  the matchbox candidate wire shape, deterministic ICE crippling, generation
  fencing, and native/browser/TURN interoperability while adopting the 0.20
  async peer-event handler and poll-driven data-channel API. Concrete active interface
  binds preserve usable zero-STUN host candidates without advertising wildcard
  addresses, including scope-correct global and loopback IPv6 candidates.
- **Breaking:** Fence protocol-v3 WebRTC signaling with a required
  `SessionPlan.generation` UUID carried by every client and server `Signal`
  envelope (issue #258). Clients must rebuild retained WebRTC pairs when the
  authoritative generation changes, use the refreshed ICE/TURN credentials,
  and discard delayed signals from older generations. The reference clients
  also bound pre-connection signal buffers and drop signals for peers outside
  the current plan; protocol-v2 wire bytes remain unchanged.
- Reduce relay allocation work for the universal WebSocket fallback (issue
  #207). MessagePack-to-JSON projection now pre-sizes its output and eliminates
  all 3/6 growth reallocations in 2-/8-/16-player mixed-format rooms, reducing
  allocation operations from 18/28/28 to 15/22/22 and allocated bytes by
  17–20%. Repeated frozen-v2 JSON and Rkyv relays also skip a no-op frame cache,
  reducing 8-/16-player operations from 4 to 3 and bytes by 51%/41%. The
  ingress-to-queue handoff now borrows its one-shot builder instead of heap
  boxing it while retaining the original public boxed compatibility seam, and
  the healthy path walks the guarded routing snapshot directly instead of
  copying every recipient into a temporary vector. The isolated handoff now
  measures 1/2/2 operations and 352/1,032/1,032 allocated bytes at 2/8/16
  players; separate JSON and binary production-ingress cells, including
  message-envelope construction, measure 2/3/3 operations and
  648/1,328/1,328 bytes. Exact message variants, delivery accounting,
  backpressure concurrency, routing snapshots, and cancellation behavior are
  checked, and no relay-latency improvement is claimed without an independent
  timing measurement.
- Reduce memory-allocation work for frozen-v2 JSON and Rkyv binary relays
  (issue #207). The server now removes one allocation operation in two-player
  rooms, two in 8-/16-player rooms, and one payload-sized copy per relay:
  measured
  operations fall from 4 to 3 for two-player rooms and from 6 to 4 for 8-/
  16-player rooms, while allocated bytes fall by 38–70%. Exact wire bytes,
  delivery accounting, queue drainage, and the v3/MessagePack paths are
  unchanged.
- Refresh the pinned nightly analysis toolchain and its local documentation
  (issue #243). Miri, AddressSanitizer, cargo-udeps, and cargo-fuzz now use
  `nightly-2026-08-01`; cargo-udeps 0.1.61 validates all targets and features,
  the fuzz workflow smoke-runs all four declared targets, and policy tests
  prevent partial pin or fuzz-target inventory updates.
- Reduce binary relay serialization allocation operations by pre-sizing the
  MessagePack envelope from the known opaque payload length (issue #207).
  Direct protocol-v3 binary relays now use 5–6 allocation operations instead
  of 10–11, while 8- and 16-player mixed-format relays use 28 instead of 37.
  Allocated bytes fall by 26–39% for direct binary traffic and 8–9% for mixed
  traffic; emitted wire bytes remain identical, and capacity-overflow errors
  remain distinct from allocator failures during initial reservation and
  fallback buffer growth while preserving allocator diagnostic context.

### Fixed

- Prevent release preparation from opening a test-red or semver-incompatible
  pull request (issue #305). The workflow now runs the release and standalone
  lockfile identity tests against the prepared tree before pushing it, and
  `**Breaking:**` Unreleased notes reject a patch bump during `0.x` or any
  non-major bump after `1.0`.
- Keep the generated and documented error-code contract limited to codes the
  server can emit (issue #300). `INVALID_TOKEN`, `AUTHENTICATION_REQUIRED`,
  `APP_ID_EXPIRED`, `APP_ID_REVOKED`, `APP_ID_SUSPENDED`, and
  `SERVICE_UNAVAILABLE` remain decode-compatible Rust variants but are reserved;
  `ErrorCode::NON_EMITTED` exposes that compatibility set to Rust consumers and
  code generators. Clients should handle `RECONNECTION_TOKEN_INVALID`,
  `MISSING_APP_ID`, HTTP 503, or `SERVER_DRAINING` for the corresponding shipped
  behavior.
- Update `lru` to 0.18.2 for its upstream `pop` panic-safety fix. Signal Fish
  uses that operation while expiring coordination deduplication entries.
- Return the durable-detach retry that a rejected reconnect took over
  (issue #297). A reconnect removes the queued retry for the membership it is
  claiming — maintenance must not delete a row the reconnect is about to make
  live — but only re-queued one when the attempt had restored the membership
  itself. A reconnect against a row that survived a failed disconnect removal
  restores nothing, so any later failure (the second room read,
  `reassign_connection`, or an undeliverable `Reconnected` baseline) erased the
  marker and left the phantom row holding a seat in every capacity check:
  genuine joiners were told `ROOM_FULL` and the room never looked empty to
  cleanup, for the remainder of the reconnection window. The uncommitted durable
  state is now one value that every rejection path unwinds, and the retry is
  handed back with the application-claim rollback provenance it started with.
- Answer a rejected `JoinAsSpectator` with `SpectatorJoinFailed`
  (issue #298). The message was documented in `docs/protocol.md` and declared in
  the AsyncAPI document but had no emitter: every failure produced a generic
  `Error` instead, so a client awaiting the documented
  `SpectatorJoined | SpectatorJoinFailed` pair — the same contract `JoinRoom`
  has with `RoomJoined` / `RoomJoinFailed` — waited out its own timeout. The
  reason and `error_code` are the values the `Error` frame carried; a client
  that only handles `Error` for this case must now handle
  `SpectatorJoinFailed`. `docs/concepts/spectator-mode.md`, which described the
  `Error` shape, now matches.
- Report the documented cause of an `AuthorityRequest` denial (issue #298).
  `AUTHORITY_CONFLICT` and `AUTHORITY_NOT_SUPPORTED` are specified in
  `docs/concepts/authority.md`, `docs/reference/error-codes.md`, and the
  AsyncAPI document, but the storage layer returned refusals as an untyped
  string and the single coordinator mapping site flattened every one to
  `AUTHORITY_DENIED`. A client that lost a race was told it lacked permission —
  and `authority.md` has clients disable host migration on that code — while a
  client in a room created with `supports_authority: false` was told the same
  and could retry forever. Denials that are genuinely neither (not a member,
  releasing a role you do not hold) keep `AUTHORITY_DENIED`, a room that
  disappeared during the transition reports `ROOM_NOT_FOUND`, and a storage
  fault keeps `STORAGE_ERROR`. Granted responses retain the frozen v2
  `reason: null` field; the protocol guide and canonical sample now show that
  exact shape. No wire schema changed: all five codes already existed.
  **Breaking (Rust API):** `GameDatabase::request_room_authority` and
  `RoomOperationCoordinatorTrait::handle_authority_request` return
  `AuthorityOutcome` instead of `(bool, Option<String>)`; use
  `AuthorityOutcome::granted()` and `AuthorityOutcome::denial()`.
- Keep a member that reconnects into a running game marked ready. Removal
  prunes the departing id from a finalized room's ready list, so the restored
  membership carries the only surviving evidence that it started the game — and
  readiness cannot be re-established by hand, because a finalized room rejects
  `PlayerReady`.
- Report the right readiness in every room snapshot. Readiness lives in the
  coordinator while a room is open and moves into the room record at
  finalization, so reading either source alone is wrong half the time:
  `SpectatorJoined` read the record and showed an all-unready lobby however
  ready the members were (and spectators receive no lobby broadcast that could
  correct it), while `RoomJoined` and `Reconnected` read the coordinator and
  showed an all-unready game after it started. All three now select by lobby
  state.
- Deliver `Latest` game data that is superseded faster than the configured
  outbound `batch_interval`. A superseding value continues its key's pendency
  instead of restarting the coalesce window, so a key updated every 8 ms against
  a 16 ms interval now reaches the socket about once per coalesce window (a
  release, the next update reopening the window, and that window elapsing);
  previously such a key delivered no data at all — indefinitely — while still
  spending one `DeliveryReport` frame per update. Only deployments with
  `websocket.enable_batching` enabled were affected.
- Drop a buffered `AuthorityChanged` from a reconnecting member's
  `missed_events` when the `Reconnected` snapshot already supersedes it, so the
  frame cannot assert both "you are the authority" and "authority is vacant".
- Announce the authority cleared by a departure. When the authority player
  leaves or disconnects, every remaining member now receives
  `AuthorityChanged` with `authority_player: null`, ordered after the
  `PlayerLeft` that explains it and never before it. `docs/concepts/authority.md` specified this
  for the disconnect case and now covers `LeaveRoom` and the ordering as well. A
  member reconnecting across the change is told the holder as it stands on
  return. Without it a client
  following the documented host-migration flow never learned the role was
  vacant.
- Keep the stored `is_authority` flag equal to the room's `authority_player`.
  A reconnecting member's pre-disconnect snapshot is no longer restored
  verbatim, so a room whose authority was claimed by a successor while the
  original was away can no longer report two authorities in `RoomJoined`,
  `Reconnected`, and `GameStarting` payloads.
- Start a rejoining member unready. Readiness belongs to a membership, so
  leaving and joining the same room again no longer restores the previous
  ready state — which had inverted that member's next `PlayerReady` toggle and
  let the remaining members reach `all_ready` (and `StartGame`) without them.
- Stop issuing TURN credentials to a recipient that cannot join the session.
  A `SessionPlan` for a member that never negotiated the session's topology and
  transport already carries an empty peer list and the relay fallback; it now
  carries no ICE servers either, since there is nothing for that member to
  gather against. The `RoomJoined` pre-gather surface still mints against the
  game's _desired_ topology, the only information available before a session
  exists, so a member eligible there can still receive a credential this seam
  later withholds.
- Fence a closing connection whose undeliverable payload was counted but whose
  exact range never reached the pending delivery report, which can happen when
  that report is full and the write that would drain it is cancelled. Teardown
  now abandons the frames behind the omission instead of flushing past a hole
  no report describes.
- Count every TURN credential issued by a finalized-room join in
  `turn_credentials_issued`, including incumbent refreshes when the joiner is a
  v2 client or the join forces a host re-plan. The reconnect and host-failover
  paths already counted theirs unconditionally, so TURN capacity planning
  systematically undercounted rooms with mixed-version members or host churn.
- Prevent a classified outbound queue from falsely closing a progressing
  recipient with `4002 slow_consumer` when capacity became and remained
  available before the configured deadline but the waiting producer was not
  scheduled again until after it. Capacity first available at or after the
  deadline remains expired. The H14 mixed-encoding experiment now also bounds
  both proxy receive windows so its equal 32 KiB/s comparison cannot depend on
  host TCP autotuning (issue #290).
- Make automatic room creation reliably return an unoccupied, joinable code
  (issues #283 and #284). Invalid length/prefix combinations now fail at startup,
  and a generated-code collision is retried with up to eight independently
  generated candidates instead of joining the existing room or rejecting the
  creation. Explicit room-code join/create behavior is unchanged.
- Make an unreachable TURN allocation self-describing in the native reference
  client and its interoperability lane (issue #276). The client now reports the
  complete resolved ICE bind set on every pairing — bound addresses, which came
  from interface enumeration, and which the routing probe added — instead of
  reporting only the union's additions, so a run where the probe contributed
  nothing is no longer indistinguishable from one where it never ran. An ICE
  server that does not resolve or does not route is a warning rather than a
  debug line. The lane also captures the host's own view of the TURN network
  (`ip route get`, addresses, routes, and the Docker bridge's link state) into
  its failure artifacts, because whether the expected source address existed
  and whether its device was carrier-up cannot be recovered from a client log
  after the fact. Those diagnostics falsified the recorded cause on their first
  failing run: the bind set already contained the bridge's routable source and
  the bridge was carrier-up, so the missing datagrams are a property of the
  path, not of the client's address selection. The lane therefore now measures
  that path itself — a STUN Binding request from the host over the same
  connected-socket source the client picks — and waits, bounded, for an answer
  before handing the run to the clients, instead of reporting an unreachable
  coturn as a thirty-second `no candidate pairs` ICE failure.
- Stop a closing connection from writing the queue that sits behind a socket
  write abandoned in flight (issue #274). The live writer runs inside the close
  `select!`, so a close request cancels it wherever it is — including while a
  socket write owns one queued payload. That payload's wire position is then
  unknown, yet the graceful teardown kept draining everything queued behind it,
  which is how a recipient could observe a delivered sequence skipping one no
  `DeliveryReport` ever described (`expected 90, got 91` on the `Nextest
  (macos-latest)` lane). The remainder is now abandoned and counted instead:
  truncation at close is always legal, a hole never is. Loud slow-consumer
  teardown already abandoned its queue and is unchanged.
- Bind the kernel's own source address for every configured STUN/TURN server in
  the native reference client, alongside the enumerated interface addresses
  (issue #276). Interface enumeration reports only interfaces whose operational
  status is _up_, while reachability is a property of the routing table: a
  Docker bridge that is momentarily carrier-down — reproduced here as
  `NO-CARRIER ... state DOWN` on an `--internal` network — is invisible to
  enumeration while remaining the only address that network accepts. webrtc
  0.20 starts a binding request or allocation from _every_ bound socket, so
  when none of them routes to the server the session gathers no relay candidate
  at all, and under `--ice-transport-policy relay` no candidate whatsoever,
  which is how the TURN interoperability lane failed intermittently with only
  "no candidate pairs" as evidence. The source address is obtained by
  `connect()`ing an unbound UDP socket, which performs the route lookup without
  sending a packet; the two sets are unioned and filtered by the same rule, so
  a routing answer can never widen `--ip-family`, and `--cripple-ice` is
  exempt.
- Preserve deterministic `--cripple-ice` fallback on webrtc-rs 0.20 by keeping
  offer/answer and gathering lifecycles live while preventing both local and
  SDP-embedded remote candidates from forming an ICE pair.
- Make the WebRTC loss-recovery proof observable: after fault lift, every pair
  is rebuilt through the bounded PairRetry protocol, every peer must complete
  its exact reliable exchange on that clean generation, and only then does the
  harness release the exact unreliable exchange.
- Hold late-join WebRTC clients at a shared success barrier and compare each
  peer-status fan-out against its reporter's exact pre-teardown transition
  sequence, avoiding autonomous-exit races without weakening the oracle.
- Fix the codegen-facing AsyncAPI accountability envelopes (issue #261).
  Room snapshots, relayed game data, lifecycle watermarks, and reconnect replay
  now expose closed protocol-v2/protocol-v3 wire unions that reject impossible
  mixed or partial shapes while preserving the frozen protocol-v2 wire.
- Fix protocol-v3 peer transport health after room changes (issue #260). The
  first `TransportStatus` report in each seated membership now fans out even
  when it matches prior room or spectator state. Spectator entry and leave also
  reset deduplication so the next accepted report is counted again, but remain
  roomless and do not fan out;
  same-generation duplicates stay suppressed and protocol-v2 bytes unchanged.
- Reject clients whose declared protocol maximum is below the deployment
  minimum instead of silently upgrading them (issue #257). The authentication
  response now uses `UNSUPPORTED_PROTOCOL_VERSION`, including auth-disabled
  connections whose endpoint default is incompatible with a v3-only server.
- Keep rooms protected from garbage collection while a reconnect claim is in
  flight, even when the original reconnect window expires during restoration
  (issue #257).
- Detect required WebRTC data-channel close and error events in both reference
  clients (issue #257). A lost channel now tears down the unusable peer link,
  reports the transport disconnected, and engages the live WebSocket relay
  fallback even if the peer connection itself still says `connected`.
- Correct the protocol-v3 AsyncAPI and canonical samples (issue #257).
  Connection metadata and session plans now expose exact legal schema unions,
  nullable authority fields match their actual wire shape, and v3 room
  snapshots always pair `epoch` with the recipient-visible `seq` baseline.
- Fix `host + direct` plans that advertised an unusable peer-to-peer upgrade
  (issue #251). Direct selection now requires an endpoint-ready host, skips an
  authority that cannot anchor the connection, and carries the validated host
  and port in every authoritative v3 `SessionPlan`; reconnect and failover
  revalidate the replacement host, while the relay floor remains available.
  The endpoint remains self-declared and retains the legacy exposure boundary:
  room snapshots can reveal it to v2/v3 players and spectators, so it is not an
  authenticated identity or v3-only privacy boundary.
  Both reference clients now explicitly reject Direct execution they do not
  implement, report the failed transport, and engage that relay fallback.
- Enforce configured application room ownership and quotas (issue #249).
  Authenticated apps can no longer join, spectate, or reconnect into another
  app's persisted room; legacy unowned rooms are claimed only by a successful
  seated admission. `max_rooms` now applies atomically across game names and
  `max_players_per_room` caps creation and future seats without ejecting
  existing players. Auth-disabled rooms remain unowned, and ownership errors
  use the non-enumerating `ROOM_NOT_FOUND` response.
- Keep retry sleeps within the configured maximum after jitter is applied
  (issue #205). Persistent in-memory lock contention can no longer turn the
  default 5-second maximum into a 6-second sleep; sub-millisecond precision is
  preserved and extreme duration/factor combinations saturate safely.
- Make GitHub Release retries work after the default branch's workflow files
  advance. The release job now publishes the already-verified immutable tag
  without redundantly passing its historical source as `target_commitish`,
  skips identity mutation when the Release already exists, and retries SBOM or
  binary attachments through asset-only uploads. This avoids a workflow-write
  permission that `GITHUB_TOKEN` cannot receive. Registry retry validation also
  recognizes Cargo's canonical clean VCS metadata, where the `dirty` field is
  omitted, while still rejecting dirty or source-mismatched packages. Existing
  Releases must also retain the expected public state, exact notes, source
  revision, and image digest before any SBOM or binary asset is replaced. Asset
  uploads also name the repository explicitly, so split-checkout publication
  jobs never depend on an ambient Git worktree for repository discovery.
- Keep spectator-occupied rooms alive during garbage collection (issue #241).
  Spectator-only rooms now use the inactive-room timeout, spectator join,
  detach, and live connection traffic refresh room activity, and the normal
  empty-room timeout begins only after the final spectator leaves. Inactive
  cleanup also reconciles any removed room with the process-local spectator
  role during maintenance so the client can join again.

### Removed

- Removed the unused zero-copy serialization surface and the `rkyv` dependency
  (issue #296). `broadcast` and `rkyv_utils` were public modules that no
  production path called, and their advertised optimization could not work:
  `BroadcastMessage::get_or_serialize_rkyv`, `PreSerializedMessage::from_rkyv`,
  and `PreSerializedMessage::get_rkyv_bytes` returned
  `RkyvSerializeError::NotImplemented` unconditionally, and every crate doctest
  demonstrating them was `ignore`d. The `rkyv` derives on the protocol types
  were likewise never used to archive anything. Dropping them removes 11 crates
  from the shipped graph — `rkyv`, `rkyv_derive`, `bytecheck`,
  `bytecheck_derive`, `munge`, `munge_macro`, `ptr_meta`, `ptr_meta_derive`,
  `rancor`, `rend`, `simdutf8` — roughly 44,000 lines carrying ~1,347 `unsafe`
  occurrences, in a project that denies `unsafe_code` in every one of its own
  manifests. The direct `smallvec` dependency and its mock-only performance
  suite are removed too: `broadcast` held the only production small-vector
  player list, while that suite only characterized locally invented wrappers
  and third-party primitives, not a remaining Signal Fish path.
  **Breaking (Rust API):** the `broadcast` and `rkyv_utils` modules and the
  rkyv trait implementations on protocol types are gone, so the next release is
  at least `0.6.0`. No wire behavior changes: the `GameDataEncoding::Rkyv` wire
  token is a reserved/internal encoding the server relays opaquely and is pure
  serde, so it — and the browser client's `BinaryGameDataEncoding` union — are
  unaffected.

### Security

- Define the WebSocket application boundary as a public app-ID allowlist, not
  client authentication (issue #250). Canonical config is now
  `security.enforce_app_id_allowlist` plus `security.allowed_apps`; legacy
  `require_websocket_auth` / `authorized_apps` input remains accepted.
  Deprecated `app_secret` input is discarded and never retained or emitted.
  Ambiguous mixed-name sources fail closed, documented config precedence is
  enforced, and duplicate public IDs are rejected instead of using the last
  entry's limits.
  **Breaking (Rust API):** `SecurityConfig.require_websocket_auth`,
  `SecurityConfig.authorized_apps`, `ServerConfig.auth_enabled`, `AppAuthEntry`,
  `AuthMiddleware`, `AppInfo`, credential-validation methods, and
  `AuthError::InvalidCredentials` plus the public `*_app_info` helpers are
  replaced by the corresponding public-ID allowlist fields,
  `AppRegistrationEntry`, `AppIdAllowlist`, `AppContext`, `resolve_app_id`, and
  `*_app_context`; `AppIdAllowlist::new` now returns `Result` so duplicate IDs
  cannot construct an ambiguous policy. Frozen v2/v3 `Authenticate` wire bytes
  are unchanged.
- Prevent first-party production panic paths in the server and native reference
  client (issue #255). Invalid queue and host invariants fail closed, oversized
  batch/replay/serialization inputs are bounded or rejected, and runtime-less
  task starts return errors. The fail-closed repository gate covers both Cargo
  graphs, first-party unsafe Rust remains forbidden, Miri is gating, and
  AddressSanitizer covers both root and native libraries.

## [0.5.2] - 2026-07-31

### Added

- Enforce the no-unsafe property that the server and its reference clients
  already held by habit: every package manifest now declares an explicit
  `unsafe_code` lint policy (`forbid`, or `deny` for the Godot fixture that
  needs one `unsafe impl ExtensionLibrary` marker), and a git-discovery policy
  test fails when a package omits it (issue #205).

### Changed

- Install `rust-analyzer` with the pinned developer toolchain so VS Code's Rust
  extension works in compatible local and devcontainer environments without a
  separate component bootstrap.
- Replace permanently-zero minute/hour/day/reset/cache rate-limit telemetry
  with production-wired rejection counters for authentication, room creation,
  join attempts, signaling, and detailed signaling errors. The aggregate
  `signal_fish_rate_limit_rejections_total` remains stable and equals the sum
  of those five sources; the removed Prometheus series and JSON snapshot /
  dashboard fields were never connected to enforcement. The corresponding
  public `ServerMetrics` fields, `RateLimitWindow`, and dead record/cache
  methods are also removed, making this a Rust library API change.
- Remove the unused `AuthMaintenanceConfig`, root `Config.auth` field, and
  `EnhancedGameServer::new` argument. The three documented `auth.rate_limit_cache_*`
  settings never affected the bounded in-memory per-app limiter; legacy JSON
  and environment keys remain tolerated and ignored, while `--print-config`
  no longer advertises them. This is a Rust library API change.
- Correct the `max_signal_errors` contract: it budgets detailed rejected-signal
  responses before the server substitutes a generic rate-limit error; it does
  not stop validation, drop subsequent attempts, or disconnect the client.
- Keep the standalone native reference client and fuzz dependency graphs
  current and reproducible (issue #225). Dependabot now monitors both packages,
  the fuzz application commits a Rust 1.89-compatible lockfile, and every
  standalone update job that traverses the server path dependency must inherit
  all measured root version holds. Stable, nightly, cargo-deny, and scheduled
  cargo-audit gates now cover the root, native, and fuzz graphs; live policy
  tests also keep native banned-crate/source rules aligned while allowing only
  a narrower graph-specific license set.
- Define the server's single-home consistency and durability contract
  (issues #206 and #210): successful operations commit to one process's memory,
  reconnect restores current control state but never missed gameplay payloads,
  and drain or process loss requires clients to rebuild the room. Define the
  additional disconnect/outage exposure as the old queue tail plus every
  dequeued-but-client-unobserved pipeline frame plus traffic accepted while
  absent. Machine-check its conditional burst/rate/window ceiling without
  counting already-accounted delivery-class omissions or inventing a numeric
  guarantee from defaults that impose no room-wide gameplay admission bound.
- Reduce steady-state relay fan-out allocation operations by pre-sizing the
  recipient snapshot from room membership and reserving async wait machinery
  for recipients that are actually backpressured (issues #207 and #211).
  Current 2-, 8-, and 16-player fan-outs use 3, 4, and 4 allocation operations
  instead of 6, 7, and 7, while allocated bytes fall by 61–86% and the
  classified per-recipient queue remains allocation-free. Exact delivery,
  concurrent slow-recipient waits, configured grace deadlines, and wire
  behavior are unchanged.
- Reduce repeated socket serialization for relayed game data by sharing each
  exact v2/v3 text, binary, or mixed-format wire frame among compatible room
  recipients (issue #222). At 16 players, measured relay time improves by
  30.5% for JSON, 12.6% for MessagePack binary, and 25.9% for mixed
  MessagePack-to-JSON traffic; allocation operations per relay fall from 81 to
  12, 111 to 14, and 159 to 40 respectively. One-recipient performance and all
  emitted wire bytes remain unchanged.
- Count the teardown's final delivery report as its own registered close-write
  step, so `REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS` is 4 and the derived
  `registered_connection_shutdown_settle_timeout()` (and the binary's graceful
  shutdown budget) still covers the whole sequence. A wedged socket can therefore
  take one more `CONNECTION_CLOSE_WRITE_TIMEOUT` (1 s) to reclaim; a healthy one
  is unaffected.
- Require every workflow `apt-get update` to first drop the Azure CLI and
  Microsoft prod source lists, enforced by a sweep test over all workflows.
  Those mirrors periodically break `apt-get update` on GitHub runners; five call
  sites already did this by convention and a sixth did not.
- Symbolize AddressSanitizer/LeakSanitizer reports by locating the runner's
  `llvm-symbolizer` (rustup's `llvm-tools` component does not ship one). The
  lookup is best-effort and the diagnostic step now states whether
  symbolization was active, so an unsymbolized report is self-describing rather
  than an undiagnosable stack of raw addresses.
- Bump the pinned GitHub Actions group (`actions/checkout` 7.0.1,
  `taiki-e/install-action` 2.85.4, `docker/login-action` 4.6.0,
  `mozilla-actions/sccache-action` 0.0.11, and `actions/upload-artifact`),
  carrying Dependabot #202 and #215 forward.
- Refresh the compatible dependency set (Tokio 1.53.1, serde 1.0.229, futures
  0.3.33, clap 4.6.4, hdrhistogram 7.6.0 and others), including a coherent
  native-reference-client upgrade of the complete webrtc-rs family from 0.17.1
  to 0.17.2. The `tokio-tungstenite`
  0.30, `serial_test` 4.0, `base64` 0.23, and `syn` 3.0 declarations proposed
  alongside them are deliberately not taken: the first duplicates the
  Tungstenite stack Axum still pins to 0.29, the second requires rustc 1.93.1
  against a 1.89.0 MSRV, the third adds a second `base64` used by nothing but
  this crate, and the fourth removes `syn::Arm::guard` without deduplicating
  anything. The locked graph keeps exactly one WebSocket and one base64
  implementation. Dependabot #214 re-proposes all four; each rejection was
  re-measured against the current graph rather than carried forward on the
  earlier note — the PR's own lockfile adds `tokio-tungstenite`/`tungstenite`
  0.30 beside the 0.29 pair axum still requires and a second `base64`,
  `serial_test` 4.0.1 declares `rust-version = 1.93.1`, and compiling against
  `syn` 3.0 fails with `no field 'guard' on type '&Arm'` while syn 2 stays in
  the graph for `bytecheck_derive`, `derive_more-impl`, and `educe` regardless.
  Dependabot now holds those exact incompatible major/minor lines instead of
  reopening the same measured rejection while security advisory scanning
  remains active.

### Fixed

- Make release preparation include every tracked standalone Cargo graph that
  uses the local server package, while ignoring same-named registry packages.
  Patch, minor, and major preparation now resolve every graph, restore all
  release files after a failed postflight, and safely resume only while a
  matching release branch and pull-request head remain at the verified commit.
- Make room creation atomically require both its creation and shared join
  budgets, preventing partial accounting and `u32` overflow. Direct-library
  zero windows can no longer disable enforcement, cleanup duration arithmetic
  saturates, cleanup tasks no longer retain dropped limiters, expired debug
  stats are truthful without moving enforcement windows, and live subsecond
  retry durations round up instead of reporting zero seconds.
- Enforce WebSocket I/O deadlines as exclusive boundaries (issue #233).
  Selected outbound and server Ping writes are accepted only when the server
  observes completion strictly before the deadline. Authentication and
  authenticated-idle input are likewise accepted only when the server observes
  receipt strictly before the deadline; readiness at or after it cannot revive
  expired work. Existing `4002 slow_consumer`, `4003 activity_timeout`, `4001
  auth_timeout`, and `4004 idle_timeout` attribution remains unchanged, and an
  already-requested connection close retains precedence.
- Keep every control-message capacity wait bounded by the configured
  slow-consumer grace. Queue capacity that returns at or after the deadline can
  no longer revive an expired room-join/reconnect baseline, lifecycle
  notification, session plan, replan, or room transaction. Otherwise-live,
  still-eligible recipients whose timeout initiates closure now follow the
  documented `4002 slow_consumer` path.
- Fix false `4003 activity_timeout` disconnects for bandwidth-constrained
  recipients that are still draining application traffic (issue #217).
  Successful outbound application writes now supersede redundant WebSocket
  Pong deadlines while the Ping is still sent to refresh read-only clients;
  bounded queue sojourn and socket writes continue to close a genuinely stalled
  or reliably oversubscribed recipient with `4002 slow_consumer`.
- Make `server.ping_timeout = 0` actually disable the inbound-activity reaper as
  documented instead of expiring every registered client on the next cleanup
  sweep.
- Keep protocol-v3 delivery counters tied to frames successfully written and
  flushed to the recipient socket (issue #218). Cancelling or failing a queued
  `DeliveryReport` send no longer advances the counter frontier used by the
  teardown's final unsupported-format report, so that final report cannot
  inherit counters from a frame whose socket send/flush never completed.
- Stop unsupported-format accountability from evicting the recipient it exists
  to protect (issue #212). A binary payload that a peer's negotiated encoding
  cannot represent was reported with one `DeliveryReport` frame per omitted
  message, so a JSON peer in a MessagePack room paid **5.4x** the wire bytes of
  the compact frames its room-mates received (2,096,502 against 389,618 bytes
  for a 5,000-message burst). Under an equal 32 KiB/s bandwidth fault that
  difference is the difference between surviving and being disconnected as a
  slow consumer: the weaker peer was evicted after 703 of 5,000 messages while
  the compatible peer was unaffected. Consecutive omissions from one sender and
  delivery class now coalesce into one exact range under the same merge rule the
  queue already applied to its own gap reports, written before the next
  delivered frame, with the rate-limited advisory, immediately after a queued
  report, at most one second after the first omission when the recipient is
  otherwise idle, or after the teardown drain. The ranges are retired only once
  the frame is on the wire, so a write cancelled by the connection's close signal
  re-reports them instead of losing a whole coalesced burst.
  The same burst now costs 2,218 bytes in four
  reports (0.01x) with all 5,000 sequences still accounted for exactly, and the
  experiment passes under the single-core contention that reproduced the CI
  failure. Wire-visible for protocol v3 only: reports may now carry several
  `unsupported_format` ranges, which the documented "union of ranges covers
  every missing sequence" contract already required clients to handle. Both
  in-repo reference clients validated the old shape (`from_seq == to_seq` and a
  single gap per report) and would have rejected the server's own frames as
  accountability violations; they now validate the ranges, with tests proving a
  coalesced range is accepted only when the counters move by exactly the
  sequences it names. The **published** `signal-fish-client` 0.9.0 carries the
  same over-strict check and needs the same change
  (`signal-fish-client-rust` issue #81): `docs/protocol.md` has never promised a
  shape for these reports — it requires clients to authorize a hole from the
  union of ranges, and `DeliveryGap` is defined as an inclusive range — so the
  server now emits what the documented contract always allowed. Until a client
  release ships, a 0.9.0 client reports an accountability violation instead of
  recording the gap, and only on the error path that produces these reports at
  all: a convertible payload is still delivered as a JSON fallback with no
  report, so this needs an unrepresentable (reserved/`rkyv`) or malformed
  payload to trigger.
- Make the chaos proxy's bandwidth fault rate-accurate. Pacing slept a fixed
  interval per chunk, so the pump's own read/write/scheduling latency was added
  to every period and the achieved rate drifted below nominal under load — a
  "32 KiB/s" link delivered measurably less on a busy machine. Chunks are now
  released against a virtual clock with catch-up credit capped at one period, so
  a late iteration is absorbed rather than compounding and a long stall cannot
  burst. This does not resolve the pre-existing H14 slow-consumer flake tracked
  in issue #212; that experiment's assertion now reports its counters, elapsed
  time, and the server's own message instead of a bare code mismatch.
- Repair the load-test suite, which was measuring nothing. Every cell built a
  room code of the wrong length (10, 8, or 7 characters against the server's
  required 6), and `handle_join_room` reports a rejected join only by leaving
  the player roomless. The two throughput cells therefore recorded 0% success
  and had been excluded from CI as "known-broken" against an incorrect
  diagnosis, while the nightly latency cell passed while broadcasting into
  empty rooms. Codes now come from one checked helper, the room ceiling is
  sized for the cells that need it, and the cells assert that players actually
  entered a room. All three run in the nightly load lane (issue #207).

- Restore the weekly Firefox cell of the Fortress/Godot no-thread WASM
  interoperability gate, which had failed on every run since it was added. Two
  causes: Firefox's blocklist refuses a WebGL context without an accelerated
  adapter (`webgl.force-enabled`), and — unlike Chromium, which carries
  SwiftShader — Firefox has no software fallback of its own, so even headless it
  needs an X display to resolve a Mesa GL driver. The runner script now runs the
  Firefox harness under `xvfb-run`; the workflow additionally pins the Mesa/EGL
  packages as defensive hardening. Both browsers are probed for a WebGL2 context
  before the export loads, so the failure is reported with the browser's own
  reason, and a bounded wait for a page global now reports the page's captured
  errors instead of a bare timeout.

## [0.5.1] - 2026-07-24

### Changed

- Repackage the repository's AI-agent guidance as portable Agent Skills with
  discoverable metadata, colocated references and scripts, a generated catalog,
  and structural validation. Update every workflow, hook, test, and document
  that consumes the skill library to use the new layout.
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

### Fixed

- Disable Nagle's algorithm (`TCP_NODELAY`) on every accepted WebSocket socket,
  on both the plain and TLS serve paths, so small bidirectional relay frames are
  no longer stalled ~40-90 ms by the Nagle x delayed-ACK interaction on loopback
  (#197). The plain `axum::serve` path and the integration-test harness share one
  `bind_serve_listener` seam, and the TLS stack uses a matching
  `ConfiguredAcceptor`, so tests and production configure accepted sockets
  identically.
- Stop the outbound batch timer from adding a per-hop frame of latency to
  real-time relay traffic (#198). Batching is now opt-in
  (`websocket.enable_batching` defaults to `false`), and even when enabled only
  `latest` game data waits to coalesce same-key values — `reliable`, `volatile`,
  and control frames are released immediately. Integration testing measured
  round-trip latency fall from ~46 ms to ~12-20 ms; throughput deployments retain
  batching by opting in.

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
  immutable full-revision `sha-<40-character-commit>` tags from one verified
  digest, record that digest and source revision in the Release notes, fail
  closed on identity drift, and support safe retry completion plus tagged
  historical backfills.
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

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0

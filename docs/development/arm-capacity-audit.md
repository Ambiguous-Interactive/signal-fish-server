# ARM capacity campaign: audit and experiment ledger

Campaign: [#636](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636).
Plan: [C0–C5](../../PLAN.md#highest-priority--correctness-first-arm-capacity-campaign).
This ledger starts at `b24b5e13` (2026-09-27). It inventories code and
existing evidence. Inventory is **not** a correctness review or a capacity
measurement. Keep a row open until its invariants and missing cases have been
checked against the named revision.

## Audit method

Allowed states are **unreviewed**, **hypothesis**, **reproduced**, **fixed**,
and **deferred**. A deferred finding needs a linked issue. A reproduced finding
needs a deterministic test or equivalent direct proof. A fixed finding needs
the code revision and regression check. Existing green tests alone do not
advance a row. For each slice, inspect the named code, enumerate failure paths,
write a red test for any defect, sweep related paths, and record the green
result. Keep correctness findings separate from performance hypotheses.

Finding records use this form; assign IDs `ARM-C001`, `ARM-C002`, ... for
correctness and `ARM-P001`, `ARM-P002`, ... for performance:

| Field | Required content |
| --- | --- |
| ID, state, severity | Stable ID; one allowed state; critical/high/medium/low |
| Player impact | Concrete gameplay or resource effect; affected feature |
| Source and revision | Path, function/line, inspected commit |
| Invariant | Expected behavior and the observed violation or hypothesis |
| Confidence and reproduction | Direct proof or exact failing command, seed, setup, and output |
| Disposition | Fix/test commits or a linked issue with mitigation and owner |

No new finding is confirmed by this initial inventory.

## Correctness findings

### ARM-C001 — Spectator cap during room creation

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | Spectators can enter a new room before its configured cap is set. This can exceed the cap and grow spectator fan-out. |
| Source and revision | `src/database/mod.rs` room creation and `src/server/room_service.rs` cap application; `src/server/spectator_service.rs::join_owned`, reviewed at `684ca906`. |
| Invariant | A spectator must see the configured cap before admission. The room row is visible with `max_spectators=None` while the creator still holds the room-code lock. The old spectator path read that row without the lock. |
| Confidence and reproduction | Deterministic paused-create test: `scripts/dev-loop.sh spectator_join_waits_for_creation_cap_before_admission` failed before the fix with `spectator admission must wait until creation applies its cap`. |
| Disposition | The spectator path now holds the room-code lock until it enters the room event lane, then reads the room again before admission. The paused-create and lock-failure tests pass. Fix revision: `38e2e42d`. |

### ARM-C002 — Spectator admission on a retired room code

| Field | Record |
| --- | --- |
| State, severity | Fixed, high |
| Player impact | A spectator can enter a room with its old code after the authority rotates that code. |
| Source and revision | `src/server/spectator_service.rs::join_owned` and `src/server/moderation.rs::handle_regenerate_room_code_operation`, reviewed at `5a48ee29`. |
| Invariant | Rotation must finish after any old-code admission already in progress. The spectator path releases the old-code lock once it enters the room event lane; the old rotation path changed the code without entering that lane. |
| Confidence and reproduction | Paused in-lane room read: `scripts/dev-loop.sh rotation_waits_for_in_flight_old_code_spectator_admission` failed before the fix with `rotation must wait for the admitted old-code spectator`. |
| Disposition | Rotation now holds the room event lane across the storage swap, after acquiring both code locks. The regression checks old-code refusal, new-code admission, roster, and lock release. Fix revision: `8a22035b`. |

### ARM-C003 — Rotation waits on its own or a busy code lock

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A generated code equal to the room's current code makes rotation wait on its own lock and fail. Two rotations can wait on each other's old codes. |
| Source and revision | `src/server/moderation.rs::handle_regenerate_room_code_operation`, reviewed at `5a48ee29`. |
| Invariant | Candidate selection must make bounded progress while the old-code lock is held. The old path waited for each candidate lock, including its own. |
| Confidence and reproduction | Scripted `OLDCOD`, then `NEWCOD`: `scripts/dev-loop.sh regenerate_skips_own_or_busy_code_before_waiting_for_a_second_lock` failed before the fix with `rotation must skip OLDCOD without waiting`. The test also holds a candidate lock to model the cross-rotation wait. |
| Disposition | Rotation skips its own code and busy candidate locks within the existing eight-attempt budget. Both scripted cases pass. Fix revision: `8a22035b`. |

### ARM-C004 — Spectator admission after shutdown drain

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A spectator can join during shutdown after waiting for a room lock or response capacity. The new role is short lived and can reach peers during teardown. |
| Source and revision | `src/server/spectator_handlers.rs::handle_join_as_spectator_operation` and `src/server/spectator_service.rs::join_owned`, reviewed at `1e42cf07`. |
| Invariant | Shutdown drain refuses new spectator roles. The handler checked drain only before the owned admission, which can wait at the room-code lock or baseline delivery. |
| Confidence and reproduction | The initial `spectator_join_waiting_on_room_code_refuses_shutdown_drain` test failed before the fix: the join returned `SpectatorJoined` after drain. Its deterministic replacement is `scripts/dev-loop.sh spectator_join_rechecks_drain_after_code_lock`; companion tests trigger drain after storage add and during a blocked baseline. |
| Disposition | The service rechecks drain inside the room lane, rolls back a durable add if drain starts during storage, and cancels a blocked baseline through the drain signal. The focused tests pass in this change (#647). |

### ARM-C005 — Reconnect admission after shutdown drain

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A reconnect can restore a seat and spend its one-time token after shutdown drain starts. The forced close then removes that short-lived seat. |
| Source and revision | `src/server/reconnection_service.rs::handle_reconnect_owned` and `src/server.rs::register_local_client_with_initial_message_async`, reviewed at `07952b86`. |
| Invariant | Drain refuses reconnects until the `Reconnected` baseline is queued. Rejection rolls back the seat and releases the token. A queued baseline commits the reconnect. |
| Confidence and reproduction | `reconnect_restoring_membership_refuses_shutdown_drain_and_releases_token` failed before the fix: a paused membership write resumed after drain and returned success. `reconnect_waiting_for_baseline_capacity_refuses_shutdown_drain` covers the blocked response queue. |
| Disposition | Reconnect rechecks drain after the room gate and durable add, cancels a blocked baseline when drain starts, and serializes baseline enqueue with the drain transition. Both regressions pass (#647). |

### ARM-C006 — Reconnect can restore a duplicate player name

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | Another player can join with a disconnected player's name, then the old player can restore the same name through reconnect. |
| Source and revision | `src/server/room_service.rs` join name validation, `src/server/reconnection_service.rs` restore, and `src/database/mod.rs::add_player_to_room`, reviewed at `07952b86`. |
| Invariant | Seated player names must remain unique under the join path's canonical comparison. |
| Confidence and reproduction | `reconnect_name_taken_by_new_member_rejects_without_spending_token` failed before the fix: reconnect restored `Straße` beside a seated `STRASSE`. |
| Disposition | Reconnect now checks the saved name under the room event gate before restoring membership. A conflict returns `ReconnectionFailed` and leaves the token usable until its window expires (#647). Capacity and names are not reserved during disconnect. |

### ARM-C007 — Room creation can publish after shutdown drain

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A creator can receive `RoomJoined` after shutdown drain starts and enter a room that is about to close. |
| Source and revision | `src/server/room_service.rs::handle_join_room_owned` and its async baseline commit, reviewed at `fd18c6dc`. |
| Invariant | A new room must not commit its first join after drain starts. Existing-room joins remain allowed during drain. |
| Confidence and reproduction | `scripts/dev-loop.sh draining_room_creation_cancels_baseline_before_it_is_queued` failed before the fix: a paused baseline room read resumed after drain and delivered `RoomJoined`. The test covers explicit and generated room codes. |
| Disposition | Created-room baseline enqueue now shares the shutdown drain commit gate. A canceled baseline rolls back the unpublished room and returns `SERVER_DRAINING` (#647). |

### ARM-C008 — Creator name write failure can publish the placeholder name

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A room creator requesting a display name can receive `RoomJoined` with the stored placeholder `Creator` if the name write fails or reports a missing row. |
| Source and revision | `src/server/room_service.rs` creator admission and `src/database/mod.rs::update_player_name`, reviewed at `6877e4b0`. |
| Invariant | A successful creator join publishes the requested name. A failed name write refuses the join and releases the unpublished room and its code. |
| Confidence and reproduction | `creator_name_failure_refuses_creation_and_allows_retry` failed before the fix with `RoomJoined` and `Creator`; `creator_name_missing_row_refuses_creation_and_allows_retry` covers `Ok(false)`. Both verify room rollback and a successful retry with the requested name. |
| Disposition | Creator admission now requires a confirmed name write. On storage failure or missing row it rolls back the unpublished room and returns `ROOM_CREATION_FAILED` (#647). |

### ARM-C009 — Failed spectator-cap write can publish an unlimited room

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A storage failure while setting a new room's spectator cap can leave it unlimited despite a positive deployment cap. Spectators can then exceed the configured limit. |
| Source and revision | `src/server/room_service.rs` creation-time cap write and `src/database/mod.rs::set_room_max_spectators`, reviewed at `6d2b3eb2`. |
| Invariant | A creator join cannot publish a room with a cap different from the deployment policy. Explicit `0` is the unlimited opt-out. |
| Confidence and reproduction | `spectator_cap_write_failure_refuses_creation_and_allows_retry` failed before the fix with `RoomJoined`. The green test checks room/code rollback, retry, and one-spectator enforcement. `explicitly_unlimited_spectators_need_no_cap_write` covers the opt-out. |
| Disposition | A failed positive cap write refuses creation. The in-memory backend now hides the pending room and retries deletion if rollback fails (ARM-C013). Other backend and lifecycle cases remain in [#658](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/658). |

### ARM-C010 — Old peer metadata can overwrite a rejoined seat

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | Peers can receive a former endpoint at game start after a player leaves and rejoins the same room. |
| Source and revision | `src/server/game_data.rs::handle_provide_connection_info` and `src/database/mod.rs::update_player_connection_info`, reviewed at `5345b9ec`. |
| Invariant | A metadata write from the old seat must finish before that player can leave and rejoin. The old handler read the room, then awaited storage without the client lifecycle gate. A new seat with the same player ID could receive the old write. |
| Confidence and reproduction | `scripts/dev-loop.sh old_connection_info_cannot_overwrite_rejoined_seat` failed before the fix: the rejoined row contained `Direct { host: "old-endpoint", port: 7777 }`. The deterministic pause is at the storage write. |
| Disposition | The handler now holds the client lifecycle gate through the metadata write. The regression passes with the new seat's metadata unset (#647). |

### ARM-C011 — Reconnect can restore an old Direct endpoint

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | After a network change, peers can receive the disconnected socket's old Direct host endpoint in a new `SessionPlan`. |
| Source and revision | `src/reconnection.rs::register_disconnection_with_identity`, `src/server/reconnection_service.rs` restore, and `src/server/session_policy.rs` host selection; reviewed at `e615b0da`. |
| Invariant | A new socket must advertise its own peer endpoint. The old disconnect snapshot kept `PlayerInfo.connection_info`, and reconnect restored it unchanged. |
| Confidence and reproduction | `scripts/dev-loop.sh reconnect_preserves_join_time_but_clears_old_peer_endpoint` failed before the fix: the restored row retained `Direct { host: "old-network.example", port: 7777 }`. The test also checks the saved join time and name. |
| Disposition | Disconnect registration now clears the old socket's peer metadata while retaining identity and replay state. The reconnect and duplicate-registration regressions pass. Existing Direct host plan tests verify that a host without an endpoint is re-elected or falls back to relay (#647). |

### ARM-C012 — Old transport status can reach a rejoined seat

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | Peers can receive a stale `PeerTransportStatus` after its sender leaves and rejoins. This can show an old direct or WebRTC path as live. |
| Source and revision | `src/server/message_router.rs::TransportStatusFanOut::deliver`, reviewed at `55a2b3a2`. |
| Invariant | A status from one membership generation must not publish in a later generation. The old fan-out rechecked recipients but not the sender. |
| Confidence and reproduction | `scripts/dev-loop.sh transport_status_delivery_does_not_block_concurrent_leave` failed before the fix with `old status reached the new seat` after a paused delivery resumed following leave and rejoin. |
| Disposition | Fan-out now checks the sender's room membership generation at recipient queue commit, after any capacity wait. A canceled event does not increment the fan-out counter. The focused regression and transport-status tests pass (#647). |

### ARM-C013 — A failed room rollback leaves a joinable unfinished room

| Field | Record |
| --- | --- |
| State, severity | Fixed for the in-memory backend, medium; broader #658 remains open |
| Player impact | If creator setup and deletion both fail, another player can join the unfinished room. A failed spectator-cap write then leaves that room unlimited. |
| Source and revision | `src/server/room_service.rs` unpublished admission rollback and `src/database/mod.rs` room lookup, reviewed at `c85ee60b`. |
| Invariant | A newly created room is invisible until its creator response commits. An abandoned room reserves its code until storage confirms deletion. |
| Confidence and reproduction | `scripts/dev-loop.sh failed_cap_write_and_delete_keep_unpublished_room_closed` failed before the fix: the next join received `RoomJoined` for the unfinished room. The green test checks a second server instance, spectator refusal, repeated repair failure, one deletion count, and code reuse after repair. Name-write and failed-read regressions cover sibling rollback paths. |
| Disposition | In-memory creation starts pending. The response builder publishes while holding the room code and mutation locks, before the queue commit; cancellation rolls back under both locks. Failed rollbacks are marked abandoned, and maintenance retries deletion by ID after checking that state again. The trait default now refuses server creation without a pending lifecycle and refuses direct protected creation without an atomic seal. Process-loss behavior and other #658 acceptance cases remain open. |

These findings cover room-code rotation, player names, transport status, and spectator,
reconnect, and room-creation drain seams. The rest of the C1 room and storage
rows remain unreviewed.

## Coverage ledger

All rows were inventoried at `b24b5e13`. Their reviewed revision is **none**
until a C1 audit records one. Paths identify the review seam;
listed tests, models, and fuzz targets are leads, not completed reviews.
Check default, `tls`, `legacy-fullmesh`, `trace-validation`, and
`allocation-tracking` feature combinations where applicable (`Cargo.toml`).
The library modules exported by `src/lib.rs`, the binary, and shipped reference
clients are covered below.

Public entry points to check: `src/main.rs` builds the root listener, including
`/v2` routes, `/v3/client-config`, `/health`, `/readyz`, and metrics endpoints.
`src/websocket/routes.rs` builds `/ws`, `/client-config`, `/health`, `/readyz`,
`/metrics`, `/metrics/prom`, `/v3/ws`, and `/v3/client-config`; its router is
nested under `/v2` by the binary. The binary also has a legacy listener path.
The library API surface is the public modules exported from `src/lib.rs`:
`auth`, `config`, `coordination`, `database`, `distributed`, `logging`,
`metrics`, `protocol`, `rate_limit`, `reconnection`, `retry`, `security`,
`trace_validation`, `server`, and `websocket`. Each remains unreviewed.

Feature coverage also starts unreviewed. Exercise `default=[]`, `tls`,
`legacy-fullmesh`, `tls,legacy-fullmesh`, and `--all-features`. The legacy path
is for local interop and has a separate security posture. `trace-validation`
is an internal verification seam; `allocation-tracking` is a development
benchmark seam. Check those two in the relevant tests and all-feature build;
neither is a deployed capacity preset.

| Subsystem and paths | Invariant to check | Existing evidence lead | Missing cases / next check | State |
| --- | --- | --- | --- | --- |
| Startup and CLI: `src/main.rs`, `src/lib.rs` | Startup rejects bad config; startup failure leaves no listener | `tests/config_and_endpoints_tests.rs` | Failure after partial startup; feature matrix | Unreviewed |
| Config and reload: `src/config/**` | Defaults, validation, and reload preserve one coherent policy | `tests/config_and_endpoints_tests.rs`, `tests/config_validation_coverage_scan.rs` | Key/allowlist swap order and invalid reload | Unreviewed |
| Authentication: `src/auth/**`, `src/rate_limit.rs` | Unauthorized traffic cannot enter a room; limits count refusals | `tests/auth_integration_tests.rs`, `formal/tla/RateLimitWindow.tla` | Concurrent admission and auth timeout boundary | Unreviewed |
| Security: `src/security/**`, `src/websocket/token_binding.rs` | Token, origin, TLS, and TURN credential checks fail closed | `tests/mtls_token_binding_e2e.rs`, `fuzz/fuzz_targets/fuzz_reconnect_tokens.rs` | Token rotation/expiry during claim; TLS variants | Unreviewed |
| Protocol: `src/protocol/**`, `src/trace_validation.rs` | V2/V3 decoding, wire bytes, and delivery class match contract | `tests/v2_wire_golden.rs`, `tests/v3_wire_properties.rs`, `fuzz/fuzz_targets/decode_protocol.rs` | Malformed/deep frames, mixed format boundaries | Unreviewed |
| Room and player storage: `src/database/**` | Membership and room limits stay atomic and app isolated | `tests/integration_tests.rs`, `tests/model_based_state_machines.rs` | Concurrent joins at both limits; rollback | Unreviewed |
| Room lifecycle and moderation: `src/server/room_service.rs`, `moderation.rs`, `spectator_service.rs`, `spectator_handlers.rs` | Join, leave, kick, ban, spectator state and ownership agree | `tests/lobby_integration_tests.rs`, `src/server/room_service_tests.rs` | ARM-C001–C004 fixed in spectator and room-code seams; join-only, leave/disconnect, kick/ban, and authority races remain | Unreviewed |
| Readiness and gameplay: `src/server/ready_state.rs`, `authority.rs`, `session_policy.rs`, `signaling.rs` | Membership and transport changes invalidate stale plans/readiness | `tests/v3_session_plan_e2e.rs`, `formal/tla/SignalFishSession.tla` | Start/leave, authority loss, reconnect publication order | Unreviewed |
| Relay routing: `src/server/game_data.rs`, `message_router.rs`, `messaging.rs`, `relay_policy.rs` | Each permitted message reaches only valid peers with correct sequence/class | `tests/v3_game_data_sequencing_e2e.rs`, `tests/mixed_encoding_relay_e2e.rs` | Mixed conversion refusal; stalled room fairness | Unreviewed |
| Coordination and queues: `src/coordination/**`, `src/distributed.rs` | Transaction and queue failure is explicit; one room cannot strand another | `tests/relay_backpressure_e2e.rs`, `formal/tla/RoomMessageTransaction.tla` | Cancellation/panic at reservation and commit | Unreviewed |
| WebSocket ingress and egress: `src/websocket/**` | Bounded frames, priority control, close and drain semantics hold | `tests/transport_frame_limits_e2e.rs`, `tests/slow_consumer_no_cascade_e2e.rs` | Slow reader, batching age, TLS close paths | Unreviewed |
| Reconnect and retry: `src/reconnection.rs`, `src/retry.rs`, `src/server/reconnection_service.rs` | Claims have one owner; replay and stale routes cannot leak or misroute | `tests/reconnect_window_races_e2e.rs`, `formal/tla/ReconnectionClaimLifecycle.tla` | Simultaneous claim, expiry, failed restore/retry | Unreviewed |
| Maintenance and deadlines: `src/server/maintenance.rs`, `heartbeat.rs`, `dashboard_cache.rs`, `src/deadline.rs` | Expiry and cleanup are bounded; live state survives sweeps | `formal/tla/RoomLifecycleGC.tla`, `tests/clock_source_scan.rs` | Exact expiry boundary; churn growth; dashboard cost | Unreviewed |
| Metrics and logging: `src/metrics.rs`, `src/logging.rs`, `src/websocket/metrics.rs`, `prometheus.rs` | Counters report outcomes; labels and logs stay bounded and safe | `tests/config_and_endpoints_tests.rs`, `tests/websocket_test_helpers/prometheus_scrape.rs` | Cardinality and logging pressure under floods | Unreviewed |
| Admin and shutdown: `src/server/admin.rs`, `shutdown.rs`, `connection_manager.rs` | Drain closes all owned tasks and reports queued work accurately | `tests/close_code_semantics_e2e.rs`, `formal/tla/ConnectionTeardown.tla` | Drain racing claims, queued reliable data, panic | Unreviewed |
| Browser client: `clients/browser/src/**` | Reconnect, delivery reports, fallback, and negotiation match server | `clients/browser/src/page/*.test.ts` | Browser network fault and client revision matrix | Unreviewed |
| Native client: `clients/native/src/**` | Same client contract across native sockets | `clients/native/tests/interop_e2e.rs` | Restore and mixed-encoding error paths | Unreviewed |
| Fortress clients: `clients/fortress/src/**`, `clients/fortress-wasm/src/**` | Reference peers handle relay and fallback without silent loss | `clients/fortress/README.md`, `clients/fortress-wasm/README.md` | Cross-stack fault and resource cases | Unreviewed |

`src/server.rs` owns shared server state across the server rows. `src/websocket/routes.rs`
and `src/main.rs` own the plain/TLS listener boundary. `src/config/coordination.rs`
and `src/distributed.rs` provide local coordination seams; they do not establish
cross-node room replication. External mobile/Steam clients, operated TURN, and
AWS hardware are outside this code inventory and need separate evidence.

## Existing measurement evidence and limits

- `tests/websocket_test_helpers/delivery_ledger.rs` checks exact receiver,
  sender, and sequence outcomes or an explicit disconnect. The V3 tests and
  `tests/multiprocess_delivery_e2e.rs` provide real-socket regression leads.
  `formal/tla/**`, `formal/z3/**`, and `fuzz/fuzz_targets/**` explore related
  state spaces. None substitutes for an executable regression at a found seam.
- `tests/load_tests.rs` runs in-process smoke workloads. The shared-process
  H2 numbers in [Scaling](../architecture/scaling.md#size-the-relay-floor)
  include the generators on the same runner. Neither measures the standalone
  ARM server ceiling or proves scheduled-send latency under overload.
- `benches/relay_allocations.rs`, `relay_serialization_allocations.rs`, and
  `relay_serialization_runtime.rs` isolate relay costs and preserve exact-wire
  and delivery checks. They do not include full socket, TLS, kernel, or
  standalone-server costs. Keep their exact-byte and ledger checks in later
  comparisons.
- [#207's allocation profile](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/207#issuecomment-5489028363)
  found 0 queue allocations and 1 fan-out core allocation per relay in its
  measured layers. Its stored-baseline Criterion deltas had about 25% noise.
  Neither result is a whole-server ceiling.
- [#636's follow-up](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636#issuecomment-5843564707)
  rejected extra JSON buffer reservation, cache-hit clone changes, and
  recipient-vector work without new hot-path evidence. Its
  [arm64 microprofile](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636#issuecomment-5851524523)
  used a 12-core host and 1,024-relay microbench; socket and syscall costs
  were outside that scope. [PR #644's result](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636#issuecomment-5852268752)
  already reduced mixed-room duplicate JSON body encoding from two passes to
  one. Do not register that candidate again as pending work. These are
  historical findings, not measurements from this ledger revision.

## Experiment contract

Register an immutable record **before** each measured comparison. Store the
record and raw artifacts under a run ID outside the source tree; link them from
the finding or campaign issue. A result without the manifest, complete attempts,
and outcome ledger is provisional.

| Field | Required value |
| --- | --- |
| Identity | Run ID, schema version, hypothesis, decision owner, registration time |
| Revisions | Baseline and candidate SHAs, binary/config hashes, features, toolchain |
| Workload | Endpoint, seed, protocol/encoding mix, room/player counts, payload bytes, per-sender rate, delivery classes, warm-up, duration, churn/reconnect schedule |
| Environment | ARM CPU model, kernel, two-CPU affinity and quota, 4-GiB cgroup limit, swap policy, socket limits, TLS path, generator host/resources and network path |
| Primary measure | Scheduled application send to recipient receipt p99; pass at <= 50 ms for relay cells |
| Safety and validity | Exact delivery-class outcomes, all offered work, queue bounds, generator lag, clock error or same-clock method, memory limit, disconnects |
| Comparison | Predeclared run order, all attempts, raw interval/histogram/log artifacts, paired result, uncertainty, accept/reject/defer reason |

Measure idle density, churn, active relay, and mixed loads separately. Never
compare unsynchronized host timestamps to the one-way 50-ms target. Increase
load only after generator negative controls pass. Keep load generators outside
the server's CPU and memory limits. Record unavailable counters and invalid
runs; do not omit them. AWS validation remains required for deployment claims.

## Next two PR contracts

**C1 first slice: identity and membership ([#647](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/647)).**
Start with concurrent joins at
room and app limits, `join_only` stale-directory behavior, and leave/disconnect
races. Inspect `src/database/mod.rs`, `src/server/room_service.rs`,
`src/server/connection_manager.rs`, `src/server/message_router.rs`, and
`src/websocket/connection.rs` at the
reviewed SHA. Use barriers or paused time to reproduce any violated invariant;
test both accepted and refused outcomes, cleanup, and application isolation.
Update the relevant rows and finding records. A confirmed player-impacting
defect blocks capacity tooling until fixed or separately tracked with a
mitigation.

**C2 first runner PR: standalone real-socket foundation
([#648](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/648)).**
Accept endpoint,
seed, room/player count, protocol/encoding, payload bytes, sender rate,
delivery class, warm-up, duration, churn/reconnect schedule, and output
directory. Spawn or connect to
a release server process; keep generators outside its resource limits. Use a
single monotonic clock for scheduled send and receipt pairs. Emit a manifest,
interval samples, latency histogram, exact per-recipient outcomes, unsent and
outstanding work, and server/generator resource diagnostics. A small reliable
relay scenario must pass. Inject missing, duplicate, misrouted, and delayed
deliveries, a slow reader, generator saturation, and server termination; each
must fail or invalidate the run with an explicit reason. Replaying artifacts
must reproduce the outcome summary. Do not add a production wire field.

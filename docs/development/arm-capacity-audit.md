# ARM capacity campaign: audit and experiment ledger

Campaign: [C0–C5 (#636)](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636).
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

### ARM-C014 — A creator setup panic leaves a pending room outside repair

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A storage panic after room insertion can reserve its code and room quota until process exit. The room remains invisible, so creators cannot use that code. |
| Source and revision | `src/server/room_service.rs` creator cap and name setup, reviewed at `49497a44`. |
| Invariant | A failed creator setup must delete the pending room or mark it abandoned for repair before the creation lock is released. The outer panic supervisor lacks the room ID until setup returns. |
| Confidence and reproduction | `scripts/dev-loop.sh creator_setup_panic_keeps_pending_room_repairable` failed before the fix: injected cap-write panic left no abandoned room for cleanup. The green test covers cap and name panics followed by a delete failure. |
| Disposition | Creator setup catches a storage panic while it still owns the room ID, rolls back or marks the room abandoned, and returns a creation failure so the locks release. The focused regression checks the response, metrics, repair, and retry. Interrupted creation after insertion is covered by ARM-C015. |

### ARM-C015 — An interrupted creator reserves a hidden room indefinitely

| Field | Record |
| --- | --- |
| State, severity | Fixed for the in-memory backend, medium |
| Player impact | If the creator stops after the atomic pending insert and before rollback, its hidden room consumes quota and reserves the requested code until shutdown. |
| Source and revision | `src/database/mod.rs` pending creation and `src/server/maintenance.rs` repair, reviewed at `630b5d3e`. |
| Invariant | Repair may delete a Creating room only after its creator operation ends. A lost room-code lease alone does not prove the creator stopped. General room expiry must leave every unpublished room to the dedicated repair path. |
| Confidence and reproduction | `interrupted_creation_releases_hidden_room_and_reserved_code` failed before the fix because maintenance scanned only Abandoned rooms. The green test drops the creator token, lets its code lock expire, then checks deletion, quota, metrics, and code reuse. `pending_room_repair_preserves_active_creator_after_code_lease_expires` keeps the token alive past lock expiry and confirms publication succeeds. `generic_room_cleanup_keeps_unpublished_rooms_for_repair` covers both general reapers. |
| Disposition | The in-memory insert stores the original creator ID and a weak operation token under the same commit as the room, code, and pending marker. The response path holds the token through first publication. Repair takes the code lock and atomically rechecks the marker, owner liveness, and creator-only membership before deletion. A process restart drops all in-memory rooms and codes; durable adapters must provide their own process-loss recovery. |

### ARM-C016 — A creator insert panic leaves lifecycle counters unbalanced

| Field | Finding |
| --- | --- |
| State, severity | Fixed for the in-memory backend, low; broader #658 remains open |
| Player impact | Dashboards undercount rooms and joined players when storage inserts a pending room and panics before the server resumes. Later repair can count deletion without a matching creation or departure. |
| Source and revision | `src/database/mod.rs` pending insertion and deletion; `src/server/room_service.rs` admission rollback, reviewed on this branch. |
| Invariant | A committed pending insert counts one room and creator before another suspension point. Removing that room counts the creator leaving exactly once, including direct deletion and repeated repair. |
| Confidence and reproduction | `panic_after_pending_insert_balances_creation_metrics_once` failed before the fix with `rooms_created=0` after storage committed the pending room. It now covers failed then successful repair and all four counters. `direct_pending_room_delete_balances_creator_metrics_once` checks direct deletion and repeated leave accounting. |
| Disposition | The in-memory insert records creation and join while holding its commit guards. Rollback and deletion share one atomic accounting token for the departure. The trait compatibility default cannot provide the same guarantee for other backends; durable process-loss recovery remains in #658. |

### ARM-C017 — Kick loses a reconnected seat during a second disconnect

| Field | Finding |
| --- | --- |
| State, severity | Fixed, high |
| Player impact | A kicked player can retain a fresh reconnection record and restore the seat after the authority receives `PlayerKicked`. |
| Source and revision | `src/server/moderation.rs` disconnected-target eviction, reviewed at `3ed1c337`. |
| Invariant | A kick either removes the restored live seat while holding its current lifecycle gate or tombstones that room's pending record under the room event gate. A second disconnect cannot re-arm a restorable seat before removal, and the kick cannot revoke a newer credential for another room. |
| Confidence and reproduction | `kick_cannot_leave_a_fresh_reconnect_record_after_target_disconnect` failed before the fix with an untombstoned pending record. It pauses after kick validation, reconnects the target, then pauses after kick reads the restored route while the target disconnects. The green test verifies that disconnect cannot arm another record before removal and that no claimable record remains. `kick_does_not_tombstone_a_new_pending_record_in_another_room` checks a newer room B record against kick's stale room A validation. Existing active-kick and pending-seat tests remain green. |
| Disposition | Eviction rejects a stale target lifecycle guard after waiting, then rechecks the current lifecycle after its first tombstone. It holds a restored socket's lifecycle gate through removal, or rechecks and tombstones a pending record under the room gate if no socket exists. Both tombstone writes require a fresh same-room record check. |

### ARM-C018 — Cyclic moderation waits on lifecycle locks

| Field | Finding |
| --- | --- |
| State, severity | Fixed defensive lock-order gap; reachable player impact unconfirmed |
| Potential impact | If live authorities have cyclic stale storage rows, simultaneous kick or ban requests can wait forever on each other's lifecycle locks. |
| Source and revision | `src/server/moderation.rs::resolve_kick_style_target` and `evict_member_by_authority`, reviewed at `e4ae5526`. |
| Invariant | Moderation operations must not hold lifecycle locks in a wait cycle. A late reconnect can create the target lifecycle after initial validation, so both acquisition sites need the same coordination. |
| Confidence and reproduction | A synthetic cross-room stale-row test, `crossed_stale_seat_moderation_completes_without_lifecycle_deadlock`, failed before the fix: both authority locks were held and the first kick timed out after one second. It covers kick/kick and kick/ban pairs; a three-way test covers longer cycles. The shipped in-memory failure path does not establish this exact crosswise state: `leave_storage_error_preserves_membership_routing_and_reconnect_token` keeps the route, `disconnect_storage_error_forces_terminal_teardown_and_keeps_claim_reachable` leaves a room-bound reconnect record, and `disconnect_storage_error_retries_without_reconnection_support` checks repair. No observed player incident is claimed. |
| Disposition | One moderation lifecycle gate serializes kick and ban while they may hold two lifecycle locks, including the late target reacquisition. It releases before the terminal result send. The synthetic regression and existing moderation cases check completion and preservation of unrelated live memberships. Other client-lifecycle users take one lifecycle lock per operation. |

### ARM-C019 — Failed reconnect keeps an unrouted authority

| Field | Finding |
| --- | --- |
| State, severity | Fixed, high |
| Player impact | A failed reconnect can leave its unrouted player as room authority while storage removal is unavailable. Live members cannot take authority until detach repair succeeds. |
| Source and revision | `src/server/reconnection_service.rs::rollback_claimed_reconnect`, reviewed at `1be93b32`. |
| Invariant | A reconnect that cannot deliver its baseline must release any authority it gained, even if its restored membership row cannot yet be removed. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(failed_reconnect_rollback_does_not_leave_an_unrouted_authority)'` failed before the fix: the room retained `authority_player: Some(reconnecting)` after baseline delivery and rollback removal failed. |
| Disposition | Rollback now clears authority when membership removal fails and keeps the failed detach queued. The regression checks that a live member can claim authority before repair, repair removes the row, and the original token can retry (#647). |

### ARM-C020 — Stale terminal unroute deletes a newer route

| Field | Finding |
| --- | --- |
| State, severity | Fixed at the coordinator seam, medium |
| Potential player impact | If a stale room-A terminal unroute followed a move to room B, room B could stop receiving broadcasts. A move to the roomless lobby could lose directed responses. The reviewed shipped socket paths exclude this order. |
| Source and revision | `src/server.rs::unroute_local_client_with_tail` and `src/server/connection_manager.rs::clear_room_assignment_with_tail`, reviewed at `b7e11dea`. |
| Invariant | Refusal to clear a foreign room assignment must preserve the current room route or roomless delivery handle. The old coordinator swept every room route and removed the handle even when the assignment callback returned no terminal tail. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(stale_terminal_unroute_preserves_new_room_route)'` failed before the fix with `left: Some([]), right: Some([player_id])` after room A to B. The same test covers A to roomless after the fix. |
| Shipped-flow reachability at `8d47df25` | Excluded for the reviewed WebSocket paths. `leave_room_locked_operation` is the only server caller of terminal unroute. Explicit leave holds its source `ClientLifecycle` gate through that call; disconnect and kick/ban hold the same player's gate when they call leave. Join and spectator admission also hold that gate, so they cannot publish another role first. Disconnect removes the old connection only after terminal unroute; reconnect refuses a target ID while that entry exists. Owned tasks retain the gate if their caller is cancelled. Missing-room maintenance uses the same gate, and shutdown uses quiet unregister without terminal unroute. The room A to B/lobby order in the coordinator regression is built with lower-level test hydration APIs, not a shipped socket request. This proof does not cover direct embedder use of public test-hydration or no-lifecycle unregister APIs. |
| Disposition | A terminal unroute with no tail now removes only the named room route and leaves the direct delivery handle until explicit unregister. `stale_terminal_unroute_preserves_new_room_route` and `leave_from_old_socket_cannot_use_replacement_lifecycle` passed at `8d47df25`. The defensive coordinator fix stays in place; [#685](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/685) can close as a documented shipped-flow exclusion. |

### ARM-C021 — Removed socket leave departs a restored player

| Field | Finding |
| --- | --- |
| State, severity | Fixed at the server operation seam, high |
| Player impact | An old leave can remove a player's restored room seat and send a second departure event after reconnect. |
| Source and revision | `src/server/room_service.rs::leave_room_owned`, `src/server/message_router.rs`, and `src/websocket/connection.rs`, reviewed at `5b79ac98`. |
| Invariant | An old socket's leave must not act on a lifecycle later installed under the same player ID. The old missing-lifecycle fallback could reach the restored seat. A frame already in the receive task can also resume after its send task unregisters and capture the new lifecycle by ID. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(leave_from_removed_socket_cannot_depart_reconnected_lifecycle)'` failed before the fix: the old leave paused after observing no lifecycle, reconnect restored the seat, and the leave removed it (`left: None`, `right: Some(room_id)`). `leave_from_old_socket_cannot_use_replacement_lifecycle` checks the stale physical socket Arc through the owned transaction and both router leave forms. These are server tests; the WebSocket ordering follows from its independent send and receive tasks and the router's await before dispatch. |
| Disposition | The missing-lifecycle branch returns. WebSocket dispatch now carries its socket lifecycle to the router and the owned leave, which locks that exact Arc and verifies it still owns the player ID. The green tests check assignment, durable membership, routing, departure count, peer and player messages, and a valid leave from the new socket. ARM-C020 records the shipped-flow exclusion for [#685](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/685). [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) tracks other old-socket operations. |

### ARM-C022 — Old socket moderates after authority reconnect

| Field | Finding |
| --- | --- |
| State, severity | Fixed for kick and ban, high |
| Player impact | A kick or ban from a removed socket can evict a peer after its authority reconnects on a new socket. |
| Source and revision | `src/server/message_router.rs` and `src/server/moderation.rs::resolve_kick_style_target`, reviewed at `330e25f7`. |
| Invariant | A moderation request must use the physical socket lifecycle that sent it. The old kick and ban handlers awaited the moderation gate, then looked up a lifecycle by player ID; reconnect could replace that lifecycle during the wait. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(old_socket_moderation_cannot_target_peers_after_authority_reconnect)'` failed before the fix: a paused old request resumed after authority reconnect and removed the target. The test covers both kick and ban, then verifies that moderation from the restored socket still works. |
| Disposition | The router passes the source lifecycle through both handlers. The resolver locks that Arc and rejects it if it no longer owns the player ID. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for other socket-bound operations. |

### ARM-C023 — Old socket relays as a restored sender

| Field | Finding |
| --- | --- |
| State, severity | Fixed for text and binary relay, high |
| Player impact | Peers can receive game data from a removed socket stamped as a player's restored connection. |
| Source and revision | `src/server/game_data.rs`, `src/server/connection_manager.rs::next_relay_stamp_in_room`, and `src/websocket/connection.rs`, reviewed at `330e25f7`. |
| Invariant | A relay frame may receive a sequence stamp only while its physical socket lifecycle owns the sender ID and room. The old text handler could resume after the router check, and the binary path bypassed that check. Both stamped the replacement lifecycle by player ID. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(old_socket_game_data_cannot_relay_after_reconnect)'` failed before the fix: peers received both old text and binary frames with the restored sender's epoch. The green test also verifies that the restored socket's own frames receive sequence 1 and 2. |
| Disposition | Both paths carry the source lifecycle and check it at entry and under the connection entry lock when assigning a relay stamp. The WebSocket receive task also stops frames from an already replaced socket before parsing. A frame that was in progress during replacement can still charge a byte budget before the final stamp rejects it; [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) tracks remaining accounting and response effects. |

### ARM-C024 — Old socket changes restored authority's room policy

| Field | Finding |
| --- | --- |
| State, severity | Fixed for unban, authority transfer, room access, and code rotation; high |
| Player impact | A removed authority socket can lift a ban, transfer authority, seal a room, or rotate its join code after that authority reconnects on another socket. |
| Source and revision | `src/server/message_router.rs` and `src/server/moderation.rs`, reviewed at `2d104af8`. |
| Invariant | Every authority operation must hold the lifecycle of the physical socket that sent it through its room mutation, rather than look up the current socket by player ID after router dispatch. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(old_socket_cannot_change_room_authority_or_access_after_reconnect)'` failed before the fix: an old `UnbanPlayer` frame crossed the router check, the authority reconnected, and the old frame removed a seeded ban (`left: false, right: true`). The green test covers all four operations, unchanged storage and recipient queues for old frames, and successful operations from the restored socket. |
| Disposition | The router passes its source lifecycle into each operation. Each handler locks that exact lifecycle and rejects it if the player ID now maps to another socket. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for other dispatch paths and relay accounting. |

### ARM-C025 — Old socket changes restored state through direct message handlers

| Field | Finding |
| --- | --- |
| State, severity | Fixed for authority requests, readiness, game start, peer metadata, Signal, TransportStatus, and application Ping; high |
| Player impact | A frame paused after the router's connection check could resume after reconnect and use the restored player's current lifecycle, changing room state or sending peer-visible traffic. An old Ping could also charge the new socket's reply budget. |
| Source and revision | `src/server/message_router.rs`, `src/server/authority.rs`, `src/server/ready_state.rs`, `src/server/game_data.rs`, `src/server/signaling.rs`, and `src/server/heartbeat.rs`, reviewed at `da3b10f8`. |
| Invariant | Every socket-originated direct handler must lock the lifecycle of the physical socket that sent the frame and confirm it still owns the player ID before acting. |
| Confidence and reproduction | `old_socket_authority_request_cannot_release_restored_authority` failed before the fix: the old authority frame released the restored player's authority (`left: None, right: Some(...)`). Green paused-dispatch regressions cover authority, readiness, game start, metadata, Signal, TransportStatus, and Ping, plus valid frames from each replacement socket. |
| Disposition | The router carries its source lifecycle to all seven direct handlers. Each locks that exact lifecycle and checks the current mapping before mutation, fan-out, or budget charge. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for admission, transport control frames, and pre-stamp relay accounting. |

### ARM-C026 — Old spectator leave replies to a restored socket

| Field | Finding |
| --- | --- |
| State, severity | Fixed for plain and correlated spectator leave, medium |
| Player impact | An old `LeaveSpectator` request can send `NotASpectator` to the restored player's socket. Its room-operation form can also charge the restored socket's error budget during capability refusal. |
| Source and revision | `src/server/message_router.rs`, `src/server/spectator_handlers.rs`, and `src/server/spectator_service.rs`, reviewed at `38e5ccb5`. |
| Invariant | A spectator leave and its reply belong to the physical source socket. A stale request cannot use a replacement socket's lifecycle or reply budget. |
| Confidence and reproduction | `scripts/dev-loop.sh old_socket_spectator_leave_cannot_reply_to_restored_player` failed before the fix: an old leave sent a failure to the restored socket. The test covers plain and correlated forms after a paused dispatch. |
| Disposition | The handler and detach service carry the source lifecycle and check ownership before detach and reply. The router checks ownership while it reads room-operation capability and sends a capability failure. The correlated spectator test covers valid leave and rejects a mismatched source lifecycle. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for other admission, transport control, and relay accounting. |

### ARM-C027 — Old socket joins or reconnects under a restored identity

| Field | Finding |
| --- | --- |
| State, severity | Fixed for room join, spectator join, and connection-task reconnect; medium |
| Player impact | An old socket frame can resume after a replacement reconnects and send a room-join, spectator-join, or reconnect failure to that replacement. It can also enter admission using the replacement's lifecycle. |
| Source and revision | `src/server/room_service.rs`, `src/server/spectator_handlers.rs`, `src/server/spectator_service.rs`, `src/server/reconnection_service.rs`, and `src/websocket/connection.rs`, reviewed at `744a2080`. |
| Invariant | Each socket-originated admission transaction uses the physical source socket's lifecycle through its first state change and any failure reply. A replaced socket cannot adopt a current player ID. |
| Confidence and reproduction | `old_socket_join_cannot_reply_to_restored_player`, `old_socket_spectator_join_cannot_reply_to_restored_player`, and `old_socket_reconnect_cannot_reply_to_restored_player` each failed before the fix with an old failure delivered to the replacement. The tests cover plain and correlated forms. |
| Disposition | The router and WebSocket receive task pass the source lifecycle into owned join and reconnect transactions. Spectator join checks it before drain refusal and after a failed transaction. Positive room and spectator joins and correlated reconnect remain covered. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for transport control, relay accounting, and other lifecycle capture points. |

### ARM-C028 — Old control and invalid frames affect a restored socket

| Field | Finding |
| --- | --- |
| State, severity | Fixed for WebSocket receive control and refusal paths, medium |
| Player impact | An old socket can refresh a restored player's idle clock or send it an error and charge its reply budget after reconnect. |
| Source and revision | `src/websocket/connection.rs` receive loop, `src/server/heartbeat.rs`, `src/server/game_data.rs`, and `src/server/messaging.rs`, reviewed at `c0b3c8b0`. |
| Invariant | A socket-originated control frame or refusal can affect only the physical socket that sent it. The receive loop's first ownership check alone does not protect later awaited work. |
| Confidence and reproduction | `scripts/dev-loop.sh old_socket_receive_frame_cannot_affect_restored_player` failed with the old receive calls: malformed text reached the replacement, and Ping changed its heartbeat counter from 1 to 2. The green socket test covers malformed text, Ping, Pong, binary format refusal, and repeated Authenticate. A one-reply budget proves stale frames consume no replacement reply; a current refusal still succeeds. The helper regression also checks the idle clock and current activity. |
| Disposition | Ping/Pong liveness, receive-loop parse/size/encoding refusals, and game-data refusals now lock and verify the source lifecycle through their effects. Authenticate processing holds the source gate while it reads and changes connection policy. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for relay accounting before stamping and other lifecycle capture points. |

### ARM-C029 — Old relay charges a restored player's byte budget

| Field | Finding |
| --- | --- |
| State, severity | Fixed for relay budget admission, medium |
| Player impact | An old socket relay suspended at a budget wait could charge the restored player's sender and room byte windows for a frame the lifecycle-guarded stamp then rejected — a phantom charge that throttles the replacement's own game data. |
| Source and revision | `src/server/game_data.rs`, `src/coordination/mod.rs`, and `src/server.rs`, reviewed at `409482e5`. |
| Invariant | Relay budget admission, stamp allocation, and enqueue complete under the sender's source lifecycle gate; the gate releases before any backpressured fan-out completion, so a reconnect never waits on queue drains. Charged bytes and room delivery agree for every admitted frame. |
| Confidence and reproduction | `old_relay_budget_admission_serializes_restored_player_reconnect` failed with the gate disabled: the reconnect rekeyed while the old relay sat paused at each budget wait. The green table covers text and binary lanes at both budget waits, proves the rekey waits for admission, and checks the charged-byte count against the delivered frame plus the replacement's unthrottled follow-up relay. |
| Disposition | Both relay lanes hold the source gate across the budget waits and the coordinator start. A new `enqueue_relay_broadcast_after_contention` seam splits the coordinator contention fallback so its routing waits stay under the gate and only the drain awaits outside it. Budget rejections reply after the gate releases. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) remains open for the lifecycle-capture-point sweep. |

### ARM-C030 — MessagePack conversion decode had no wire-level depth contract

| Field | Record |
| --- | --- |
| State, severity | Hardened, low (defense-in-depth; no production abort reproduced) |
| Player impact | Before this change, the conversion path's recursion budget was an implementation detail of one dependency version: rmp-serde 1.3's internal 1024-level guard. A 60 KB sub-cap fixarray chain requests ~60k levels, so an upgrade or replacement dropping that internal guard would have turned a cross-format relay into a stack-overflow process abort. Payloads nested 129-1023 levels, which JSON game data could never reach at the same boundary, converted successfully; they now refuse. |
| Source and revision | `src/websocket/sending.rs::decode_binary_to_json` decoded attacker-supplied MessagePack into a recursive `serde_json::Value` with `rmp_serde::from_slice`, reviewed at `48bdc7fb`. The frame-size cap bounds bytes, not depth: one fixarray marker byte adds one nesting level. |
| Invariant | The MessagePack nesting the server decodes must sit under an explicit wire contract (128 levels, serde_json's own JSON limit), enforced before any recursive decode, not on the decoder's internal recursion counter. |
| Confidence and reproduction | `scripts/dev-loop.sh over_deep_message_pack_is_refused_before_decoder_recursion` runs the production decode on a 256 KiB stack: before the guard it aborted (`fatal runtime error: stack overflow`, SIGABRT), with the guard it returns the refusal (constant stack). On production 2 MB tokio worker stacks, guardless rmp-serde 1.3.1 stops at its internal 1024-level limit with a clean decoder error, so the abort class was latent, not live; the probe pins the guard against regressions on every platform. |
| Disposition | An iterative depth scanner (`msgpack_depth_within`, `MSGPACK_MAX_NESTING_DEPTH = 128`) walks the structure with an explicit stack before any recursive decode and refuses deeper trees as an undeliverable conversion, reusing the existing exact report and advisory accounting. The same scan guards the token-bound binary envelope (`parse_binary_message`). Malformed input stays a decoder error; the scanner only answers depth. The walk is allocation-free: its fixed-size sibling stack's capacity is the enforced limit itself, pinned by the #558 mixed-source allocation ceiling in `relay_serialization_allocations` (the first scanner draft cost one `Vec` allocation per relay and failed that ceiling on the per-PR lane). Differential review: a spec-faithful reference parser agreed with the scanner on 200k random well-formed payloads at limits {1, 2, 3, 4, 8, 128}, exhaustive 1- and 2-byte marker spaces, and all truncations; 100k mutation fuzz found no scanner-refused-but-decoder-accepted case. Pins: `depth_scanner_matches_the_limit_boundary`, `depth_scanner_counts_map_entries_as_two_slots`, `message_pack_depth_limit_is_exact`, `depth_scanner_accepts_shallow_wires_and_skips_payload_bytes`, `depth_scanner_is_conservative_on_malformed_input`. |

### ARM-C031 — The rejected app-ID warning echoed the raw client-supplied ID

| Field | Record |
| --- | --- |
| State, severity | Fixed, low (log forgery; once per rejected connection) |
| Player impact | None on the wire: the rejection, error code, and close are unchanged. Operator impact: a client could forge or distort operator-facing log lines precisely when its app ID was rejected — the `Public app ID rejected` warning printed the raw ID with a Display field in the arm where the log-safety gate had just refused it for control characters (newlines, ANSI escapes) or length. |
| Source and revision | `src/websocket/connection.rs` authentication error arm, reviewed at `dd37153b`. The gate (`app_id_is_log_safe`) rejects unsafe IDs at resolve time; the `Err` arm then logged `%app_id` unescaped. |
| Invariant | A field the log-safety gate has not vetted must never reach a log line through a Display (`%`) field; client-chosen text in anomalous-path warnings is Debug-escaped. |
| Confidence and reproduction | Direct code reading: the `Err` arm is reached by every resolve failure, each carrying the unvetted client-supplied ID — the gate-rejection `InvalidAppId` variant (which also serves unknown IDs), `RateLimitExceeded`, and the reserved app-status variants. The sibling anomalous-path warnings (`message_router.rs`, `room_service.rs` spans) already document and apply the Debug-escape rule for exactly this hazard. |
| Disposition | The field is now `?app_id` (Debug-escaped) with the rule restated at the site. Sweep: every remaining `%app_id` log site is gate-vetted (inside `Ok(info)` arms) or a typed UUID; the wire-level rejection contract stays pinned by `test_unloggable_app_id_fails_authentication_with_invalid_app_id`. |

### ARM-C032 — The undeliverable-relay warning fired per frame per recipient

| Field | Record |
| --- | --- |
| State, severity | Fixed, low (log flood; bounded harm, no delivery impact) |
| Player impact | None on delivery: reports, advisories, and drop counters are unchanged. Operator impact: one sender relaying an encoding a recipient cannot convert (e.g. rkyv into a JSON-only room) produced one `tracing::warn!` per frame per recipient with no throttle, so a single mismatched sender into a large room could flood the log sink (CPU/disk pressure) while the in-band advisory beside it was already limited to one per sender per second. |
| Source and revision | `src/websocket/sending.rs::notify_on_undeliverable`, reviewed at `dd37153b`. |
| Invariant | A per-event log on a hot path must share the rate limit of the response it describes; per-event accounting belongs to counters, not to unbounded log emission. |
| Confidence and reproduction | Direct code reading against the advisory limiter (`unsupported_notice`, one notice per sender per second with a suppressed count): every undeliverable conversion passed the unthrottled warn before any cadence check. |
| Disposition | The warning now rides the advisory cadence and carries the suppressed count; the fail-closed missing-metadata error log is unchanged apart from gaining the same `encoding`/`reason` fields the old warn carried. The per-event totals stay observable through the delivery ledgers and drop metrics pinned in the mixed-encoding and volatile-loss reviews. Per-sender cadence holds under any sender count; as with the pre-existing advisory limiter, total log volume scales with distinct offending senders, bounded by concurrent connections. |

These findings cover room-code rotation, player names, transport status, and spectator,
reconnect, room-creation drain, and terminal routing seams, plus the two log-bound
hardenings above. The rest of the C1 room and storage
rows remain unreviewed.

### C1 admission-limit review (2026-09-28)

At `46f840ab`, reviewed the in-memory room seat check and the server and
application room-count gates. `concurrent_joins_for_last_seat_admit_one_player`
parks the first seat write while a second player joins the same full-boundary
room. It checks one admission, one `ROOM_FULL` refusal, the stored roster, and
both routes. Existing barrier checks
`server_room_cap_is_atomic_across_games` and
`application_room_cap_is_atomic_across_games_and_independent_between_apps`
cover concurrent creation in different games at the server and application
limits. `join_racing_room_deletion_reports_room_not_found` now checks both
ordinary and `join_only` admission when a room vanishes before the membership
write. Both receive `ROOM_NOT_FOUND`. No violation was reproduced in these
paths. Other storage adapters, leave/disconnect races, and the remaining C1
cases are unreviewed.

### C1 leave/disconnect ordering review (2026-09-28)

At `56803cf0`, checked the socket lifecycle gate where an explicit leave
waits behind disconnect after the reconnect claim is armed, and where
disconnect waits behind a leave stalled on peer delivery. The regressions
`leave_queued_during_disconnect_cannot_consume_reconnect_or_repeat_departure`
and `disconnect_queued_during_leave_does_not_arm_a_reconnect` confirm one
absent durable row, one peer `PlayerLeft`, one departure count, and the expected
token outcome for each order. Neither interleaving reproduced a defect. Other
leave/disconnect races remain unreviewed.

### C1 reconnect and activity-reaper ordering review (2026-09-28)

At `24e1dafb`, checked the case where activity cleanup selects an expired
transient socket before reconnect moves that socket to the restored player ID.
`stale_reaper_snapshot_cannot_close_a_restored_reconnect` advances paused time,
captures the cleanup candidate, reconnects, then applies the stale candidate
through the farewell and fallback close paths. The test checks the restored
room seat, route, peer event, consumed token, and usable socket. No violation
was reproduced. The opposite order, where cleanup pins a close before the
reconnect claim, is covered by
`reconnect_on_a_reaper_pinned_socket_is_refused_and_preserves_the_token`.
Other stale-socket cleanup paths remain unreviewed.

### C1 reconnect claim expiry during restore (2026-09-28)

Reviewed the claimed reconnect while its membership write is paused across
the monotonic admission deadline. The unit regression
`reconnect_claim_survives_expiry_during_membership_restore` runs the server's
expired-record cleanup after the deadline, then releases the write. Cleanup
removes an expired unclaimed sibling but retains the claimed record and room
protection; the original seat, route, peer
event, and directed baseline complete, and the one-time token is consumed.
No violation was reproduced. The manager's existing
`expired_but_claimed_reconnection_survives_every_expiry_surface` test covers
claim release after expiry and subsequent cleanup. Other failed restore and
retry paths remain unreviewed.

### C1 lifecycle-capture-point sweep (2026-09-30)

Closes the last open row of
[#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686)
at `8191ed60`. The sweep reviewed every site that captures or validates a
`ClientLifecycle` before one or more await points and then acts, across all 17
files that mention the type: `server.rs`, `server/{authority,connection_manager,
game_data,heartbeat,maintenance,message_router,messaging,moderation,ready_state,
reconnection_service,room_service,signaling,spectator_handlers,
spectator_service}.rs`, and `websocket/{connection,handler}.rs`. The rekey
mutates the shared lifecycle only from the gate-holding reconnect transaction
(`reconnection_service.rs:678` guards `reassign_connection` at `:1313`;
`set_player_id` has no other caller), so a held gate pins the identity. A site
is safe when it re-validates `player_id()` plus `lifecycle_matches` under the
gate after its last pre-act await, or when its act is keyed to state a rekey
removes.

Every lifecycle-resolving handler follows the fenced shape: authority, ready
state, start game, transport ping, signal dispatch, transport status, join,
leave, spectator join/detach, moderation (kick/ban/unban/access/transfer/
rotation, with guards carried through `ModerationTarget`), relay text/binary
admission (the stamp adds a second `ptr_eq` fence at the final touchpoint),
authenticate and receive-loop refusals, `assign_client_to_room`,
`cleanup_client_in_missing_room`, and the reconnect transaction itself.
Documented exclusions, each verified against the code:

- Ungated keyed-liveness lanes (`maybe_update_last_seen`, the router's
  pre-dispatch activity writes, the roomless binary branch): the stale id's map
  entry is gone after a rekey and unknown ids never take a throttle stamp, so
  the write lands nowhere or only on the retired identity; the router re-checks
  the source after the await before dispatching.
- Socket-keyed acts (probe state, farewell enqueues, close pins from the
  socket's own I/O tasks): the signal belongs to the physical socket and a
  rekey cannot redirect it.
- Post-release replies (moderation terminal results, relay budget refusals,
  spectator failure replies): keyed to an id whose route the rekey removes, or
  re-locked and re-validated before sending.
- Test-only entries: `handle_reconnect` with no lifecycle is unreachable from
  production dispatch; the socket path always forwards the source lifecycle.

No violation was reproduced. One defense-in-depth residual was found and
fixed: `charge_error_reply` pinned the `4006` close by map key after the
farewell await, so a rekey landing inside that await skipped the pin, and the
one-shot `report_exhaustion` never retried it until the window rolled over.
The window was unreachable (every charge site holds the charging player's own
lifecycle gate, which the rekey needs), but a future ungated charge site would
have reintroduced it. The close now pins the per-socket close signal captured
under the charge guard, so it follows a concurrent identity swap
([#697](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/697),
fixed by PR
[#699](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/699),
regression
`error_reply_exhaustion_pins_the_close_through_a_rekey_inside_the_farewell_await`).
The same capture-under-fence pin hardened the authority kick's one-shot
`4007` close (`evict_member_by_authority`), the last same-class residual
found by the fix review.
With this sweep, [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686)
is closed: each path has a deterministic regression from PRs #687-#696 or a
documented exclusion here.

### C1 reconnect failed-restore and retry review (2026-09-30)

At `bdf8bddb`, reviewed every post-claim rejection in `handle_reconnect_owned`
(`src/server/reconnection_service.rs`): the pre-restore refusals (room full,
missing seat record, name conflict), the kicked, already-connected, banned,
and wrong-application-id rechecks inside the claimed window, the
membership-write storage fault and its room-missing reclassification, the
stale-endpoint clear failure, the drain refusal, the post-restore authority
grant, the room-readback faults, and the reassignment and
baseline-publication failures. The rejection
contract holds on every reviewed path: `reject_claimed_reconnect` rolls back
exactly the state the attempt made durable (restored membership row, granted
authority, taken-over detach retry with its application-claim provenance),
releases the claim unspent, and replies with the classified code. The
`Option<Option<_>>` detach-requeue arithmetic re-queues inherited provenance
unchanged when the rollback removal fails (promoting a bare inheritance to
an owed bare retry), and when removal succeeds retains only
ownership-carrying provenance — a bare inheritance then means nothing is
left to repair.

Evidence added:

- `reconnect_membership_restore_storage_error_releases_claim_for_retry` — a
  transient fault on the membership-restore write (new one-shot
  `fail_next_add_player_to_room_for_test` injection) replies
  `InternalError`, leaves the roster untouched, releases the claim, and the
  same token reconnects once storage recovers. Red-proofed by disabling the
  storage-error rollback.
- `reconnect_authority_grant_storage_failure_completes_degraded_and_recovers`
  — an authority-grant fault (new one-shot
  `fail_next_request_room_authority_for_test` injection) after membership
  restored completes the reconnect degraded: live member, vacant role, no
  presented authority, and the member's next reconnect re-runs the grant.
  Red-proofed by making the grant failure reject the attempt. Disposition:
  best-effort continuation is correct — the membership row is already
  durable when the grant runs, so rejecting would owe a removal to the
  storage that just failed; recovery is the client-driven reconnect any
  live was-authority member can perform.
- The room-deleted reclassification, room-full, name-conflict, drain,
  reassignment-failure, and baseline-failure rejections were already pinned
  by `reconnect_room_deleted_during_restore_is_classified_room_not_found`,
  `reconnect_room_full_failure_releases_claim_for_retry`,
  `reconnect_name_taken_by_new_member_rejects_without_spending_token`,
  `reconnect_during_shutdown_drain_is_rejected_with_server_draining`,
  `reconnect_reassign_failure_rolls_back_membership_and_releases_claim`, and
  `reconnect_baseline_delivery_failure_rolls_back_and_releases_claim_for_retry`.
- The bare-detach (`None` provenance) requeue branch is covered by
  derivation plus the ownership-provenance sibling
  `rejected_reconnect_requeues_the_ownership_rollback_it_inherited`; no
  dedicated `None`-provenance pin was added.
- The remaining gates carry recorded evidence or an explicit exclusion: the
  banned refusal is pinned end to end through `handle_reconnect`
  (`ErrorCode::Banned`, `moderation_tests.rs` ban-reconnect regression), the
  wrong-application-id refusal by
  `app_bound_room_owner_gates_seated_spectator_and_reconnect_admission`
  (wrong app → `RoomNotFound`), the claim-level kicked refusal by the
  moderation unit pin (`claim_reconnection` → `Err(Kicked)`), and the
  pre-claim already-connected refusal at the dispatch boundary
  (`reconnect_during_teardown_preserves_token_for_retry` and the H6
  duplicate-claim suite). The stale-endpoint clear failure, the pre- and
  post-restore room-readback faults, and the kicked/already-connected
  rechecks inside the claimed window are covered by derivation only: each
  exits through the same `reject_claimed_reconnect` seam whose storage-fault
  and rollback branches are red-proofed here, injects no state a later
  check trusts, and releases the claim through the same guard. No dedicated
  pins were added for them.

No violation was reproduced. Every post-claim rejection of the reconnect
restore has a recorded disposition: each gate is pinned end to end or
carries an explicit derivation exclusion in this section.

### C1 token rotation boundary review (2026-09-30)

At `562a63fb`, reviewed the rotation boundaries of the reconnect transaction
(`src/server/reconnection_service.rs`) against the concurrent-claim guards
(`src/reconnection.rs`). The invariant chain: `claim_reconnection` checks and
sets the claim under one write-lock hold with no awaits, so a second claimant
during the transaction gets `AlreadyInProgress` (pinned by
`test_reconnection_claim_is_single_use_under_concurrency`, the H6
duplicate-claim e2e, and the fuzz model's no-double-claim invariant). The
disconnect registration consumes the pre-issued join token, so no credential
exists for the identity between disconnect and rotation. Rotation mints the
fresh token as the last step of the `Reconnected` baseline builder, inside the
coordinator's registration critical section; completion removes the claimed
record only after that baseline is queued, so the token is never consumed
without a delivered baseline. The new token is unclaimable until the next
genuine disconnect arms it, and a retry presenting the consumed token after
a completed reconnect is refused — `NoRecord` while the consumed record
stays removed, and by token comparison once the next disconnect has armed
the fresh token.

Failure unwinding after rotation: a rejection once reassignment has occurred
runs `reject_after_reassigned_reconnect_failure`, which discards the freshly
rotated pre-issued token before restoring the transient identity and rolling
back, so a player who was never restored never holds an armed credential. The
panic supervisor repeats the same unwinding for the reassigned-but-not-committed
phase (`ReconnectAfterReassignment` regression) and, post-commit, completes the
claim and keeps the rotated token (`ReconnectAfterTerminal` regression pins the
consumed old token). `ReconnectionClaimGuard::drop` deliberately releases
nothing; only the supervisor's phase-aware completion or rollback may mutate
the claim.

Evidence added (extends
`reconnect_baseline_delivery_failure_rolls_back_and_releases_claim_for_retry`):

- A delivered retry must surface a rotated token on the wire (`Reconnected`
  payload token differs from the consumed one) and re-arm the pre-issued map
  for the next disconnect. Previously only the consumed old token was pinned;
  the fresh-credential side of the rotation was not.
- A failed baseline delivery leaves no pre-issued token for the unrestored
  identity.
- Boundary record: in that test's queue-full scenario the coordinator refuses
  at initial-slot reservation, before the baseline builder runs, so the
  rotation has not happened yet and the reject-path discard is a no-op there.
  The post-rotation failure windows (builder fault, commit channel-close,
  drain flip between builder and commit) share this discard line and the
  same unwinding as the pinned reassigned-failure path.
- Post-rotation drain-flip pin (issue #707, 2026-09-30): a new one-shot
  `get_room_players` pause seam parks the reconnect transaction inside the
  baseline builder, before the builder's final rotation step. Flipping the
  shutdown drain while parked makes the post-builder commit gate refuse with
  `DeliveryOutcome::Canceled` — the exact drain-flip window — so the reject
  path must discard the freshly minted token.
  `reconnect_drain_flip_after_baseline_rotation_discards_the_fresh_token`
  pins the full end state: `ReconnectionFailed(ServerDraining)` on the wire,
  no pre-issued token for the unrestored identity, restored transient
  identity, claim released for retry, and rolled-back membership. Red-proofed
  by removing the discard line: the fresh token survives armed and the test
  fails on the pre-issued-token assertion. The builder-fault and
  channel-close windows keep derivation-only status for their own arrival
  paths, but they exit through this same now-pinned discard line.

No violation was reproduced. The rotation ordering, concurrent-claim refusals,
both unwinding phases, and the post-rotation reject-path discard (through the
drain-flip window) carry pinned evidence.

### C1 allowlist and key reload boundary review (2026-10-01)

At `a6b6214e`, reviewed the SIGHUP reload seam end to end: the glue
`reload_allowed_apps_from_config` (`src/main.rs`), the allowlist swap
`AppIdAllowlist::reload` (`src/auth/middleware.rs`), the key and posture swap
`EnhancedGameServer::install_connect_token_key` (`src/server.rs`), the
handshake consumption point (`src/websocket/connection.rs` `Authenticate`),
and the configuration sources (`config::load` folds
`public_key_path` into `public_key` at load, so the reload reads the same
sources as startup and an unreadable key file is a load error, never a
keyless server).

The invariant chain: a config that fails to load or fails
`validate_config_security` applies nothing — the gate runs before both swaps,
and the two swaps are synchronous calls with no await between them, so a
partially applied reload is unreachable on the SIGHUP path. A valid SIGHUP
applies the allowlist swap and the key swap together: revoked apps stop
resolving for fresh handshakes, added apps resolve, and live connections keep
their resolved context by design (live revocation stays a restart-level
action). The install order is posture-before-key: arming briefly publishes
required-with-old-key (token-less handshakes refused — fail-closed), and the
reverse order is the only one that could open a required-but-unenforced
window. A presented token with no key installed is refused `NoKeyConfigured`,
so removing the key refuses closed, and every mixed state refuses closed.
`install_connect_token_key` re-parses the
key, but `validate_config_security` parses the same key first, so a
corrupt-key SIGHUP is rejected before either swap and cannot regress below
the gate.

Evidence added (binary `connect_token_reload_tests`, through the SIGHUP
glue; the allowlist state is observed with a public diff probe — re-applying
a set reports an empty diff exactly when it is the running set):

- `sighup_security_invalid_config_keeps_the_running_allowlist_and_key`:
  duplicate app IDs (carrying an armed posture and a fresh key), and a
  `require_connect_token=true` entry with the key block removed, each keep
  the running allowlist, the running key (old tokens verify; the rejected
  config's key does not install), and the running posture. Red-proofed by
  disabling the validation gate: with the gate gone, the duplicates config
  installs its key and arms the posture, and the test fails on key
  retention. The duplicates variant is additionally defended in depth by
  the swap's own entry validation (`AppIdAllowlist::reload` rejects it
  again); the no-key variant isolates the glue gate alone.
- `sighup_reload_applies_allowlist_and_key_swaps_together`: one valid SIGHUP
  revokes app-a, adds app-b, and rotates the key; the swapped set is live,
  fresh-key tokens verify, and old-key tokens stop verifying in the same
  pass. Red-proofed by disabling the allowlist swap: the probe fails on the
  swapped-set assertion.

No violation was reproduced. The reload boundary — key/allowlist swap order
and invalid reload — carries pinned evidence through the SIGHUP glue.

### C1 identity-slice completion review (2026-10-01)

At `81cc153a`, closed the three remaining identity and membership case
families of [#647](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/647):
spectator transitions, kick/ban races, and application isolation.

**Spectator transitions.** Every role-mutating path serializes on the
player's `ClientLifecycle` gate: a voluntary leave locks it through
`detach_expected` and validates ownership after the wait; the disconnect
teardown holds it across `unregister_client_locked`, whose spectator
`detach_if` runs inside that hold; join, moderation, and reconnect admission
carry their source lifecycle (ARM-C021/C026/C027). The owned detach
re-validates the player→room mapping under the room mutation gate (the
issue-241 TOCTOU fence), is idempotent on an absent entry, and the prune
path refuses a moved or gone session through `expected_room`. Kick and ban
refuse spectator targets by contract (`KickTargetNotFound` — the authority
documentation scopes both operations to seated players; no `Kicked`
spectator reason exists). The one unpinned interleaving — voluntary leave
racing the disconnect teardown — is now pinned in both orders
(`spectator_leave_wins_disconnect_race_with_exactly_one_voluntary_detach`,
`spectator_disconnect_wins_race_and_the_late_leave_detaches_nothing`):
exactly one detach wins, room events carry the winner's reason, the loser
is inert (no second event, `StorageError` without state change), and the
roster, local role, and retry backlog end clean. Red-proofed by disabling
both serialization fences (the `detach_expected` lifecycle fence and the
`detach_owned` room-gate re-validation): the leave-wins order publishes a
phantom second spectator event, and the disconnect-wins order lets the
losing leave report success; the tests fail on those assertions. A losing
leave whose pre-gate `is_spectating` probe predates a completed detach
replies `StorageError` to a socket the disconnect already removed — the
reply has no route and no state changes, so it is inert by derivation.

**Kick/ban races.** The ban write serializes on the room mutation gate
ahead of the eviction, and the reconnect claim re-reads the room
gate-fresh and refuses a recorded ban (issue #525 pins both orders). A kick
of a disconnected target tombstones under the room gate and fences the
second-disconnect re-arm (ARM-C017); moderation serializes through one
lifecycle gate against lock cycles (ARM-C018); old-socket kick, ban,
unban, transfer, access, and rotation are fenced at the source lifecycle
(ARM-C022/C024). A kick that races a voluntary leave waits on the target's
lifecycle gate, then re-reads storage truth under it
(`resolve_kick_style_target` revalidation): the seat is either still held
and evicted, or already gone and refused `KickTargetNotFound` — no
intermediate state is observable. Covered by derivation from the fenced
validation ordering.

**Application isolation.** The app-owner gate covers every admission
perimeter in both allowlist and open modes: fresh `JoinRoom`
(non-enumerating `RoomNotFound`), `JoinAsSpectator`, and reconnect
(`app_bound_room_owner_gates_seated_spectator_and_reconnect_admission`,
including persistence-based authorization after a cache loss and
token-preserving wrong-app refusals), plus open-policy scoping
(`open_policy_rooms_are_scoped_to_their_application`), atomic application
room-cap claims, and per-app player caps. Moderation is room-scoped by
resolution (the target must be a member of the authority's own room), so
no cross-app moderation path exists. No violation was reproduced.

With this review, every case family in the identity and membership slice
has a recorded disposition: pinned end to end, or derived from a fenced
ordering with named evidence. The slice is complete.

### C1 gameplay-transitions review (2026-10-01)

At `9e553041` (main after #712), reviewed the gameplay-transitions case
families of [#647](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/647):
ready-state invalidation on membership change, start/leave races, authority
election and loss, host/direct readiness, v2/v3 negotiation, transport
capability intersections, stale transport reports, reconnects with changed
encoding or capabilities, and the publication order of room snapshots,
session plans, and gameplay events. Every family already carries pinned or
derived evidence; the review found one unrecorded fail-closed coupling and
pinned it.

**Start authorization trusts room assignment.** The coordinator's
`handle_start_game_with_publication` authorizes "the designated authority,
otherwise any member" but never re-checks that the sender is a seated member
under the room mutation gate; the handler-level `get_client_room` lookup is
the only membership evidence ("the sender is already known to be in this
room"). The shipped dispatch paths keep that trust sound: a spectator's
connection is never assigned a room (`join_owned` requires
`get_client_room(..).is_none()` and never assigns one), so a spectator's
`StartGame` refuses `NOT_IN_ROOM` before the coordinator; a departing
player's leave serializes on the sender's own `ClientLifecycle` gate, which
the start handler holds from dispatch through the coordinator call, so no
leave can interleave between the room lookup and the gate acquisition. The
soundness is one refactor away from breaking: giving spectator connections a
room assignment (for example to deliver snapshots or broadcasts — the gap
`spectator-mode.md` records as a known limitation) would let a spectator
finalize any authority-less room whose players are all ready. The coupling
is now pinned (`spectator_start_game_cannot_finalize_the_lobby`):
dispatching `StartGame` from a joined spectator through the real router
against a fully ready authority-less lobby requires the `NOT_IN_ROOM`
refusal with the lobby still open and the ready set intact. Red-proofed by
performing that exact refactor in a probe (assigning the spectator's
connection to the room): the start then finalizes the lobby and the test
fails on the refusal assertion. `spectator-mode.md`'s "What Spectators
Cannot Do" now lists `StartGame` explicitly.

**Case-family dispositions** (reviewed at `9e553041`; no new violations):

- **Ready-state invalidation on membership change.** Joins deliberately
  break a cached `all_ready` without a corrective broadcast; the
  authoritative `StartGame` gate recomputes readiness over routed membership
  under the room gate (#447 F1 decision b,
  `join_breaks_cached_all_ready_without_a_corrective_broadcast`). Rejoining
  does not restore stale readiness; a reconnect into a finalized room
  restores that member's recorded readiness; room snapshots read the
  coordinator ready set through `snapshot_ready_players` (finalized rooms
  report their final readiness); departures prune the departing id directly
  and ready toggles prune non-members.
- **Start/leave races.** Finalization commits through
  `commit_room_messages_if_members` plus the `finalize_room_game` CAS on
  (members, authority, lobby state): a member leaving before the commit
  yields `RoutingChanged` (bounded retries, then the terminal-boundary
  fallback), and the storage-level ghost-row case is the documented
  `SnapshotChanged` deferral (session-193, #396). The exact-membership
  snapshot gate is pinned
  (`handle_player_ready_finalizes_with_member_snapshot_matching_game_starting_peers`,
  `start_game_builder_gates_each_plan_by_that_exact_members_version`), and
  old-socket lobby frames are lifecycle-refused
  (`old_socket_lobby_frames_cannot_toggle_ready_or_start_game`).
- **Authority election and loss.** `request_room_authority` is
  membership-gated (`NotAMember`) and refuses a held designation
  (`AlreadyHeld`); a departure that actually removed the authority clears
  the designation and every `is_authority` flag (no auto-reassign, per
  protocol); reconnect authority restore is live and replay-visible, never
  overrides a successor, degrades explicitly on storage failure, and rolls
  back without leaving an unrouted authority (pins in `signaling_tests`).
- **Host/direct readiness.** Host+Direct plans carry empty ICE, and a host
  without a validated endpoint falls back to the relay floor
  (`emit_host_direct_room_carries_empty_ice_even_with_turn_enabled`,
  `emit_host_direct_room_without_endpoint_falls_back_to_relay`).
- **V2/v3 negotiation and capability intersections.** The selection ladder
  is table-pinned rung by rung including every downgrade
  (`selection_table_resolves_each_rung_and_downgrade`); the finalized
  seat-fill gate demands v3 plus both sticky axes
  (`seat_fill_predicate_tracks_version_and_both_sticky_axes`); signal
  transport gates are v3+WebRTC on both ends
  (`signal_sender_must_be_v3_even_if_webrtc_transport_is_present`,
  `signal_to_v2_peer_reports_target_not_found`,
  `signal_to_v3_relay_only_peer_reports_target_not_found`).
- **Stale transport reports.** ARM-C012's post-leave/rejoin status fix is
  pinned by the transport-status cohort (dedup without re-fan-out, flap
  fan-out per transition, budget bounding, slow-peer sharing, non-v3
  ignored).
- **Reconnect with changed encoding or capabilities.** A downgraded
  incumbent keeps its seat but can never be named host: `host_invalid`
  flags the seated-but-incapable host (unit-pinned including the
  Direct-endpoint loss), the downgrade-reconnect re-election re-emits fresh
  plans end to end over real sockets
  (`host_downgrade_reconnect_reelects_and_empties_downgraded_plan`), a
  departure heals a wedged entry
  (`non_host_departure_heals_present_but_unpairable_host`), and an aborted
  re-plan retains the entry for the next event
  (`aborted_replan_transaction_retains_the_wedged_entry_for_the_next_event`).
- **Publication order.** The `start_game_publication_builder` mechanism
  queues `GameStarting` as phase zero and each per-recipient `SessionPlan`
  as phase one in one exact-membership transaction; finalized joins and
  reconnects queue the actor's plan ahead of incumbents' in two ordered
  phases (`actor_close_after_commit_does_not_suppress_incumbent_plan_phase`);
  signal dispatch holds the room mutation gate so no `Signal` can overtake a
  recipient's plan (`signal_dispatch_waits_for_room_plan_publication_gate`,
  `signal_waiting_on_plan_gate_cannot_cross_target_incarnations`).

With this review, the gameplay-transitions case families have recorded
dispositions. The coverage row moves to partially reviewed: the shared
`src/server.rs` state seams remain for later slices.

### C1 room-event duplicate-delivery disposition, #713 (2026-10-01)

[Issue #713](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/713)
reported that, while iterating on the spectator-start pin, two awaited
`handle_player_ready` toggles delivered each `LobbyStateChanged` twice per
member when per-frame `recv` awaits were interleaved between the operations,
and asked whether the room-event lane's lease/stall recovery re-runs a
completed publication when tokio's auto-advanced paused time trips a lease
deadline. Investigated at `97506b11` (main after #714). Disposition: **the
suspected mechanism does not exist and the doubling was not reproducible in
any arrangement; no defect is confirmed.**

- **The lane has no lease deadline and no re-run path.** The room-event
  mutation lease is a plain `tokio::sync::Mutex` guard
  (`RoomEventMutationLease`, `src/coordination/mod.rs`) with no TTL and no
  recovery; the ready-toggle publication takes no distributed lock. The
  slow-consumer delivery deadline arms only under per-recipient backpressure
  and, on expiry, disconnects that recipient — it never re-runs the
  publication (`reserve_one_if`, `src/server.rs`). `RoomEventLane::drain`
  executes each enqueued job exactly once: the queue pop removes the job, the
  mutex-guarded `running` flag keeps at most one live drain per lane, and the
  completion resolves only after the job's sends finish. A completed
  publication cannot re-run.
- **The reported arrangement cannot advance the paused clock.** After an
  awaited toggle, every broadcast frame is already queued, so interleaved
  `recv` expects never park and tokio's auto-advance never fires. No
  timer-driven mechanism can intervene between the operations at all.
- **Non-reproducibility sweep.** Five mechanically distinct arrangements
  delivered exactly one `LobbyStateChanged` per toggle per member: awaited
  toggles with interleaved expects (paused), the same with a spectator join
  and a router-dispatched `StartGame` refusal interleaved (paused), spawned
  non-awaited toggles with interleaved expects (paused), a fully idle 30 s
  auto-advance window between awaited toggles (paused), and real time without
  `start_paused`. All clean. The original scratch test was not retained, and
  a single-run observation remains unexplained; if the pattern reappears,
  capture the exact test.
- **Pinned invariant.**
  `interleaved_awaits_deliver_each_lobby_broadcast_exactly_once`
  (`src/server/ready_state_tests.rs`) encodes the report's detection recipe:
  interleaved expects consume one copy per toggle, a fully idle window lets
  the paused clock auto-advance past any suspected deadline, and a final
  drain must find no second copy. Red-proofed by enqueueing the toggle
  publication twice in a probe: the pin fails on the duplicated frame. The
  probe was reverted byte-identically.
- **Test-authoring hazard recorded.** During the sweep, a parked idle window
  after a silently failed setup (a 5-character room code refused by the
  fixed `room_code_length` validation) surfaces a confusing
  `Error(NOT_IN_ROOM)` at the next expect. Tests that drain instead of
  expecting can hide such setup refusals; expect the first frame of a phase
  when its arrival is the phase's evidence.

### C1 cross-room stall fairness review (2026-10-01)

At `72d9188f` (main after #715), reviewed the delivery slice's
"slow-recipient isolation; healthy-room progress during another room's
stall" case families. The within-room half was already pinned
(`slow_consumer_no_cascade_e2e.rs`, `relay_backpressure_e2e.rs`: one stalled
recipient is evicted loudly while its own room keeps flowing); the cross-room
half — one room's stall must not strand an unrelated room — had no
executable pin. The coupling audit found no shared state a stalled recipient
can hold across its delivery wait, and the invariant is now pinned.

**Coupling audit** (each shared seam a stall could hold, with dispositions):

- **Room event lanes.** `RoomEventLane::drain` runs one job at a time per
  room; a lane job parked on a slow recipient delays only that room's later
  mutations. Other rooms get their own lanes; the sequencer registry stores
  weak handles only.
- **Backpressured data deliveries.** `BackpressuredDelivery` owns
  per-recipient state only. The routed fan-out paths
  (`start_routed_deliveries`, `start_routed_deliveries_with_shared`) are
  synchronous under the routing snapshot and drop the shared `room_players`
  and `local_clients` read guards before any capacity wait is awaited; the
  parked wait holds no shared lock across its slow-consumer deadline.
- **Concurrent waits and eviction.** `finish_deliveries` awaits all
  backpressured recipients concurrently (`join_all`), bounding one
  broadcast's latency to its slowest single recipient; the subsequent
  slow-consumer removal takes the evicted player's routing write gate and
  their rooms' gates briefly, with no await inside the critical section
  beyond lock acquisition.
- **Control-plane conditional deliveries.** `reserve_one_if`'s capacity wait
  likewise owns only per-recipient state; the caller drops the shared guards
  before awaiting the reservation.
- **No shared egress budget.** The delivery path contains no semaphore or
  per-app budget a stalled recipient could exhaust: the only semaphore-class
  primitives in `src/` are `#[cfg(test)]` scaffolding, and the distributed
  room-cap lock (room creation) is mutex-based, not a delivery-path permit.
- **Sender tasks.** A sender's connection task awaits its own room's fan-out
  (within-room ordering by design, bounded by the slow-consumer deadline);
  other rooms' connection tasks are independent tokio tasks.

**Pinned invariant.**
`stalled_room_does_not_strand_a_healthy_room`
(`tests/cross_room_stall_fairness_e2e.rs`) runs two independent rooms on one
server: room A holds a flooding sender and a never-reading recipient
(connected over a clamped 4 KiB receive buffer, so the wedge is deterministic
on every host), so its fan-out parks in backpressure until the slow-consumer
deadline evicts exactly that recipient; room B runs a continuous relay flood,
a member that joins while the stall-room flood is in flight, and two draining
recipients. When room A's sender observes the stalled peer's `PlayerLeft`,
room B must already have relayed frames through the whole grace window, the
join's `PlayerJoined` broadcast must have reached a room-B recipient, and no
fair-room member may ever observe a `PlayerLeft`, an `Error`, or a socket
close; the fair-room relay's longest inter-frame gap must stay under half the
stall window (a bare frame count is not enough — early frames before the
wedge would satisfy it); and exactly one slow-consumer eviction with
abandoned-frame drops must be counted. Red-proofed by holding a shared gate
across the game-data dispatch in a probe: room B's relay went silent for the
full stall window and the pin failed on the gap oracle; the probe was
reverted byte-identically.

**Pin-authoring notes.** Wedging a recipient deterministically requires (a)
frames under the 64 KiB inbound `max_message_size` — larger frames are
refused before fan-out and wedge nothing; (b) the heartbeat Pong deadline
above the delivery deadline — otherwise the heartbeat reaper evicts the
stalled peer first and `websocket_slow_consumer_disconnects` stays zero; and
(c) a clamped receive buffer on the stalled peer's socket (the repo-standard
`connect_with_small_recv_buffer` helper) — unclamped loopback autotuning can
absorb tens of MiB, the writer never parks, and no `Full` is ever reported.

With this review, the delivery slice's cross-room fairness family has a
recorded disposition. The coverage row for relay routing moves to partially
reviewed: mixed conversion refusal remains.

### C1 reconnect epoch and sequence review (2026-10-01)

At `a096bf33` (main after #716), reviewed the delivery slice's "reconnect
epoch and sequence changes" case families. Disposition per family; no
violation was reproduced.

- **Resumed epoch without a provisional value.** The reconnect path folds
  `last_epoch + 1` (the pre-disconnect epoch survives in the reconnection
  record) into the reassignment itself, so the first metadata read already
  observes the final incarnation and no transient-socket epoch is ever
  visible. Pinned (`reassign_connection_applies_the_resumed_epoch_immediately`,
  `src/server/connection_manager.rs`; e2e
  `reconnecting_sender_bumps_epoch_and_stamps_it_on_game_data`).
- **Recipient-visible stream ordering.** A recipient that never left sees the
  sender's `(epoch, seq)` stream strictly increase across reconnect (epoch
  bump, seq restart at 1), rejoin (self-describing epoch jump), and room
  switch (epoch carries forward, seq restarts). Pinned
  (`sender_leave_and_rejoin_restarts_seq_at_one`,
  `epoch_carries_across_a_room_switch_not_reset_to_one`); the recipient's
  epoch attribution rides `PlayerReconnected.epoch` (golden
  `golden_player_reconnected_with_epoch`) and the room snapshots (e2e
  `v3_room_snapshots_carry_epoch_pre_v3_omit_it`).
- **Stale-sender dispatch racing the rekey.** The reconnect rekey waits on
  the sender's lifecycle gate, which is held across the budget charge, the
  stamp, and the enqueue (issue-#686 fence,
  `src/server/game_data.rs`); a stale socket's frame is dismissed with no
  charge. Dispositioned by the completed lifecycle-capture-point sweep
  (ARM-C030); the gate is released before backpressured fan-out completion,
  so the rekey never waits on a queue drain.
- **Epoch saturation.** Both bump paths (`prepare_client_to_room`, the
  reconnect resume) use `saturating_add`, can never regress an epoch, and log
  loudly at `u32::MAX` (`src/server/connection_manager.rs`,
  `src/server/reconnection_service.rs`). The terminal-incarnation reuse is
  unreachable in practice (~2^32 incarnations of one sender) and is reviewed
  by inspection; no pin.
- **Failed-restore rollback.** `restore_reassigned_connection` resets the
  roomless transient identity's stamp state to 0, dominated by the
  reconnect-path epoch resume on the retry. Dispositioned by the
  failed-restore and retry review above (2026-09-30).
- **Cross-epoch exact gap accounting.** Each incarnation restarts `seq` at 1,
  so a sender's loss ranges from different epochs overlap numerically; a
  merge that ignored `epoch` would swallow the newer incarnation's loss
  record into the older epoch's range and the client would never learn of
  the newer loss. The single merge rule
  (`gaps_merge`, `src/coordination/outbound_queue.rs`) requires epoch
  equality and is shared by the queue's gap reports and the writer's
  pending unsupported-format report, so one clause guards both. Now pinned
  (`cross_epoch_gaps_of_one_sender_stay_distinct_ranges`): epoch-4 and
  epoch-5 supersession losses of one sender must surface as two distinct
  ranges, each carrying its own epoch. Red-proofed by removing the
  epoch-equality clause in a probe: the ranges collapsed into one
  epoch-4 range and the pin failed; the probe was reverted byte-identically.
- **Old-epoch frames queued across the reconnect.** Fan-out appends in stamp
  order and the data lanes drain FIFO, so a recipient's undelivered
  old-epoch frames precede every new-epoch frame and no spurious gap is
  reported for them; per-epoch loss attribution is the pinned merge rule
  above, and cross-epoch supersession attribution is pinned
  (`latest_key_spans_sender_epochs_and_reports_the_replaced_epoch`).

With this review, the delivery slice's reconnect epoch/sequence family has a
recorded disposition. The coverage rows for reconnection and coordination
gain the new evidence; their remaining items are unchanged.

### C1 latest coalescing keys and generations review (2026-10-01)

At `b851bf50` (main after #717), reviewed the delivery slice's "latest
coalescing keys and generations" case families. Disposition per family; no
violation was reproduced. One explicit-pin gap was closed.

- **Key composition `(from_player, room_id, key)`.** An equal application
  key from a different sender, routed through a different room, or carrying
  a different key value is an independent stream. Owner and room isolation
  are pinned (`latest_supersede_requires_matching_stream_owner_and_room`);
  the key value now has its own pin
  (`latest_supersede_requires_the_matching_key_value`): two keys of one
  sender in one room both deliver in enqueue order with no supersession
  counter movement and no gap report. Red-proofed by probing `latest_key()`
  to a constant key: the second value superseded the first (`losses: 1`)
  and the pin failed on the enqueue outcome; the probe was reverted
  byte-identically. Cross-epoch, the key deliberately excludes `epoch` so
  one logical stream continues across a sender's reconnect; the
  supersession report carries the replaced (older) incarnation's epoch
  (`latest_key_spans_sender_epochs_and_reports_the_replaced_epoch`).
- **Class/key contract at dispatch.** A v3 `latest` frame without `key`, a
  `reliable`/`volatile` frame with one, and any class/key on a pre-v3
  sender are refused `InvalidDeliveryClass` before the payload cap and the
  relay-budget charge, before fan-out, and without consuming a relay
  sequence (pinned e2e `invalid_delivery_class_does_not_consume_a_relay_sequence`
  over all four illegal pairings;
  `pre_v3_sender_with_delivery_metadata_rejects_invalid_delivery_class`;
  v2 wire tests). The queue's `InvalidMetadata` fail-closed guard is
  defense in depth behind the dispatch gate
  (`queue_rejects_control_data_and_mismatched_delivery_metadata`).
- **Generation shielding.** After a room transition, the coalescing scan
  and the volatile-eviction scan match only current-generation rows: a
  stale-generation same-key row is neither superseded nor evicted, stale
  rows drain in fence order before the transition barrier, and a
  dropped-full report lands after the barrier
  (`room_transition_shields_stale_generation_rows_from_latest_scans`).
  A stale-scope arrival is canceled by `scope_matches` before any scan.
- **Supersession mechanics.** Each supersession removes the exact
  predecessor, emits its causal `LatestSuperseded` gap carrying the
  predecessor's `(from_player, epoch, seq)`, and enqueues the successor
  with the key's original pendency time, so a continuously superseded key
  neither extends its coalesce deadline nor loses its causal report
  (`latest_supersession_appends_successor_and_reports_exact_predecessor`;
  e2e `latest_coalescing_reports_exact_gap_before_successor`; the
  anti-starvation bound is pinned by
  `continuously_superseded_latest_key_still_reaches_the_socket`).
- **Saturation paths.** A latest arrival at a full data lane evicts the
  oldest current-generation volatile with a causal `VolatileDropped`
  report, never a reliable row; with no eligible victim the arrival drops
  with `LatestDroppedFull` (`full_latest_evicts_oldest_volatile_before_dropping_arrival`,
  `volatile_replaces_oldest_volatile_but_never_reliable`,
  `sustained_latest_overload_coalesces_reports_without_fail_close`); an
  unreportable loss fails closed
  (`lossy_change_fails_closed_when_report_lane_is_full`).
- **Coalescing window.** Only a `Latest` front arms the batch window and it
  releases on window elapse, batch threshold, or queue progress; an
  interleaved control pop never consumes the armed budget; the pre-v3
  legacy lane never coalesces and fails closed on a `Some(Latest)` row
  (`regression_198_latest_behind_reliable_still_coalesces`,
  `interleaved_control_pop_preserves_armed_latest_batch_budget`,
  `pre_v3_data_never_arms_latest_coalescing_deadline`,
  `latest_row_on_legacy_lane_fails_closed_as_accountability_breach`).
- **Counter conservation.** Per-class counters conserve every terminal
  outcome, including latest supersession
  (`per_class_metrics_conserve_every_terminal_outcome`).

With this review, the delivery slice's latest coalescing keys/generations
family has a recorded disposition. The coordination and queues coverage row
gains the new evidence and moves to partially reviewed: its delivery-side
queue families are dispositioned, while the reservation/commit
cancellation and panic seam of `RoomMessageTransaction` still needs its own
review.

### C1 mixed encoding and unsupported conversion review (2026-10-02)

At `3b83e00e` (main after #720), reviewed the delivery slice's "mixed
encodings and unsupported conversions" case families. Disposition per
family; no violation was reproduced. One explicit-pin gap was closed: the
pre-v3 recipient side of an opaque refusal had no end-to-end evidence.

- **Negotiation boundary.** A requested `game_data_format` outside the
  deployment's supported set is refused with a budget-charged
  `UnsupportedGameDataFormat` error and downgrades to JSON, the universal
  text floor (`test_rkyv_game_data_format_request_falls_back_to_json_and_is_not_advertised`,
  `test_opt_in_rkyv_and_protobuf_negotiate_and_advertise`). Format
  negotiation is protocol-version-agnostic.
- **Same-format and lossless cohorts.** A JSON sender reaches every
  recipient as text; a MessagePack payload reaches same-format recipients
  as a direct binary cohort and JSON recipients through a lossless
  MessagePack-to-JSON decode, with no error amplification
  (`mixed_json_and_message_pack_relay_without_error_amplification`).
- **Opaque refusal.** rkyv and protobuf payloads carry no schema the
  server can convert, so every cross-format recipient is skipped instead
  of receiving a lossy guess (`decode_binary_to_json` refuses both). A
  same-format peer still receives byte-identical strict-v3 envelopes;
  cross-format v3 peers receive an exact `UnsupportedFormat` gap report
  before any later frame plus the per-sender rate-limited advisory
  (`opaque_opt_in_encodings_relay_directly_and_report_cross_format`).
  The refusal is pair-complete: every directed pair from an opaque source
  is unsupported, and the preflight unit matrix pins the same decision per
  pair (`unsupported_binary_fallback_preflight_is_exact`).
- **Pre-v3 recipients (new pin).** A v2 queue never accumulates a pending
  unsupported report (`record_unsupported_format` refuses non-v3 queues)
  and the report writer keeps its own v3 gate, so a `DeliveryReport` can
  never leak onto a v2 wire. The omission is still counted
  (`websocket_messages_dropped`, per-connection `dropped_for_you`) and the
  recipient still receives the rate-limited advisory, now pinned by
  `v2_recipients_of_opaque_payloads_get_advisories_without_v3_reports`:
  a v2 JSON and a v2 MessagePack recipient of one opaque rkyv frame each
  observe no payload frame, one advisory, no report, and a control plane
  that keeps flowing through the sender's leave. Red-proofed by probing
  the opaque refusal into a lossy JSON fabrication: both v2 recipients
  received the fabricated `GameData` frame and the pin failed; the probe
  was reverted byte-identically.
- **Oversized fallback.** A cross-format payload whose decoded JSON would
  exceed the outbound cap is refused by the preflight before cache
  allocation and accounted as an omission
  (`binary_fallback_decode_budget_rejects_compact_tree_before_cache_allocation`).
- **Amplification under load.** The throttled unsupported-format storm
  scenario (a weaker recipient must not be evicted; advisories only) is
  pinned by the nightly-only
  `unsupported_message_pack_fallback_does_not_flap_weaker_recipient` and
  the report/advisory ordering conformance tests
  (`conformance_gap_counters_are_causal_with_rate_limited_unsupported_advisories`,
  `conformance_unsupported_advisory_requires_prior_report_but_not_adjacency`);
  the storm cohort stays out of the default CI run by design.

With this review, the delivery slice's mixed encodings and unsupported
conversions family has a recorded disposition. The relay-routing coverage
row is fully reviewed.

### C1 permitted volatile loss and exact gap/report accounting review (2026-10-02)

At `b4d30b1a` (main after #723), reviewed the delivery slice's "permitted
volatile loss" and "exact gap/report accounting" case families. Disposition
per family; no violation was reproduced. Two composed-path pin gaps were
closed with one real-socket pin.

- **Queue-level lossy drops.** Supersession, full-lane volatile eviction,
  and dropped-full arrivals each drop at most one row per lossy enqueue and
  emit a causal exact gap (`try_enqueue_latest`, `try_enqueue_volatile`):
  the matrix is pinned at queue level
  (`latest_supersession_appends_successor_and_reports_exact_predecessor`,
  `full_latest_evicts_oldest_volatile_before_dropping_arrival`,
  `volatile_replaces_oldest_volatile_but_never_reliable`,
  `room_transition_shields_stale_generation_rows_from_latest_scans`,
  `sustained_latest_overload_coalesces_reports_without_fail_close`), and the
  supersession half is pinned end to end
  (`latest_coalescing_reports_exact_gap_before_successor`). An unreportable
  loss never drops silently: `fail_accountability` fails the whole queue
  closed (`lossy_change_fails_closed_when_report_lane_is_full`), so loss can
  never outrun its report capacity.
- **Coordinator fan-out drops.** Accounted drops land in both the server-wide
  counter and the per-connection ledger (`record_queue_outcome`);
  accountability-unavailable closes the connection loudly; a cancelled park
  resolves attempted+abandoned without closing (the recipient is not at
  fault, and latest/volatile loss is part of the delivery model).
- **Teardown and writer abandonment.** Abandonment counts per class from the
  queue and batcher, and the abandoned-in-flight fence abandons the remainder
  rather than write a hole no report covers
  (`close_flush_never_writes_the_queue_behind_an_abandoned_write`): the
  observable stream stays a gap-free prefix plus exact reports.
- **Generation fences.** A stale-scope lossy arrival is canceled
  (CANCELED, `websocket_deliveries_canceled`), not dropped-and-reported: the
  epoch bump plus transition barrier make the boundary self-describing, and
  stale rows drain before the barrier.
- **Report exactness mechanics.** Lossy enqueues reserve causal report
  capacity before mutating the data lane (`can_record_gap`); the single merge
  rule requires epoch equality (`cross_epoch_gaps_of_one_sender_stay_distinct_ranges`);
  frontier stamps clamp to what was written and commit only after flush;
  exact ranges are never throttled while the advisory prose is limited to 1/s
  per sender; the 256-range bound rolls over; the v3-only gates hold at
  record and write time. Each is pinned (queue and writer unit tests, the
  conformance auditor's counter-delta/range-validity oracles, and the golden
  wire tests).
- **Persistence across reconnect.** Reports do not persist across reconnect
  by design: a resumed recipient is baselined by authoritative
  `SenderWatermark`s, and the cumulative per-connection ledger survives a
  rekey. Permitted silent drops are limited to advisory `RelayStats` frames
  on a full control queue (cumulative counters; the teardown is the signal)
  and the farewell advisory skip on a connection that is closing anyway.
- **New pin (closes both composed-path gaps).** No PR-lane test had observed
  a real volatile eviction over a socket, and no test had observed the
  per-connection `dropped_for_you` ledger move off zero.
  `flooded_nonreading_recipient_observes_exact_volatile_gaps_and_dropped_for_you`
  (`tests/v3_game_data_sequencing_e2e.rs`) floods 2,000×16 KiB volatile
  frames at a silent recipient whose data lane is two slots: the sender is
  never backpressured and the recipient is never closed; the observed seqs
  plus the exact disjoint `VolatileDropped` ranges must cover the whole
  offered stream; the cumulative `volatile.dropped` counters (wire report
  and class ledger) must equal the missing count; `dropped_for_you` must be
  non-zero in both the `RelayStats` frame and the server-side connection
  ledger; and a marker frame must arrive with the stream's next seq.
  Red-proofed by suppressing the causal gap report at the volatile-eviction
  site: coverage never closed (`delivered=19, gaps=[]` with 1,981 counted
  drops) and the pin failed; the probe was reverted byte-identically. The
  class-ledger conservation read settles before asserting equality: the
  writer records a delivered row after its socket write resolves, so the
  ledger can lag the recipient's observation by a scheduling beat (first
  CI run caught exactly that sampling race, 18 counted vs 19 observed).

With this review, the delivery slice's permitted-volatile-loss and
gap/report-accounting families have recorded dispositions. Reviewed by
inspection with no separate pin: the advisory limiter's oldest-sender
eviction at its 256-sender cap (the bound itself is pinned by
`unsupported_notice_limiter_bounds_sender_state`), and the metadata-less
`record_unsupported_class` branch that sits behind the fail-closed arm.

### C1 transaction reservation/commit cancellation and panic review (2026-10-02)

At `9e07ecd7` (main after #724), reviewed the recovery slice's opening seam:
cancellation and panic at `RoomMessageTransaction` reservation and commit
(`commit_room_messages_if_members_with_hook` and its sibling
`broadcast_to_room_if_with_hook`). One defect class was reproduced and fixed;
every case family now carries a disposition.

- **Structured cancellation at the transaction's awaits.** Every pre-hook
  terminal path (batch canceled, slow consumer, channel closed, routing
  change, hook rejection, hook error) already recorded each reserved frame
  and retried or returned explicitly; the lane's spawned jobs cannot be
  dropped by a caller (detached-job pin at the lane level), so the only
  caller-visible cancellation is process teardown (drain row).
- **Red-first defect: panic released reservations without accounting.** A
  panic inside the commit hook, the phase callback, or the broadcast replay
  hook unwound through its publication path and dropped every held permit
  silently: no `websocket_deliveries_canceled` movement, while the same
  terminal paths reached by `Err`/`false` each count exactly once. Red
  evidence: `panicking_commit_hook_releases_and_accounts_every_reservation`
  failed with `left: 0, right: 4` and
  `panicking_phase_callback_accounts_remaining_frames_and_never_delivers_phase_one`
  failed with `left: 0, right: 1` on the unfixed code; the broadcast
  replay-hook pin was red-probed against the fixed code by disabling
  `ConditionalReservationGuard`'s accounting flag (`left: 0, right: 2`);
  the probe was reverted byte-identically.
- **Fix (class sweep).** Reservation ownership on both publication paths now
  flows through drop-accounting guards (`RoomBatchReservationGuard` and a
  sibling `ConditionalReservationGuard` of the same design) that count each
  still-undelivered reserved frame exactly once on any exit that is not an
  explicit, accounted terminal path. The two explicit helper functions and
  all twelve manual call sites were removed; the guards cover the window
  from guard arm (immediately after the batch reservation resolves) through
  commit, so a missed accounting exit is unreachable there, including for
  future early returns. The commit loop consumes permits through the guard,
  so its own per-frame accounting never double-counts.
- **New pins (one per fixed seam).** The three pins (two red-first, one
  red-probed) cover each fixed seam. `panicking_commit_hook_releases_and_accounts_every_reservation`:
  a panicking commit hook delivers no frame, counts all four reserved frames
  exactly once, releases recipient capacity (a follow-up transaction's worth
  of `try_send` succeeds), and surfaces as the job's panic.
  `panicking_phase_callback_accounts_remaining_frames_and_never_delivers_phase_one`:
  a panicking phase callback delivers phase zero exactly once, never
  delivers phase one, counts the unpublished phase-one frame, and releases
  its capacity.
  `panicking_broadcast_replay_hook_releases_and_accounts_every_reservation`:
  a panicking broadcast replay hook delivers no frame, counts both reserved
  frames, and releases their capacity. All three bind the observed panic to
  its sentinel message, not just to `is_panic`.
- **Panic recovery shape (by inspection, no new pin).** Each panic surfaces
  as the room-event job's `JoinError`, which the lane isolates (pinned,
  `panicking_room_event_isolated_from_the_next_job`) and converts to a
  caller error, so each caller's existing publication-failure arm (for
  example the join fallback) runs unchanged. A panic after the hook's
  durable mutation commits leaves phase zero or nothing published; the
  reconnect baseline is the client-driven recovery path, matching the
  recorded degraded-restore disposition. The broadcast pin drives
  `broadcast_to_room_if_members_with_hook`, whose wrapper differs from the
  production replay-hook callers only in membership filtering before the
  same guarded core.
- **Sibling sweep.** `broadcast_to_room_if_with_hook` had the same silent
  window inside its replay hook; its reservations now flow through a sibling
  guard of the same design, and its drain/continue/rejection paths keep
  their exact prior accounting semantics. `reserve_one_if`'s parked waits
  were already fenced (`ParkedWaitAccounting`, #417).

With this review, the coordination and queues coverage row is fully
reviewed: its remaining reservation/commit cancellation and panic families
carry pinned evidence and the row moves to reviewed. The recovery slice
continues with deadlines, wall-clock versus monotonic expiry, drain/shutdown
with queued data, and process-loss behavior (admin/shutdown coverage row).

### C1 maintenance and deadlines expiry boundary review (2026-10-02)

At `98d86306` (main after #725), reviewed the recovery slice's
deadlines-at-boundary and wall-clock-versus-monotonic families across
`src/server/maintenance.rs`, `heartbeat.rs`, `dashboard_cache.rs`, and
`src/deadline.rs`. No defect was found; the one unpinned player-visible
boundary now carries a pin, and every family carries a disposition.

- **The activity-reaper pair flips once at the boundary (new pin).**
  `collect_expired_clients` and `request_activity_timeout_if_expired` both
  read monotonic `tokio::time::Instant` and agree at exactly `ping_timeout`:
  the snapshot does not collect the client and the atomic revalidation
  refuses to pin the close, while one tick later both expire and the
  revalidation pins the `ActivityTimeout` close. The pairing is what lets a
  Pong arriving between the snapshot and the revalidation rescue the
  connection (the entry-guard exclusion documented on the revalidation).
  New pin `activity_reaper_expiry_flips_once_at_the_ping_timeout_boundary`
  (paused clock), red-proofed by flipping the snapshot comparison to `>=`
  and the revalidation to `<`: the pin failed at the exactly-at survival
  assertion; the probe was reverted byte-identically.
- **Zero timeout disables the reaper end to end (pinned, existing).** The
  cleanup task's `ping_timeout.is_zero()` guard short-circuits the snapshot,
  pinning the documented "`0` disables the activity reaper" contract
  (`zero_ping_timeout_disables_activity_reaper`). The revalidation predicate
  has no internal zero guard by design; its only production callers sit
  inside that guarded loop.
- **Monotonic decision paths (pinned, existing).** Reaper, throttle,
  reconnect window, room GC, dashboard staleness, and the cleanup-claim
  window all decide on the runtime clock, and `tests/clock_source_scan.rs`
  walls production time reads behind injectable or tokio seams. Boundary
  pins: `should_update_last_seen_throttles_until_threshold_elapses`
  (inclusive by design for the last-seen database-write throttle),
  `reconnect_eligibility_flips_once_at_the_monotonic_deadline` (strict),
  `wall_clock_step_cannot_reap_monotonic_fresh_occupied_room`,
  `monotonic_idle_rooms_are_reaped_despite_fresh_wall_stamps`,
  `every_activity_path_refreshes_monotonic_liveness`,
  `cleanup_claim_and_prune_boundaries_run_on_monotonic_time`, and
  `staleness_gate_decides_on_monotonic_elapsed_time` (strict).
- **Wall-clock changes cannot open or close a monotonic deadline.**
  Reconnect records capture both clocks at the same moment; a wall-clock
  jump rewrites every stored UTC field but not the decision deadline
  (`wall_clock_jumps_cannot_open_or_close_the_reconnect_window`). Room rows
  pair the wall `last_activity` stamp with the monotonic liveness stamp, and
  the wall fallback exists only so a foreign row insertion cannot become an
  immortal room. The drain's advertised wall-clock deadline is clamped by
  one grace period for later observers (`wait_before_close_since`), so an
  overflowed sentinel or a backwards step cannot stretch the drain
  (in-file pins). The public `ReconnectionToken::is_expired`/`is_valid`
  wall-clock answers are embedder conveniences documented as not the
  admission decision; verified to have no server-internal caller.
- **Deadline overflow never inverts into immediate expiry (pinned,
  existing).** `deadline::after`/`saturating_after`/`wait_until` are pinned
  in file and consumed by the backpressure, reconnect-window, ping-write,
  and shutdown-wait seams; the shutdown deadline arithmetic saturates, and
  the dashboard's staleness conversion saturates
  (`chrono::Duration::from_std(..).unwrap_or(chrono::Duration::MAX)`).
- **Sibling sweep of expiry predicates (recorded dispositions).**
  Rate-limit window (inclusive,
  `window_admits_up_to_limit_then_expires_at_the_inclusive_boundary`),
  batching write deadline (inclusive,
  `selected_write_expires_at_or_after_deadline_without_completing_accounting`),
  room-state expiry at caller-injected now
  (`is_expired_at_times_windows_off_last_activity_at_the_callers_now`), and
  the database room-idle comparisons (strict, consistent with the in-memory
  reaper) are each pinned or inspected-consistent. The
  `InMemoryDistributedLock` lease predicates uniformly read `expires_at >
  now` with no dedicated fail-open/fail-closed branch; covered off-boundary
  by real-time tests and recorded here by inspection, since an exactly-at
  early expiry errs toward releasing a lease, the safe direction for a
  coordination lock and not player-visible.

The maintenance and deadlines coverage row moves to partially reviewed: the
expiry-boundary and clock-source families are reviewed and pinned, while
churn growth and dashboard cost remain measurement work for C3/C5. The
recovery slice continues with drain/shutdown racing active reconnect claims
and queued data, cleanup racing join/reconnect, and process-loss behavior.

### C1 drain/shutdown with queued data and active reconnect claims review (2026-10-03)

At `54feb610` (main after #726) with this review's fix, reviewed the
recovery slice's drain/shutdown family across `src/server/shutdown.rs`,
`connection_manager.rs`, `reconnection_service.rs`, `room_service.rs`,
`spectator_service.rs`, the coordinator's initial-transition registration
(`src/server.rs`), and the per-connection close path
(`src/websocket/connection.rs`). Two accounting defects were found and
fixed; every family carries a disposition.

- **A committed reconnect claim always reaches the close fan-out
  (fence verified).** `begin_shutdown_drain` takes
  `shutdown_drain_commit_gate` before flipping the drain atomic, so a
  baseline commit either finishes under the gate before the flip or sees
  `should_commit() == false` and refuses. The restored identity is inserted
  into `connection_manager` before the coordinator commit, so the later
  `close_connections_for_shutdown` iteration sees it; a fan-out that ran
  before the swap pins the transient socket, and the `Shutdown` reason is
  classified as crossing the identity swap and is carried into the restored
  connection's close signal (pinned,
  `reassign_still_adopts_entry_pinned_for_shutdown`). New upgrades are
  refused while draining, with a post-registration recheck pinning the
  inline 4000 if the flip races the insert. The committed-claim end state —
  `Reconnected` baseline, then the coded 4000 — is therefore guaranteed and
  is pinned piecewise: the wire shape by
  `shutdown_drain_sends_goingaway_and_closes_4000_without_reconnect_record`
  and the swap-crossing pin above.
- **A drain flip during a parked baseline reservation was misaccounted
  (fixed, new pin).** The initial-transition reservation's backpressure
  park had no drain arm, so the outer drain race dropped the parked future
  and the parked-wait guard resolved the attempt as
  `websocket_messages_dropped` — the sibling conditional-delivery park
  records the identical event as `websocket_deliveries_canceled` with an
  explicit not-a-drop pin. The reservation park now carries the same biased
  drain arm (cancellation precedence over capacity and expiry), and the
  pre-attempt refusal moved into the reservation itself (pinned by
  `pre_flipped_drain_refuses_the_initial_transition_before_counting`). New
  pin `drain_flip_during_initial_transition_park_cancels_instead_of_dropping`,
  red-proofed by reverting the fix: the flip counted a dropped message and
  left the cancellation ledger untouched.
- **Two silent releases of a reserved baseline (fixed, new pins).** A
  failed baseline build and a drain-gated commit refusal each released the
  reserved initial frame with no paired outcome for the counted attempt —
  the same class the #725 panic-accounting fix closed for transaction
  hooks. Reservation ownership now flows through
  `InitialTransitionReservationGuard`, defused only when the commit
  resolves the attempt. `drain_gated_commit_refusal_accounts_the_reserved_baseline`
  and `failed_baseline_builder_accounts_the_reserved_transition` and
  `dropped_registration_between_reservation_and_commit_accounts_the_attempt`
  (external cancellation mid-build) are red-proven against the unfixed
  revision;
  `committed_initial_transition_resolves_its_attempt_exactly_once` pins the
  no-double-count property on the committed path. Release-path pins assert
  the freed queue slot with a follow-up send on a one-slot channel.
- **Close ordering and queued data (contract-consistent, pinned
  piecewise).** The close frame is written by the per-connection send task
  after a bounded flush (one second per close step), so a coded 4000 never
  overtakes queued frames; a remainder past the budget is abandoned and
  counted, never written late. A cancelled mid-write frame makes the
  remainder abandoned instead of flushed (a gap-free prefix that stops
  early; a hole never). Per the protocol contract, messages abandoned
  during close need no gap records because the recipient's own disconnect
  terminates the observable stream; the conservation identity
  `attempted = delivered + abandoned + unsupported` holds per class, and a
  pending unsupported-format report is flushed before the close frame.
- **Shutdown closes are terminal for reconnection state (pinned,
  existing).** A draining unregistration skips the reconnection record,
  discards the pre-issued token, and hard-removes the room row; a record
  registered just before the flip is discarded when the removal observes
  the `Shutdown` reason
  (`draining_unregister_discards_reconnect_when_drain_starts_during_leave`,
  `shutdown_drain_sends_goingaway_and_closes_4000_without_reconnect_record`).
  Replay rings are instance-local memory, so a restart cannot resume them;
  the restart-invalidates-tokens behavior is pinned in the multiprocess
  suite.
- **Shutdown-close observability (recorded disposition).** The drain's
  close fan-out is logged but there is no exported counter distinguishing
  code 4000 closes; upgrade refusals during drain are counted
  (`websocket_upgrades_rejected_draining`) and abandoned queue frames are
  counted per class. A dedicated 4000-close counter is filed as follow-up
  observability work (#727), not a correctness defect.

The admin and shutdown coverage row moves to partially reviewed: the drain
choreography, its commit fence, queued-data close ordering, and reconnect
interactions are reviewed and pinned or fixed, while distinct-metric
export remains follow-up work. The recovery slice continues with cleanup
racing join/reconnect and process-loss behavior versus documented limits.

### C1 authentication admission boundary review (2026-10-03)

At `d70867db` (main after #734), reviewed the resource-and-input-safety
slice's unauthenticated-admission families across `src/auth/middleware.rs`,
`src/auth/rate_limiter.rs`, the room-side budgets in `src/rate_limit.rs`,
and the connection receive loop's handshake handling
(`src/websocket/connection.rs`). No defect was found; every family carries
a disposition and the two named remaining cases are now pinned.

- **Unauthenticated flood posture (verified).** Every `Authenticate`
  spends the app's handshake windows before credential verification
  (`resolve_app_id` precedes `verify_connect_token`), and any resolution
  or token refusal breaks the receive loop and closes the socket — a
  refused peer cannot retry on the same connection. Pre-handshake
  application frames are refused `MISSING_APP_ID` and close the
  connection; the log-safety gate rejects hostile app IDs before any
  policy path; app-ID length is capped (`MAX_APP_ID_LENGTH`).
- **Auth timeout boundary is absolute (pinned,
  `pre_handshake_activity_does_not_extend_the_auth_deadline`; red-proofed
  by making the deadline slide per read).** The pre-handshake deadline is
  frozen at `connection_start + auth_timeout_secs`; protocol-level
  keep-alives and other inbound frames neither extend it nor reclassify
  the cut as idle. The cut keeps code 4001 / `auth_timeout`, and a cut
  with received frames correctly stays out of the zero-frame disconnect
  counter. Config bounds (5–60s, no disable) were already pinned; the
  deadline is the only admission-bound deadline in the receive loop (the
  ping-write and pong deadlines are per-operation caps by contract, and
  the idle window is intentionally activity-reset).
- **Concurrent admission conserves the ceiling (pinned,
  `concurrent_handshakes_conserve_the_app_ceiling_and_count_every_rejection`;
  red-proofed by disabling app-window enforcement).** 8 sources × 2
  racing handshakes against a ceiling of 4 admit exactly 4 under the
  worst-case probe/commit race; each of the 12 rejections increments the
  aggregate and auth rejection counters exactly once. The enforcement rests
  on the commit-time re-check under the limiter entry lock
  (`concurrent_rate_limit_enforcement` pins the primitive); a rejection
  never charges the other window, and the documented worst case is one
  wasted stamp in the offending source's own window under a probe/commit
  race (the #502 split-budget pin covers the sequential composition; this
  pin covers the concurrent one).
- **Room-side budgets (verified, previously reviewed).** The room
  creation/join/signal/relay budgets check-and-charge atomically under
  the entry lock (`try_*`/`charge` on `RateLimitEntry`), the error-reply
  gate charges under the connection's own serialization, and the
  budget/refusal accounting pins from the earlier budget and error-reply
  sessions hold. `PlayerRejectionStats` forensics are saturated-add and
  reset with the window.

The Authentication coverage row moves to reviewed: unauthorized traffic
cannot enter a room, limits count refusals exactly once, concurrent
admission conserves the ceiling, and the auth timeout boundary is
absolute and activity-immune. The resource-and-input-safety slice
continues with queue/replay bounds, parser boundaries, and metrics label
cardinality.

### C1 metrics label cardinality review (2026-10-03)

At `85abff77` (main after #735), reviewed the resource-and-input-safety
slice's metrics-label-cardinality family across `src/metrics.rs`, the
Prometheus renderer (`src/websocket/prometheus.rs`), and the bounded
`/metrics` dashboard snapshot (`src/websocket/metrics.rs`). No defect was
found; every label surface carries a documented bound with a pin or tested
lifecycle wiring.

- **Per-connection ledgers are registration-scoped (verified, pinned).**
  `connection_delivery_stats` and `slow_consumer_eviction_attributions`
  key by live player id: registered at connection registration
  (`src/server/connection_manager.rs`), re-keyed in both directions across
  a reconnection identity swap, and removed at unregistration.
  `attribution_ledgers_accumulate_rekey_and_unregister` pins the
  accumulate/rekey/unregister/no-resurrection lifecycle, including the
  zero-ledger-exists-only-for-bounding rule.
- **Per-app relay attribution is allowlist-bounded (verified, pinned).**
  `app_relay_bytes` records only senders with an app policy
  (`check_and_charge_relay_bytes` charges under `if let Some(policy)`), so
  open-mode client-chosen app IDs never create series and the map is
  bounded by the allowlist.
  `allowlist_reload_prunes_revoked_app_relay_series` pins the #552
  pruning of a revoked tenant's series on allowlist reload.
- **Client-chosen game-name maps are response-bounded (verified).**
  `roomsByGame` and `gamePercentiles` are derived from the dashboard view
  per request and truncated by `bound_response_game_map` on both the
  current view and every history sample (issue #518); nothing accumulates
  them server-side.
- **Shutdown closes are counted at the coded close fan-out (new, #727).**
  `websocket_shutdown_disconnects` counts registered connections torn down
  with the server-initiated 4000 close, classified at the semantic close
  step after every reason override, so the drain close fan-out is
  observable without log scraping. The drain pin proves one closed
  connection counts exactly once, and the activity-reaper pin proves a
  non-shutdown close cannot land in the counter
  (`tests/close_code_semantics_e2e.rs`). A late registration refused
  during a drain never registered, so it stays under
  `websocket_upgrades_rejected_draining`.

The metrics label cardinality family is closed with bounded-series
dispositions. The resource-and-input-safety slice continues with
queue/replay bounds and parser boundaries.

### C1 parser boundaries and queue/replay bounds review (2026-10-03)

At `48bdc7fb` (main before this change), reviewed the
resource-and-input-safety slice's parser-boundary families (depth, size,
malformed frames, Unicode, numeric boundaries) across the ingress parsers
(`src/websocket/token_binding.rs`, `src/protocol/binary.rs`, `serde_json`
typed envelope decodes) and the conversion path
(`src/websocket/sending.rs`). One hardening was landed with differential
evidence: ARM-C030.

- **Depth (hardened).** rmp-serde decode into a recursive target leaned on
  that dependency's internal 1024-level guard, not a wire contract; the
  boundary is now an explicit 128-level scan before any recursive decode
  (ARM-C030 for the rationale, differential evidence, and pins). The same
  class was swept: the v3 binary envelope decodes through flat `rmp::decode`
  field reads, the JSON envelope decodes under serde_json's 128-level limit,
  and the token-bound binary envelope shares the scanner. rkyv and protobuf
  payloads are opaque to the server by design and are never decoded. On
  production 2 MB stacks no abort was reachable through the locked
  rmp-serde 1.3.1; the hardening removes the dependency-version dependence.
- **Size.** The transport caps inbound frames at `2 × max_message_size` and
  the receive loop re-checks `max_message_size` before parsing; per-encoding
  payload ceilings (`security.max_game_data_bytes`, #634) gate game data at
  admission; the conversion preflight bounds the decode input against the
  outbound budget before any allocation
  (`binary_fallback_decode_budget_rejects_compact_tree_before_cache_allocation`).
- **Malformed frames and numeric boundaries.** Every malformed JSON,
  MessagePack, and binary envelope returns a classified parse error with a
  budget-charged refusal; fatal classes disconnect with a farewell, other
  classes keep the connection. MessagePack integers outside the value domain
  and non-UTF-8 strings are decoder errors; the strict v3 envelope pins
  exact field types (`src/protocol/binary.rs` tests).
- **Unicode.** Player names canonicalize through `icu_casemap` and
  `unicode-normalization` with length caps (identity slice); wire strings
  are UTF-8-validated by both decoders before use.
- **Queue and replay bounds.** The delivery-side queue families (bounds,
  saturation, coalescing, counter conservation) carry their dispositions in
  the delivery reviews above; the reconnect replay ledger is bound by the
  reconnect epoch/sequence review (2026-10-01) and the drain review
  (2026-10-03). No new violation was reproduced this review.

With this review, the parser-boundary families have recorded dispositions.
The resource-and-input-safety slice continues with inactive records, pending
detach/claim retention, task ownership, rate-limit rejection accounting, and
error and logging paths under pressure.

### C1 inactive records, pending detach/claim retention, and task ownership review (2026-10-04)

At `dd37153b` (main after #737), reviewed the resource-and-input-safety
slice's inactive-record, pending detach/claim retention, and task-ownership
families across the connection manager, reconnection service and manager,
maintenance sweeps, spectator service, moderation, the WebSocket connection
loop, and the shutdown/drain paths. No defect was found; every audited
record and task carries a verified removal or abort on all exit paths.

- **Inactive records (verified clean).** Every audited map is bounded by a
  paired lifecycle: connection admission slots release exactly once on
  unregistration (with a loud saturated underflow guard); coordinator
  routing maps prune empty room sets and re-sync the active set on every
  mutation; client-supplied labels never key a server-side map (session
  plans, ready sets, room applications, and app relay series are
  server-keyed with maintenance prunes or allowlist-bounded; the
  upgrade-rejection log evicts deterministically). The reconnection
  replay/release families carry their dispositions in the delivery,
  epoch/sequence, and drain reviews above.
- **The pre-issued-token teardown branches are covered by layered
  discards (hypothesis falsified).** The teardown chain in
  `unregister_client_locked` handles draining, snapshot-registration, and
  no-room discards, and its snapshot-unavailable branches
  (`Ok(None)`, roster miss, storage error) fall through with no explicit
  discard. A review hypothesis claimed each such exit leaks the
  pre-issued token past teardown. The hypothesis is false: the same
  unregister flow always reaches the room-removal path, which discards the
  pre-issued token after the player leaves the room
  (`room_service.rs`), and the maintenance sweep independently cleans up
  clients whose room is missing. The pin
  (`unregister_snapshot_failure_creates_no_broken_reconnect_record`)
  already asserts both the pending-record absence and the pre-issued-token
  discard under a snapshot storage fault.
- **Pending detach/claim retention (verified clean).** Every
  `pending_durable_player_detaches` insert has a removal: direct
  resolution, reconnect reclaim, or the maintenance retry sweep (storage
  success, room deleted, or live reclaim). Spectator
  `pending_unpublished_detaches` entries resolve through owned rollback,
  republished-identity checks, or the retried sweep, and re-queue on read
  failure. Claim records are single-owner with `claim_id` verification on
  every mutation; every expiry surface filters out claimed records so an
  in-flight claim cannot be swept, and every rejection path funnels
  through the claim rollback.
- **Task ownership (verified clean; the one theoretical residual is now
  fixed).** Per-socket
  send/receive tasks are joined; the ping and relay-stats tickers exit on
  the socket close signal that every unregistration requests. Server-level
  tasks are owned: the drain task is joined or aborted at shutdown, the
  cleanup loop is aborted on cancellation via a task-abort guard, the
  dashboard-cache and rate-limiter sweeps hold `Weak` owners and
  self-terminate, and lease renewal aborts on drop. The theoretical owned
  reconnect-transaction residual (#738, ARM-C037 below) is fixed: the
  in-task unwind supervisor is itself `catch_unwind`-guarded with a direct
  claim-release fallback, pinned by
  `supervisor_panic_still_releases_the_reconnect_claim_for_a_fresh_retry`.

With this review, the inactive-record, pending detach/claim retention, and
task-ownership families have recorded dispositions.

### ARM-C037 — A supervisor panic could strand the reserved reconnect claim

| Field | Record |
| --- | --- |
| State, severity | Fixed, low (theoretical: required a second-order unwind that production code could not produce; filed as #738 from the 2026-10-04 task-ownership review) |
| Player impact | If reached, permanent: a claimed reconnect record is exempt from every expiry surface (`expired_cleanup_candidates`, `remove_expired_reconnection`, `cleanup_expired` all filter `record.claim.is_none()`), so the disconnected player's every reconnect attempt would answer `AlreadyInProgress` until process restart — a stuck seat plus a consumed one-time token. |
| Source and revision | `src/server/reconnection_service.rs::spawn_reconnect_transaction`, reviewed at `dd37153b`. The transaction's `catch_unwind` covered `handle_reconnect_owned`, but the in-task unwind supervisor arm that handles its panic ran OUTSIDE that guard while the recovery snapshot (`ReconnectPanicRecovery`) may already hold the reserved claim — the sole claim reservation sits inside `handle_reconnect_owned`. An adversarial falsification pass confirmed the pre-supervisor region is panic-free and nothing aborts the inline-awaited `JoinHandle`, so no production path reached the strand today; the invariant was hygiene-by-convention across the supervisor's callee tree, not a structural property. The join seam's stranded creation lock self-heals via the distributed TTL lease (`src/distributed.rs`); the leave seam reserves nothing — the reconnect claim was the only permanent-strand surface. |
| Invariant | A panic anywhere in an owned transaction's recovery path must not leave reserved, expiry-exempt state stranded; the recovery layer is part of the panic surface it manages. |
| Confidence and reproduction | Red-first: a one-shot test seam (`panic_owned_room_supervisor_for_test`) fires at the top of the supervisor body while `ReconnectAfterReassignment` has already populated the snapshot's claim. Pre-fix, the supervisor panic escaped as a `JoinError`, the log-only arm returned, and the fresh-retry `claim_reconnection` failed (`supervisor_panic_still_releases_the_reconnect_claim_for_a_fresh_retry` red at the retry expect). An earlier outer-`JoinError`-arm recovery shape was rejected: holding the snapshot `Arc` across `task.await` extends the snapshot's `RoomEventMutationGuard` hold past the task boundary and deterministically broke `correlated_reconnect_panic_after_terminal_does_not_send_a_second_result` — recovery must stay inside the task. |
| Disposition | The supervisor body is now wrapped in its own `AssertUnwindSafe(..).catch_unwind()`. On a supervisor panic it logs `Owned reconnect transaction unwind supervisor panicked` and resolves the record by the transaction's commit state, mirroring the supervisor's own branch: a committed terminal response consumed the one-time token, so the record is completed (consumed); an uncommitted transaction leaves the token unspent, so the record is released for a fresh retry. Both manager actions are claim-id-checked with warn no-ops when the supervisor already acted before panicking. The full rollback is deliberately not retried — code that just panicked can panic again — and the record actions are panic-free (tokio-lock based, poison-tolerant snapshot read). The outer `task.await` error arm stays log-only: with the supervisor guarded, reaching it requires runtime-shutdown cancellation, where all in-memory claim state dies with the process. PR #743 review (Cursor Bugbot) caught the fallback's first shape releasing unconditionally, which would have reopened a delivered token; red-proven by `supervisor_panic_after_terminal_consumes_the_delivered_token` beside the fresh-retry pin. Pre-existing residual noted, not introduced here: a panic between `complete_claimed_reconnection` and the `opening_accounted` swap skips the `players_joined` increment; left to the metrics row. |

### C1 rate-limit rejection accounting review (2026-10-04)

At `dd37153b` (main after #737), reviewed the resource-and-input-safety
slice's rate-limit rejection accounting family across
`src/rate_limit.rs`, the auth sliding windows, the error-reply gate, the
join/spectator/signal lanes, relay byte charging, and connection admission.
No defect was found: every refusal path charges exactly once, every counter
lands on the budget that refused, and no path double-charges or inflates.

- **Charges and counters are paired exactly once (verified).** The compound
  room-creation charge is both-or-neither; the relay charge is all-or-nothing
  per budget with the room-ceiling rejection deliberately retaining the
  sender charge (pinned); the signal preflight never consumes and records a
  rejection only when it owns the drop; every retryable handshake, parse,
  moderation, authority, and drain refusal charges the error-reply gate
  exactly once via the charged helpers, and farewell/terminal refusals never
  charge. Auth windows keep rejections free via probe-before-commit, with
  the documented one-self-tightening race still counting every rejection
  (pinned). Admission refusals consume no slot, so no budget is consumed
  unaccounted.
- **The drain-window creation refusal is deliberately budget-free (now
  pinned).** The create-while-draining fast-path runs before the
  join/creation budget charge, while everything else refused in the same
  window spends its bucket. The drain comment shows the choice is
  deliberate: the drain-window flood bound is the charged error-reply gate
  (issue #518), not the join bucket. The asymmetry is now pinned by
  extending `draining_server_rejects_room_creation_without_consuming_join_locks`:
  the drain-window creation refusal creates no budget entry, and an
  existing-room join admitted in the same window spends exactly one join
  attempt.

With this review, the rate-limit rejection accounting family has a recorded
disposition.

### C1 error and logging paths under pressure review (2026-10-04)

At `dd37153b` (main after #737), reviewed the resource-and-input-safety
slice's error/logging-under-pressure family across the WebSocket hot loop,
the relay send path, heartbeat, the messaging helpers, and the subscriber
in `src/logging.rs`. Two hardenings landed with this review (ARM-C031,
ARM-C032); everything else is verified bounded or charged.

- **Rejected app-ID log forged log lines (fixed, ARM-C031).** The
  `Public app ID rejected` warning logged the raw client-supplied app ID
  with a Display field in exactly the arm where the log-safety gate had
  rejected it, so control characters in a rejected ID could forge operator
  log lines. The ID is now Debug-escaped, matching the pre-gate escaping
  pattern already used at the sibling anomalous-path warnings. A sweep of
  every remaining `%app_id` log site found only gate-vetted or typed-UUID
  fields.
- **Undeliverable-relay warning flooded logs (fixed, ARM-C032).** The
  per-recipient warning in `notify_on_undeliverable` fired once per
  undelivered frame per recipient with no throttle, while the in-band
  advisory it accompanies was already rate-limited to one per sender per
  second; one sender with an unsupported encoding into a large room could
  flood operator logs. The warning now rides the advisory cadence and
  carries the suppressed count. Delivery reports, advisories, and drop
  counters are unchanged.
- **Error-reply amplification (verified clean).** Every polite per-frame
  reply charges the per-connection budget and exhausts to one `4006`
  close; relay error branches drop the lifecycle gate before re-acquiring
  it in the refusal reply; teardown writes are deadline-bounded and
  first-wins.
- **Log content and volume (verified clean, one disposition).** No
  hot-path log includes raw frame bodies, payloads, or credentials; parse
  violations log decoder errors without content; upgrade and metrics
  rejections use per-source quiet periods. The heartbeat
  persistence-failure warnings fire inside the per-player heartbeat
  throttle (default 30 s cadence); the documented `Duration::ZERO`
  "update every message" mode combined with a persistently failing
  database would warn per frame. This is accepted: the mode is explicit
  operator configuration, the default cadence bounds the warnings, and the
  failure itself is loud by design. The subscriber in `src/logging.rs` is
  a cold path — filter selection, JSON/text formatting, and a rolling
  file appender that fails open to stdout (pinned by
  `file_appender_init_failure_falls_back_without_panicking`, guard
  deliberately leaked for process lifetime) — and carries no payload
  content, so the call-site review above covers the family.

With this review, the error and logging paths under pressure family has
recorded dispositions. The resource-and-input-safety slice closes with one
carry-over: its configuration-validation family is half dispositioned —
reload consistency carries the 2026-10-01 allowlist/key reload review,
while default coherence and validation breadth (malformed documents,
env-override interactions) remain with the Config and reload coverage row
below.

### C1 client and deployment boundaries review (2026-10-04)

At `a0af63b1` (main after #739), reviewed the C1 slice's client and
deployment boundary families: the four shipped reference clients
(`clients/browser`, `clients/native`, `clients/fortress`,
`clients/fortress-wasm`) against the reconnect/report/fallback/negotiation
contract, and the plain/TLS listener boundary with the optional features.
Four parallel audits; one reproduced defect class fixed; every family
carries a disposition.

- **Reference-client downgrade abort (fixed, ARM-C033).** The server
  refuses an unsupported requested `game_data_format` with a budget-charged
  `Error` enqueued BEFORE `Authenticated` (pinned wire order, server
  `tests/e2e_tests.rs`) and downgrades the session to JSON. Both the native
  and the browser reference client kept a first-frame gate that accepted
  only `Authenticated`/`AuthenticationError`, so an opaque request against
  a default (knob-off) deployment aborted a viable, contract-conformant
  session: native exit 2 `expected Authenticated, got Error` (reproduced
  live against a default-config server), browser the same fatal at the
  equivalent gate. The intended pre-room `ProtocolInfo.game_data_formats`
  diagnostic was unreachable in both clients for this ordering. Both
  clients now consume exactly one downgrade notice, adopt JSON for every
  post-handshake wire decision (send shape, inbound classifier, format
  gate), emit a non-fatal notice event, and stay fatal for repeat notices,
  other error codes, and authentication refusals. Native pins:
  `handshake_downgrade_error_adopts_json_and_continues`,
  `handshake_stays_fatal_for_other_frames_and_repeat_notices`. Browser pin:
  the `advanceAuthenticateHandshake` block in `orchestrator.test.ts`.
- **Native downgrade live cells (session 362, #741 item 1).**
  `unsupported_opaque_requests_downgrade_to_json_and_relay_between_reference_clients`
  runs rkyv and protobuf requests against a real server with default encoding
  knobs. Each cell runs two native client processes. Both must consume one
  downgrade notice
  before `Authenticated`, negotiate v3, reach the shared success barrier,
  and exit successfully. Each peer must receive exactly the other's JSON
  payload with the sender UUID and v3 delivery stamps. This extends the
  ARM-C033 unit controls to the handshake, format adoption, relay, and
  process completion paths. A temporary mutation that retained the refused
  encoding failed before room creation. The restored client passed both
  cells. Restore exercisability remains open in #741.
  The staged changelog check rejected this test-only work (#797). The checker,
  hook, and CI dependency detector now exempt the test directories under the
  four known client roots. Literal roots keep nested runtime `src/tests`
  paths outside the exemption. Data-driven controls cover both classes and
  mixed test/runtime changes. No runtime change or changelog entry is needed.
- **Native cross-format accountability live cell (session 364, #741 item 2).**
  The opted-in rkyv interop scenario keeps its native-to-native relay wave,
  then starts a native JSON recipient and a raw rkyv WebSocket peer on the
  same server. The peer sends one opaque payload and a valid JSON continuation.
  The recipient must consume the exact `UnsupportedFormat` gap report before
  its advisory, receive only the continuation with the sender UUID and v3
  stamps, meet its normal success criteria, and exit successfully. This
  exercises the shared native accountability path for opaque refusals. It
  does not add a protobuf cross-format cell. The follow-up wave has one
  absolute deadline and leaves the existing healthy wave's deadlines intact.
  Removing native report validation made the process reject the advisory
  for lacking a prior causal report. The restored source passed the live cell.
- **Native Fortress negative control (session 366, #741 item 6).**
  Two native game processes drive 600 active callbacks with at most one
  admission per callback. They then drain their outbound queues and complete the
  existing final acknowledgement exchange. The healthy validator must reject
  both reports for completed-send rate and sends per callback. The control
  also requires a substantial workload, matching delivery ledgers, zero relay
  faults, and successful process exits. Active traffic metrics freeze before
  final drain so drain writes cannot inflate the measured rate.
  Disabling the shared throughput checks made the live negative cell fail.
  The restored validator passed both live cells. A mutex isolates their timed
  workloads under the hosted runner's normal parallel test settings.
  The final healthy run exposed zero rollback on one peer despite clean
  delivery and checksum gates (#804). The healthy socket holds gameplay inputs
  from startup until the local game reaches frame four. Synchronization controls
  still flow. This prevents a startup backlog from bypassing prediction and
  forces repair within the existing eight-frame window. The socket releases
  retained inputs in order, then returns to its normal delivery path.
  Fortress source is private compatibility-fixture code. The changelog gate
  now classifies the two literal Fortress source roots as internal (#803).
  Native and browser reference-client runtime changes still need release notes.
- **Native Fortress drain and restart (session 367, #741 item 7 slice).**
  A Unix-only cell waits for both native peers to publish atomic gameplay
  checkpoints. Each checkpoint proves an unfinished game at or beyond frame 120,
  rollback repair, matching checksums, bidirectional traffic, and zero relay
  faults. The harness verifies both peers are alive before it sends SIGTERM
  to the real server with a one-second drain grace.
  Both peers must fail within one shared bound. Each failure must include
  the shutdown advisory and the server's coded 4000 `server_shutdown` close.
  A successful report, an unrelated error, a panic, or the normal process
  deadline cannot satisfy this control. The server must exit successfully
  within the separate drain bound.
  The harness starts a new server on the same port. Fresh peers with new
  identities must complete the full healthy workload and delivery gates.
  This proves service after restart, not restoration of the interrupted game.
  Suppressing the disconnect abort made the live cell fail on an unrelated
  send error despite receiving the shutdown advisory. The restored fixture
  passed the drain cell and both existing live workloads.
  Process and file guards clean owned resources on assertion failure.
  Inbound overflow and restore exercisability remain open in #741.
  This cell does not cover WASM shutdown behavior.
- **Native Fortress handshake close (session 368, #741 item 7 slice).**
  A Unix-only cell allows the first typed synchronization reply through the
  relay and retains later replies in its existing bounded FIFO. Requests and
  other controls still flow. Both peers must publish an atomic checkpoint
  after a successful partial synchronization step, with zero gameplay frames
  and positive completed traffic in both directions.
  The harness then sends SIGTERM to the real server. Both peers must receive
  the shutdown advisory and coded 4000 `server_shutdown` WebSocket close,
  then exit normally with code one within the shared fault bound.
  Each peer reports its phase again when it handles the close. The harness
  checks the same identity, partial synchronization, zero gameplay frames,
  and bidirectional traffic at the close. An earlier checkpoint cannot substitute
  for this close-time proof.
  Disabling the reply gate made the live cell fail because a peer had reached
  `Running` at the close. The source was restored before final validation.
  Healthy peers and the WASM fixture keep their normal reply delivery.
  Inbound overflow, restore exercisability, and WASM fault cells remain open
  in #741.
- **Native Fortress inbound overflow (session 369, #741 item 7 slice).**
  Both real peers first demonstrate healthy gameplay, rollback, and matching
  checksums. The harness stops both game consumers at explicit file barriers
  while transport polling continues. The creator sends genuine captured
  Fortress inputs in fresh, contiguous relay envelopes. The queue limit
  remains 256 frames. The receiver must retain exactly 256 frames, identify
  the first rejected application sequence and server sequence/epoch, and
  exit with code one within ten seconds. The sender observes the departure;
  the server remains live. No healthy report, panic, or deadline expiry is
  accepted. Replacing the overflow error with silent dropping makes the live
  cell fail its bounded-exit assertion. The shared native/WASM adapter now
  returns overflow evidence; both owners propagate the error immediately.
  Native unit coverage pins the admission boundary with valid codec bytes.
  Restore exercisability and WASM fault cells remain open in #741.
- **WASM Fortress active-game drain (session 370, #741 item 7 slice).**
  The browser harness waits for a Rust-origin `Running` checkpoint from each
  no-thread Godot runtime. Each checkpoint requires confirmed gameplay,
  rollback, matching checksums, and completed bidirectional relay traffic.
  It then sends SIGTERM to the real server. Both runtimes must stop within
  ten seconds with the shutdown advisory and coded 4000 `server_shutdown`
  close. The advisory stops new gameplay while transport polling continues
  until each peer receives its own close. Final reports retain the same
  identities and unfinished gameplay;
  healthy completion, unrelated failures, panic, and deadline expiry refuse
  acceptance. The server must exit normally. Existing healthy and capped
  admission cells retain their exact delivery-ledger checks. Interrupted
  games allow in-flight frames at shutdown. Validator controls reject the
  wrong close code, wrong cause, missing advisory, and vacuous checkpoints.
  WASM restart, partial-handshake close, inbound overflow, and reference
  restore exercisability remain open in #741.
- **WASM Fortress restart (session 371, #741 item 7 slice).**
  The runner first requires the active-game drain proof above. It closes
  both old browsers and waits for the server to exit normally. It then
  starts the same binary on the same port. Two fresh no-thread Godot peers
  must pass every released healthy, runtime-identity, and exact delivery-ledger
  gate. The harness keeps separate drain and restarted-game artifacts, with
  the binary, build, port, process, and peer identities in a restart record.
  Identity controls reject reuse across either peer role. Partial-handshake
  close, inbound overflow, and restore exercisability remain open in #741.
- **WASM Fortress partial-handshake close (session 372, #741 item 7 slice).**
  The `sync-close` cell waits for both Rust runtimes to report an actual
  `Synchronizing` event with nonzero, incomplete progress and bidirectional
  handshake traffic. The relay releases one sync reply and retains later
  replies, so a retry backlog cannot finish the handshake in one poll.
  Each runtime freezes Fortress progression at that point
  and continues polling the real signaling transport. The harness drains the
  server and requires both peers to report its advisory and coded 4000 close
  within ten seconds, with no gameplay or unrelated relay fault. It keeps
  each pre-close checkpoint and terminal report. Validator controls reject
  empty or completed handshakes, early gameplay, missing traffic, and wrong
  close causes. Healthy and drain cells hold startup inputs through local
  frame four, as the native fixture does, to force a real prediction and
  correction before requiring rollback evidence (#815). A paired-session
  control covers smooth delivery with zero rollbacks and the configured
  correction path. Failure artifacts retain periodic Rust-origin probe
  metrics and active/sync checkpoints. WASM inbound overflow and reference
  restore remain in #741.
- **Opaque over a v2 negotiation (fixed with the same sweep).** The native
  client validated an opaque request against the requested version only;
  the `ProtocolInfo` `None`-version (v2) arm skipped every format check,
  and the browser's advertisement gate is version-blind. `game_data_formats`
  is not version-gated on the wire, so a v2-capped deployment advertising
  rkyv would have carried the clients' attribution-less binary shape over
  v2, violating the reference clients' own v3-only opaque constraint
  (#627). Both clients now refuse an opaque request on a sub-v3
  negotiation (`opaque_request_on_a_v2_negotiation_is_refused` native; the
  `negotiated < 3` guard browser-side).
- **Docs drift (fixed, ARM-C034).** `clients/README.md` still said the
  Fortress fixtures pin released client 0.8.0/0.9.0; both fixtures pin
  `=0.13.0` since #588. The browser README described the runtime as
  JSON-only, which stopped being true when opaque negotiation landed.
  Both corrected.
- **Sound families (verified, no defect).** Browser: delivery reports
  (exact gaps, counter deltas, advisory causality, `RelayStats`),
  fallback (plan replacement, ICE loss, `PeerTransportStatus`), and
  version/format selection match the server; reconnect initiation is
  absent by documented scope (`clients/browser/README.md`), with the
  inbound `Reconnected`/watermark arms correct at unit level. Native:
  accountability model, transport fallback (cripple, TURN bad-secret,
  host-star, Direct rejection), and negotiation happy paths match and are
  pinned; reconnect initiation likewise absent by documented scope
  (`docs/guides/building-a-client.md` marks `Reconnect` optional) and the
  restore contract has zero native exercisability. Fortress fixtures: the
  "without silent loss" invariant is substantiated by executable CI gates
  on both stacks (cross-peer ledger equality, contiguity, queue-age and
  checksum gating; both families add an asserted expected-negative
  control); they are WebSocket-only, so they evidence no
  ICE-fallback claim.
- **Deployment boundaries (verified, gaps filed).** The listener cannot
  half-start: every fallible startup step precedes the bind, the listener
  socket exists only inside `bind_tcp_listener`, and start logs follow the
  bind on both paths. TLS cannot be silently enabled (validation refuses a
  `tls.enabled` config on a non-TLS binary, pinned + CI lane) and TLS-on
  fails closed pre-bind on invalid material; mTLS token binding is pinned
  e2e over the real binary. `legacy-fullmesh` is binary-local, spawns a
  separate unauthenticated plane on port+1 with a loud warning and a
  collision guard, and cannot be reached from the main router.
  `trace-validation` and `allocation-tracking` are inert dev seams.
  The ARM-C035 and ARM-C036 coverage gaps are closed (issue #740): the
  drain→4000 close path over TLS and the failure-after-partial-startup
  regressions are pinned over the real binary in
  `tests/tls_deployment_boundaries_e2e.rs`. The Fortress native harness
  gaps remain tracked (#741).

With this review, the client and deployment boundary slice has recorded
dispositions: the client rows move to partially reviewed with the missing
cases named below, and the deployment gaps are filed instead of open.

### ARM-C033 — Reference clients aborted the server's JSON downgrade handshake

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | On any deployment without the opaque-encoding knobs (the defaults), a reference client requesting `rkyv`/`protobuf` could not join at all: the session died at the handshake with `expected Authenticated, got Error` instead of continuing on the JSON floor the protocol guarantees. |
| Source and revision | `clients/native/src/client.rs` `authenticate` first-frame gate and `clients/browser/src/page/orchestrator.ts` `authenticate` gate, reviewed at `a0af63b1`; server side `src/websocket/connection.rs` (Error enqueued before `Authenticated`, session downgraded to JSON). |
| Invariant | A refused `game_data_format` downgrades the session to JSON; a client must consume the pinned notice and continue on the downgraded negotiation. |
| Confidence and reproduction | Native reproduced live: default-config server + `--protocol-version 3 --game-data-format rkyv --create-room` → exit 2 with `expected Authenticated, got Error`. Browser identical by code reading (same gate shape, same pinned server order). |
| Disposition | Both clients consume exactly one downgrade notice and adopt JSON for every post-handshake wire decision; repeat notices, other error codes, and auth refusals stay fatal. Pins listed in the review section above. |

### ARM-C034 — Client documentation described stale pins and a JSON-only runtime

| Field | Record |
| --- | --- |
| State, severity | Fixed, low |
| Player impact | None on the wire; client authors were told the Fortress fixtures validate SDK 0.8.0/0.9.0 (actual pin `=0.13.0` since #588) and that the browser runtime never accepts binary frames (opaque negotiation does). |
| Source and revision | `clients/README.md:20,21,58` and `clients/browser/README.md` binary-frame paragraph, reviewed at `a0af63b1`. |
| Invariant | Shipped documentation describes the shipped flag surface and pin set. |
| Confidence and reproduction | Direct file comparison against `clients/fortress/Cargo.toml` / `clients/fortress-wasm/Cargo.toml` and `wire.ts`'s format-conditional classifiers. |
| Disposition | Both documents corrected in this change. |

### ARM-C035 — The drain→4000 close path over TLS has no test

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium (coverage gap closed 2026-10-04, issue #740) |
| Player impact | A regression in the TLS shutdown wiring or rustls close would give wss players abrupt closes without the coded `4000 server_shutdown`, undetected by CI; the plain-socket contract is pinned, the TLS path was not. |
| Source and revision | `src/main.rs` TLS serve/shutdown wiring; plain-only contract test `tests/close_code_semantics_e2e.rs`; reviewed at `a0af63b1`. |
| Invariant | The drain choreography (GoingAway advisory, coded 4000, bounded exit) holds identically over TLS. |
| Confidence and reproduction | Direct proof of absence: the mTLS e2e spawns the real TLS binary but never exercises shutdown; no TLS drain test existed. No defect claimed. |
| Disposition | `tests/tls_deployment_boundaries_e2e.rs::shutdown_drain_over_tls_advises_then_closes_4000_and_exits_bounded` spawns the real TLS binary, seats a v3 client over wss, delivers SIGTERM, and pins the advisory (deadline anchored at the drain, `retry_after` mirroring the configured grace), the `4000 server_shutdown` close, and a clean bounded process exit. Unix-only (the drain trigger is a signal); Windows keeps the plain-socket pin. Red-proven: with the configured 1 s grace replaced by the 30 s default, the deadline/retry-after assertions fail loudly. |

### ARM-C036 — Failure after partial startup has no regression coverage

| Field | Record |
| --- | --- |
| State, severity | Fixed, low (coverage gap closed 2026-10-04, issue #740; invariant verified holding at `a0af63b1`) |
| Player impact | None today; a future regression could ship a half-started or falsely-announced server unnoticed. |
| Source and revision | `src/main.rs` startup order and `src/websocket/routes.rs::bind_tcp_listener`, reviewed at `a0af63b1`. |
| Invariant | A fallible startup step after background-task spawn (bind conflict, invalid TLS material) must abort with no listener and no "Server started" announcement. Holds by construction: every `?` precedes the bind and logs follow it. |
| Confidence and reproduction | Direct proof of absence: no EADDRINUSE / invalid-PEM / log-ordering test existed. |
| Disposition | Two regressions in `tests/tls_deployment_boundaries_e2e.rs` pin the invariant (unix-only; Windows keeps the in-process order verification): `port_bind_conflict_exits_nonzero_without_start_announcement` (an occupied port must exit non-zero, attribute the abort to the address-in-use error, and announce no start) and `invalid_tls_pem_exits_nonzero_without_announcement_or_listener` (invalid PEM must exit non-zero, attribute the abort to the TLS material, announce nothing, and leave no listener reachable on the port). |

### C1 config and reload coverage review (2026-10-04)

At `a479d3a4` (main), reviewed the last two open families of the Config and
reload coverage row: default coherence and validation breadth (malformed
documents, env-override interactions). Three parallel audits (defaults
coherence, malformed documents, env-override interactions), every candidate
defect reproduced against the real binary before the fix, five defect classes
fixed (ARM-C038..ARM-C042 below), one default-coherence defect fixed
(ARM-C043), and every family carries a disposition.

- **Non-object JSON source silent revert (fixed, ARM-C038).** A JSON source
  whose root was not an object replaced the whole merged document
  (`merge_values` catch-all), so a truncated write leaving `null` in a
  mid-precedence `config.json` silently discarded every lower-priority source
  and booted on compiled defaults plus whatever the higher sources set —
  exit 0, process healthy (reproduced live: `null` cwd file + inline
  `{"port":1234}` → port 1234, all other knobs default). Position-dependent:
  the same `null` in the highest source hard-errored. Now any present source
  with a non-object root is a hard error naming the source and the root kind,
  at every precedence position
  (`non_object_json_source_is_a_hard_error_naming_the_source`).
- **`logging.level` silent revert (fixed, ARM-C039).** The custom
  `LoggingConfig` deserializer downgraded an unrecognized level string to a
  stderr note and a wrong-typed value (number/bool/object) to a silent
  `None` — the only knob where a present-but-invalid value did not fail
  `load()`, contradicting the loader contract and the sibling
  `logging.rotation` treatment (reproduced live: `level:7` → exit 0,
  `level: null`, no diagnostic). The field now parses through `LogLevel`'s
  own strict deserializer; case/whitespace tolerance and the
  `warning`/`err` aliases survive, unknown strings and non-string values are
  hard errors (`invalid_log_level_string_is_a_hard_error`,
  `non_string_log_level_is_a_type_error`,
  `log_level_aliases_and_case_are_still_accepted`).
- **Stat-failing config file treated as absent (fixed, ARM-C040).**
  `read_file_source` gated on `Path::exists()`, which reports `false` for a
  broken symlink, symlink loop, or unsearchable parent, so a configured file
  that could not be stat'ed was silently skipped in favor of lower-priority
  sources (reproduced live: `SIGNAL_FISH_CONFIG_PATH` at a symlink loop →
  defaults, exit 0; a dangling symlink behaved identically). Now the source's
  directory entry is checked without following the final symlink
  (`symlink_metadata`): `NotFound` there is the one tolerated outcome, and
  every present-but-unreadable shape — dangling symlink, symlink loop,
  permission errors — is a hard error naming the path
  (`config_file_that_fails_stat_is_a_hard_error_naming_the_path`, covering
  both symlink shapes).
- **Env override key depth overflow (fixed, ARM-C041).** `set_nested_value`
  and the resulting `Value`'s recursive drop recursed once per `__` segment
  with no cap; `std::env` permits far deeper keys than any config path.
  Reproduced live: a 40k-segment `SIGNAL_FISH__A__A__…` variable aborted the
  process at startup with a stack overflow (SIGABRT). Overrides beyond 16
  segments (`MAX_ENV_OVERRIDE_DEPTH`; the deepest canonical path is 4) now
  fail with a named error
  (`env_override_deeper_than_the_segment_cap_is_a_hard_error`). The same
  class was already closed on the document path: serde_json's 128-level
  recursion limit bounds every JSON source and env JSON value, now pinned for
  the config path (`deeply_nested_config_source_is_a_parse_error_not_a_crash`).
- **Order-dependent case-variant duplicate env overrides (fixed, ARM-C042).**
  Two case variants of one override name (`SIGNAL_FISH__PORT` and
  `SIGNAL_FISH__port`) map to one knob; resolution was first-in-iteration
  (unspecified `std::env::vars()` order) behind a misleading
  "canonical and legacy" warning (reproduced live). Same-class duplicates now
  hard-error naming both variables; true canonical-vs-legacy alias pairs keep
  their pinned order-independent canonical-wins resolution
  (`duplicate_case_variant_env_overrides_are_a_hard_error_naming_both_vars`;
  existing `canonical_app_access_env_keys_win_over_legacy_keys_in_either_order` /
  `canonical_app_list_env_wins_over_legacy_list_in_either_order` re-run green).
- **Default per-IP budget equaled one default roster (fixed, ARM-C043).**
  `default_max_connections_per_ip` (24) equaled exactly one fully seated
  default room (8 players + auto-derived 2× spectators = 16), leaving zero
  per-IP slack for reconnect churn behind one NAT, while its own comment
  claimed 16-player headroom (arithmetically 48 seats). The default is 64
  with a corrected comment, pinned by a data-driven coherence test
  (`default_per_ip_budget_admits_a_full_nat_roster_with_churn_slack`); the
  config-reference drift guard and docs were updated with it.

Clean dispositions:

- **Defaults vs guards:** with the documented-by-design metrics-credential
  gate satisfied, `Config::default()` passes every numeric, cross-field,
  protocol-window, transport, TURN, session, metrics, logging, and rate
  guard (guard-by-guard trace recorded in the session notes); enum defaults
  round-trip through the loader's serialize→merge→deserialize pipeline.
- **Malformed documents:** unparsable JSON, wrong types at any depth (file
  side now pinned beside the pinned env side:
  `wrong_typed_file_value_is_a_hard_error_naming_the_knob`), u8/u64
  overflow, floats on integer knobs, invalid UTF-8, BOM, trailing garbage,
  directory-at-path, and permission errors all hard-error with source or
  knob attribution. Duplicate JSON keys are last-wins by serde semantics;
  legacy normalization runs post-dedup and cannot be confused by them.
- **Unknown keys:** the deny-strict-security / tolerate-elsewhere split is
  deliberate, structurally pinned, and operator-documented; removed-key
  tolerance is pinned. The one credential-bearing structure outside the
  security subtree (`session.ice_servers` entries) tolerates unknown keys by
  the same documented policy, and a missing TURN username/credential is
  warned at validation with the entry index.
- **Env interactions:** precedence (env over every JSON source, source order
  1→6 as documented), scalar/JSON/comma-array value parsing, empty-value
  refusals, secret redaction (`REDACTED_SECRET` + legacy discard paths hard-
  error or strip on every intake path), the app-registry merge (append, not
  replace, with downstream duplicate rejection), and the connect-token
  inline-vs-path conflict are all pinned or dispositioned. The SIGHUP reload
  re-runs the identical load pipeline (sources, env, folds, validation), so
  startup and reload accept/reject identically.
- **Docs vs code:** the ~70-row configuration reference matches the code
  defaults; the audit's three documentation drift findings (run-modes rows
  whose "exact command" fails under the fail-closed metrics gate, the
  development snippet missing `require_metrics_auth`, the README Docker
  trial missing the CORS override its browser-console step needs) are fixed
  in this change.

### ARM-C038 — A non-object JSON config source silently discarded lower-priority sources

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium |
| Player impact | A corrupted/truncated write leaving `null` (or a hand edit leaving an array/scalar) in a mid-precedence `config.json` reverted every lower-priority operator setting (allowlists, caps, timeouts) to compiled defaults while the process appeared healthy; under the documented README posture where env carries the security knobs, the silent revert was fully invisible. |
| Source and revision | `src/config/loader.rs` `parse_json_document` (accepted any JSON root) and `merge_values` catch-all (non-object source replaced the accumulated document), reviewed at `a479d3a4`. |
| Invariant | A config source that is present but invalid is a hard error naming the source, at any precedence position — never a silent substitution of defaults. |
| Confidence and reproduction | Reproduced live at `a479d3a4`: cwd `config.json` containing `null` + `SIGNAL_FISH_CONFIG_JSON='{"port":1234}'` → exit 0, port 1234, every other knob at compiled defaults. |
| Disposition | `parse_json_document` requires an object root and errors naming the source and root kind (`non_object_json_source_is_a_hard_error_naming_the_source`); the loader contract doc lists the non-object case explicitly. |

### ARM-C039 — An invalid `logging.level` silently reverted to the default

| Field | Record |
| --- | --- |
| State, severity | Fixed, low |
| Player impact | None directly; an operator typo (`"warng"`) or wrong-typed value (`7`) started the server on the default log level — on a deploy expecting `trace` or `error`, diagnostics were silently thinner than configured. |
| Source and revision | `src/config/logging.rs` custom `LoggingConfig` deserializer (`Option<serde_json::Value>` + lenient coercion), reviewed at `a479d3a4`. |
| Invariant | A present-but-invalid config value fails `load()` — the same treatment every other knob gets — instead of silently reverting. |
| Confidence and reproduction | Reproduced live at `a479d3a4`: `SIGNAL_FISH_CONFIG_JSON='{"logging":{"level":7}}'` → exit 0, `level: null`, zero diagnostics; `"warng"` → exit 0 with only an unspecific stderr note. |
| Disposition | The field is `Option<LogLevel>`, parsed by `LogLevel`'s strict deserializer (case/whitespace-tolerant, `warning`/`err` aliases kept); unknown strings and non-string values are hard errors. Pins: `invalid_log_level_string_is_a_hard_error`, `non_string_log_level_is_a_type_error`, `log_level_aliases_and_case_are_still_accepted`. |

### ARM-C040 — A stat-failing config file was silently treated as absent

| Field | Record |
| --- | --- |
| State, severity | Fixed, low |
| Player impact | None directly; a deployment whose `SIGNAL_FISH_CONFIG_PATH` (or cwd `config.json`) resolved to a broken symlink or symlink loop silently ran on lower-priority sources — the exact revert-on-provisioning-failure shape the app-registry fold is fail-closed against. |
| Source and revision | `src/config/loader.rs` `read_file_source` `Path::exists()` gate, reviewed at `a479d3a4`. |
| Invariant | `NotFound` on the source's directory entry (checked via `symlink_metadata`, not following the final symlink) is the one tolerated file outcome; every present-but-unreadable shape — dangling symlink, symlink loop, permission errors — is a hard error naming the path. |
| Confidence and reproduction | Reproduced live at `a479d3a4`: `SIGNAL_FISH_CONFIG_PATH` at a self-referential symlink → defaults, exit 0, no log; a dangling symlink behaved identically (verified during adversarial review of the first fix, which only covered the loop). |
| Disposition | The `exists()` gate is replaced by `symlink_metadata` (entry existence without following the final symlink); `NotFound → Ok(None)`, everything else the path-naming hard error (`config_file_that_fails_stat_is_a_hard_error_naming_the_path`, pinned for both the loop and the dangling-symlink shapes). |

### ARM-C041 — A deep env override key crashed the process via stack overflow

| Field | Record |
| --- | --- |
| State, severity | Fixed, medium (robustness; input requires control of the process environment or a corrupted launcher script) |
| Player impact | A hostile, corrupted, or buggy launcher environment variable (`SIGNAL_FISH__A__A__…`) aborted the whole server at startup or SIGHUP — every room's signaling dropped — with no diagnostic beyond the runtime's stack-overflow abort. |
| Source and revision | `src/config/loader.rs` `set_nested_value` recursion and the recursive `Drop` of the built `Value`; no depth cap on `__`-separated override keys, reviewed at `a479d3a4`. |
| Invariant | Recursion over external input is bounded by a named limit (the same class as the wire-decoder depth walls; cf. ARM-C030). |
| Confidence and reproduction | Reproduced live at `a479d3a4`: 40k-segment key → `fatal runtime error: stack overflow`, SIGABRT (exit 134), on the main thread at startup. |
| Disposition | Overrides beyond `MAX_ENV_OVERRIDE_DEPTH` (16; deepest canonical path is 4) fail with a named error (`env_override_deeper_than_the_segment_cap_is_a_hard_error`). The document path was already bounded by serde_json's 128-level recursion limit, now pinned for the config path (`deeply_nested_config_source_is_a_parse_error_not_a_crash`). |

### ARM-C042 — Case-variant duplicate env overrides resolved by unspecified iteration order

| Field | Record |
| --- | --- |
| State, severity | Fixed, low |
| Player impact | None directly; two case variants of one override (`SIGNAL_FISH__PORT` vs `SIGNAL_FISH__port`) silently resolved by unspecified `std::env::vars()` order behind a misleading "canonical and legacy" warning, making the effective config launcher-order-dependent. |
| Source and revision | `src/config/loader.rs` env-override `Occupied` entry arm, reviewed at `a479d3a4`. |
| Invariant | An ambiguous override is refused deterministically; the canonical-vs-legacy alias warning is factually accurate and keeps its order-independent resolution. |
| Confidence and reproduction | Reproduced live at `a479d3a4`: both variants set → legacy-conflict warning (false) and first-iteration winner. |
| Disposition | Same-class duplicates hard-error naming both variables and the knob path; cross-class alias pairs keep canonical-wins with an accurate warning including both names (`duplicate_case_variant_env_overrides_are_a_hard_error_naming_both_vars`; existing either-order alias pins re-run green). |

### ARM-C043 — The default per-IP connection budget equaled exactly one default room roster

| Field | Record |
| --- | --- |
| State, severity | Fixed, low (default coherence) |
| Player impact | A fully seated default room behind one NAT (8 players + auto-derived 16 spectators = 24 registrations, every seat through the per-IP limiter) consumed the default `security.max_connections_per_ip` exactly, so the first reconnect churn or extra tab from that NAT was refused `IpLimitExceeded`; the default's own comment promised 16-player headroom (arithmetically 48 seats) that did not exist. |
| Source and revision | `src/config/defaults.rs` `default_max_connections_per_ip` (24) vs `src/server/room_service.rs` auto spectator capacity (2× player ceiling) and the `connection_manager` per-IP registration gate, reviewed at `a479d3a4`. |
| Invariant | Compiled defaults compose: the documented NAT/LAN use case (16 players + 32 auto spectators) fits inside the default per-IP budget with reconnect churn headroom. |
| Confidence and reproduction | Static roster arithmetic over the registration path (all registrations — players, spectators, reconnects — consume a per-IP slot in `register_delivery`). |
| Disposition | Default raised 24 → 64 with a corrected comment; pinned by `default_per_ip_budget_admits_a_full_nat_roster_with_churn_slack` (data-driven over the roster formula). The parallel `ServerConfig::default()` literal in `src/server.rs` now derives from the same `default_max_connections_per_ip()` instead of a divergent hardcoded 24. Config-reference table, deployment docs, checklist, and example configs updated to the new default. |

### C1 protocol subsystem review (2026-10-05)

At `8f80cc6e` (main after #746), reviewed the protocol coverage row across
`src/protocol/**` and `src/trace_validation.rs` against the row invariant
"V2/V3 decoding, wire bytes, and delivery class match contract", with the
named missing cases (malformed/deep frames, mixed format boundaries) and
the fuzz target `fuzz/fuzz_targets/decode_protocol.rs`. No defect was
found; every hazard family carries a recorded disposition and the two
previously undocumented decode behaviors are now pinned.

- **Delivery-class decode carries explicitly (verified).** `class`/`key`
  are `Option` fields decoded through `deserialize_present_optional`:
  omission is `None`, an explicit `null` fails decode, and no present
  value is ever defaulted at decode. The `None → reliable` floor is a
  relay-layer rule exactly where the documented "omitted class means
  reliable" lives, binary frames are hardwired reliable by contract, the
  v2/v3 gate admits exactly the documented pairings, and the
  `INVALID_DELIVERY_CLASS` before size/seat ordering matches the
  documented validation order (all previously pinned).
- **Strict v3 binary envelope (verified, previously pinned).** The
  reference decoder rejects duplicate/unknown fields, trailing bytes,
  non-UUID senders, and out-of-range stamps without truncation; boundary
  stamps decode verbatim; per-frame encoding is negotiated, never
  sender-controlled; unsupported formats fail with the exact gap plus the
  throttled advisory (ARM-C032 cadence).
- **Depth and size limits are symmetric across paths (verified).** The
  JSON command path leans on serde_json's 128-level recursion limit
  (documented at the scanner constant), the MessagePack ingress and
  fallback conversion run the shared iterative 128-level scanner first
  (ARM-C030), frame and payload caps precede decode on both lanes, and
  the deep-nesting probes pin clean `Err` behavior on both formats
  (`tests/protocol_fuzz_hardening.rs`).
- **Enum and tag handling is fail-closed (verified).** Unknown class,
  encoding, transport, topology, `type`, or error tokens and explicit
  `null` metadata fail decode with `INVALID_INPUT` and the connection
  kept open; `ErrorCode` has no catch-all variant, so unknown codes are
  never silently coerced.
- **Duplicate members have pinned precedence (new pins).** The plain
  JSON lane rejects duplicate members at every typed envelope level —
  tag, content member, and content fields (`duplicate field …` decode
  error) — matching the token-bound lane's frame-wide
  pre-verification rejection; only opaque payload values collapse,
  deterministically last-wins, which is the only representable outcome
  once a never-inspected payload is decoded into a JSON value map (the
  MessagePack path and the Json-in-binary fallback decode
  `decode_binary_to_json` share the same `Value` semantics, so they
  collapse the same way). Pinned by
  `json_duplicate_members_have_pinned_precedence`.
- **Numeric literals have pinned fidelity (new pins).** Integer literals
  inside the `i64`/`u64` range relay exactly; literals outside it take
  serde_json's f64 path and relay as the nearest f64's shortest text
  (2^64 + 1 drifts to f64-exact 2^64 — the documented, bounded float
  approximation class, not a new defect; integer literals outside the
  `i64`/`u64` range are the pinned exception to the unqualified
  "relayed verbatim" payload guarantee, and clients needing exact large
  integers have the signed lane's interoperable range as the normative
  contract); literals beyond the f64 range fail
  decode cleanly instead of relaying an infinity or `null`; `-0`
  re-renders value-equal in float form. The exact-text pins make any
  serde_json number-handling change a loud wire-contract event. Pinned by
  `json_integer_literals_relay_within_the_pinned_fidelity_contract` and
  `json_out_of_range_float_literals_fail_decode_cleanly` in
  `tests/v3_wire_properties.rs`.
- **`trace_validation.rs` is a recorder, not a validator (verified).** No
  wire frame passes through it, so no decode bypass exists; its
  fail-closed divergence labeling and queue-close races are pinned
  in-module, and its two documented silent drops stay documented.

The Protocol coverage row moves to reviewed: wire bytes, delivery class,
and depth/size behavior match the contract on every decode path — with
the pinned out-of-i64/u64 integer-literal exception recorded above — and
the remaining numeric/member behaviors are pinned with recorded
dispositions.

## Coverage ledger

All rows were inventoried at `b24b5e13`; the review state of each row is
recorded in its State column below. Paths identify the review seam;
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
`trace_validation`, `server`, and `websocket`. Each row's review state is
recorded in the coverage table below.

Feature coverage also starts unreviewed. Exercise `default=[]`, `tls`,
`legacy-fullmesh`, `tls,legacy-fullmesh`, and `--all-features`. The legacy path
is for local interop and has a separate security posture. `trace-validation`
is an internal verification seam; `allocation-tracking` is a development
benchmark seam. Check those two in the relevant tests and all-feature build;
neither is a deployed capacity preset.

| Subsystem and paths | Invariant to check | Existing evidence lead | Missing cases / next check | State |
| --- | --- | --- | --- | --- |
| Startup and CLI: `src/main.rs`, `src/lib.rs` | Startup rejects bad config; startup failure leaves no listener | `tests/config_and_endpoints_tests.rs`, `tests/tls_deployment_boundaries_e2e.rs`; C1 client and deployment boundaries review above | Startup order verified: every fallible step precedes the bind and start logs follow it, so no half-started server is reachable; the failure-after-partial-startup regressions (bind conflict, invalid PEM post-spawn) are pinned over the real binary (ARM-C036 closed) | Reviewed |
| Config and reload: `src/config/**` | Defaults, validation, and reload preserve one coherent policy | `tests/config_and_endpoints_tests.rs`, `tests/config_validation_coverage_scan.rs`; C1 allowlist and key reload boundary review and C1 config and reload coverage review above | SIGHUP key/allowlist swap order and invalid reload reviewed and pinned; default coherence verified guard-by-guard against `Config::default()`; malformed-document and env-override breadth reviewed with five fixed defect classes (ARM-C038..ARM-C042) and the per-IP default-coherence fix (ARM-C043), all red-proven and pinned | Reviewed |
| Authentication: `src/auth/**`, `src/rate_limit.rs` | Unauthorized traffic cannot enter a room; limits count refusals | `tests/auth_integration_tests.rs`, `formal/tla/RateLimitWindow.tla`; C1 authentication admission boundary and rate-limit rejection accounting reviews above | Flood posture, budget-before-credential ordering, refusal closes, the absolute activity-immune auth deadline (`pre_handshake_activity_does_not_extend_the_auth_deadline`), concurrent ceiling conservation (`concurrent_handshakes_conserve_the_app_ceiling_and_count_every_rejection`), and every refusal path's exact-once charge/counter pairing (the drain-window creation refusal's deliberate budget-free shape is now pinned) are reviewed and pinned | Reviewed |
| Security: `src/security/**`, `src/websocket/token_binding.rs` | Token, origin, TLS, and TURN credential checks fail closed | `tests/mtls_token_binding_e2e.rs`, `tests/tls_deployment_boundaries_e2e.rs`, `fuzz/fuzz_targets/fuzz_reconnect_tokens.rs`; C1 token rotation boundary and client/deployment boundaries reviews above | Rotation ordering and concurrent-claim refusals are reviewed and pinned; the TLS-variant posture is verified (silent enable impossible, cert/key fail closed pre-bind, mTLS binding pinned e2e over the real binary) and the drain close path over TLS is pinned over the real binary (ARM-C035 closed); connect-token claim boundaries remain | Partially reviewed |
| Protocol: `src/protocol/**`, `src/trace_validation.rs` | V2/V3 decoding, wire bytes, and delivery class match contract | `tests/v2_wire_golden.rs`, `tests/v3_wire_properties.rs`, `fuzz/fuzz_targets/decode_protocol.rs`; C1 protocol subsystem review above | Malformed/deep frames, mixed format boundaries, delivery-class carry, enum/tag fail-closed behavior, duplicate-member precedence, and numeric-literal fidelity are reviewed and pinned (or previously pinned); the recorder-only `trace_validation.rs` has no decode seam | Reviewed |
| Room and player storage: `src/database/**` | Membership and room limits stay atomic and app isolated | `tests/integration_tests.rs`, `tests/model_based_state_machines.rs`; C1 admission-limit review above | Other adapters, rollback, and leave/disconnect races remain | Unreviewed |
| Room lifecycle and moderation: `src/server/room_service.rs`, `moderation.rs`, `spectator_service.rs`, `spectator_handlers.rs` | Join, leave, kick, ban, spectator state and ownership agree | `tests/lobby_integration_tests.rs`, `src/server/room_service_tests.rs`; C1 admission-limit, leave/disconnect ordering, and identity-slice completion reviews above | ARM-C001–C004 fixed in spectator and room-code seams; identity cases (concurrent limits, join-only, leave/disconnect, spectator transitions, kick/ban races, application isolation) reviewed and pinned or derived; storage-fault interleavings on other adapters remain | Partially reviewed |
| Readiness and gameplay: `src/server/ready_state.rs`, `authority.rs`, `session_policy.rs`, `signaling.rs` | Membership and transport changes invalidate stale plans/readiness | `tests/v3_session_plan_e2e.rs`, `formal/tla/SignalFishSession.tla`; C1 gameplay-transitions review above | Start/leave, authority loss, v2/v3 negotiation, capability intersections, stale reports, downgrade reconnects, and publication order are reviewed and pinned (including the spectator start-authorization coupling); shared `src/server.rs` state seams remain | Partially reviewed |
| Relay routing: `src/server/game_data.rs`, `message_router.rs`, `messaging.rs`, `relay_policy.rs` | Each permitted message reaches only valid peers with correct sequence/class | `tests/v3_game_data_sequencing_e2e.rs`, `tests/mixed_encoding_relay_e2e.rs`; C1 cross-room stall fairness, mixed encoding/unsupported conversion, and permitted volatile loss reviews above | Slow-recipient isolation, cross-room stall fairness, the mixed encoding/unsupported conversion matrix (direct cohorts, lossless fallback, opaque refusal with exact gap plus advisory, pre-v3 advisory-only wire), and real-socket volatile eviction with exact reports plus a non-zero per-connection `dropped_for_you` (`flooded_nonreading_recipient_observes_exact_volatile_gaps_and_dropped_for_you`) are reviewed and pinned | Reviewed |
| Coordination and queues: `src/coordination/**`, `src/distributed.rs`; the in-memory coordinator seams in `src/server.rs` | Transaction and queue failure is explicit; one room cannot strand another | `tests/relay_backpressure_e2e.rs`, `formal/tla/RoomMessageTransaction.tla`; C1 room-event duplicate-delivery, latest coalescing keys/generations, and transaction reservation/commit cancellation/panic reviews above | Lane job exactly-once and no lease re-run are dispositioned and pinned (`interleaved_awaits_deliver_each_lobby_broadcast_exactly_once`); cross-epoch gap ranges stay distinct per epoch (`cross_epoch_gaps_of_one_sender_stay_distinct_ranges`); latest key composition, generation shielding, supersession, saturation, and counter conservation are reviewed and pinned; cancellation/panic at reservation and commit are reviewed, the silent panic-accounting class is fixed, and all three fixed seams are pinned (`panicking_commit_hook_releases_and_accounts_every_reservation`, `panicking_phase_callback_accounts_remaining_frames_and_never_delivers_phase_one`, `panicking_broadcast_replay_hook_releases_and_accounts_every_reservation`); other `src/server.rs` state seams remain with their own rows | Reviewed |
| WebSocket ingress and egress: `src/websocket/**` | Bounded frames, priority control, close and drain semantics hold | `tests/transport_frame_limits_e2e.rs`, `tests/slow_consumer_no_cascade_e2e.rs`, `tests/tls_deployment_boundaries_e2e.rs`; C1 parser-boundary, error/logging pressure, and client/deployment boundaries reviews above (ARM-C030 bounded-depth MessagePack decode; ARM-C031/ARM-C032 log hardenings) | Slow reader, batching age; the drain→4000 close path over TLS is pinned over the real binary (ARM-C035 closed) | Partially reviewed |
| Reconnect and retry: `src/reconnection.rs`, `src/retry.rs`, `src/server/reconnection_service.rs` | Claims have one owner; replay and stale routes cannot leak or misroute | `tests/reconnect_window_races_e2e.rs`, `formal/tla/ReconnectionClaimLifecycle.tla`; C1 reaper-ordering, claim-expiry, failed-restore, token-rotation, reconnect epoch/sequence, and inactive-record/claim-retention reviews above | Simultaneous claim, expiry during claim, failed restore/retry, rotation boundaries, reconnect epoch/sequence transitions (including cross-epoch gap accounting), the pre-issued-token teardown discards (layered, hypothesis falsified), claim/pending-detach retention lifecycles, and the owned-task supervisor-panic strand (ARM-C037 fixed with a red-proven pin) are reviewed and pinned; `src/retry.rs` backoff seams and multi-failure detach accounting on failing backends remain | Partially reviewed |
| Maintenance and deadlines: `src/server/maintenance.rs`, `heartbeat.rs`, `dashboard_cache.rs`, `src/deadline.rs` | Expiry and cleanup are bounded; live state survives sweeps | `formal/tla/RoomLifecycleGC.tla`, `tests/clock_source_scan.rs`; C1 maintenance and deadlines expiry boundary review above | Expiry boundaries and clock sources are reviewed and pinned (reaper pair boundary, zero-timeout disable, monotonic windows, wall-clock-step immunity, overflow); churn growth and dashboard cost remain measurement work | Partially reviewed |
| Metrics and logging: `src/metrics.rs`, `src/logging.rs`, `src/websocket/metrics.rs`, `prometheus.rs` | Counters report outcomes; labels and logs stay bounded and safe | `tests/config_and_endpoints_tests.rs`, `tests/websocket_test_helpers/prometheus_scrape.rs`; C1 metrics label cardinality and error/logging pressure reviews above (ARM-C031/ARM-C032 fixed the rejected-ID log forgery and the unthrottled undeliverable-relay warning) | Cardinality is bounded with pinned lifecycles; hot-path log content, amplification, and throttle cadences are reviewed and pinned or dispositioned | Reviewed |
| Admin and shutdown: `src/server/admin.rs`, `shutdown.rs`, `connection_manager.rs` | Drain closes all owned tasks and reports queued work accurately | `tests/close_code_semantics_e2e.rs`, `formal/tla/ConnectionTeardown.tla`; C1 drain/shutdown review above | The drain choreography, the reconnect-commit fence, close ordering with queued data, and the drain reservation accounting are reviewed, fixed where defective, and pinned; a distinct 4000-close counter remains follow-up observability | Partially reviewed |
| Browser client: `clients/browser/src/**` | Reconnect, delivery reports, fallback, and negotiation match server | `clients/browser/src/page/*.test.ts`; C1 client and deployment boundaries review above | Reports, fallback, and negotiation verified and pinned (plus the ARM-C033 downgrade fix and its pin); reconnect initiation absent by documented scope with inbound arms unit-pinned; a live browser accountability cell, loss→recovery transitions, and browser-as-host remain | Partially reviewed |
| Native client: `clients/native/src/**` | Same client contract across native sockets | `clients/native/tests/interop_e2e.rs`; C1 client and deployment boundaries review above | Accountability model, fallback (cripple/TURN/host-star/Direct rejection), and negotiation verified and pinned (plus the ARM-C033 downgrade fix, the v2-opaque guard, and their pins); native downgrade and cross-format advisory consumption now have live cells; the MessagePack cohort is pinned; restore/reconnect end-to-end remains | Partially reviewed |
| Fortress clients: `clients/fortress/src/**`, `clients/fortress-wasm/src/**` | Reference peers handle relay without silent loss | `clients/fortress/tests/multiprocess.rs`, `clients/fortress-wasm/harness.mjs`, both interop workflows; C1 client and deployment boundaries review above | Silent-loss detection substantiated by executable CI gates on both stacks (contiguity, cross-peer ledger equality, queue-age/checksum gating; both families add an asserted expected-negative control); fixtures are WebSocket-only and evidence no ICE-fallback claim; Unix native drain/restart and partial-handshake close cells prove bounded causal failures; fresh healthy games pass after restart; WASM drain/restart and partial-handshake close cells check bounded causal failures and fresh healthy games on the same port; WASM inbound overflow and restore remain (#741) | Partially reviewed |

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

## Next PR contract

**C1 first slice: identity and membership ([#647](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/647)). COMPLETE**
(2026-10-01, five slices through 2026-10-04; see the review records above and
the client-row follow-ups in #741.)

**C2 runner foundation: LANDED.** The standalone delivery-aware runner lives
at `tests/capacity_runner/` (issue #648, first runner PR). It accepts the full
contract input set (endpoint, seed, room/player count, protocol/encoding,
payload bytes, sender rate, delivery class, warm-up, duration, churn/reconnect
schedule, output directory; `CAPACITY_RUNNER_*` environment variables for
standalone capacity-host use), spawns the release server binary as a separate
process or connects to an external endpoint, schedules offered traffic
independently of response completion with a generator-lag bound, measures
scheduled send to same-clock recipient receipt on one monotonic run epoch,
and writes versioned artifacts: `manifest.json` (schema, run ID, config,
binary/config overlay hashes, toolchain, host, features, clock method),
`deliveries.jsonl` (tagged sends/receipts/gap reports/disconnects/faults
since schema 2),
`intervals.jsonl` (scraped delivery counters, server RSS, server and
generator CPU time, cgroup memory,
generator RSS, with unavailable counters recorded as null or explicit
scrape errors), `summary.json` (the oracle outcome), and
`latency-histogram-v2.hdr`. A replay
of the raw events reproduces the outcome summary exactly (`artifacts::replay`).
The small reliable relay scenario passes and every registered negative
control invalidates with its explicit reason: missing, duplicate, misrouted,
and out-of-order deliveries (deterministic oracle controls), a paused
generator (lag lands in scheduled-send latency, not reduced offered load),
generator saturation (explicit reason with worst lag and unsent work), server
termination (declared fault, gap-free prefixes preserved), and a slow reader
(eviction recorded and accounted by the server's
`websocket_slow_consumer_disconnects_total` counter).

**C2 next runner PR: latest/volatile delivery classes — LANDED** (second
runner PR, 2026-10-05). `DeliveryClass` carries the full lossy-class
contract: `latest` (keyed newest-value; `latest_keys_per_sender` selects
newest-value or key isolation) and `volatile`. Every omission must arrive
as an exact server-stamped gap report (`latest_superseded`,
`latest_dropped_full`, or `volatile_dropped` — a reason the class cannot
produce is a violation), gaps may not overlap each other or a delivery,
ranges may not reach beyond the sender's relay stream or below the first
1-based sequence, and reliable runs reject any gap at all. The oracle
validates coverage per recipient (uncovered holes are `MissingDeliveries`;
only the loud disconnect tail may stay uncovered) and the summary now
carries per-recipient latency tails plus `gap_covered` totals, and records
the run class's seven accountable server outcomes in every interval sample.
The pressure control runs both classes over real sockets. Each cell offers
16-KiB application payloads at 200 sends/s with a three-second read pause,
a 3.5-second measurement, a clamped receive buffer, and a tiny server send
queue. The pause still spans about 600 offered frames per sender. The lower
instantaneous rate reduces generator load while the longer pause retains
the declared byte pressure. The generator lag bound stays at 500 ms.
This byte volume replaces the old 96-byte control's fixed frame-absorption
estimate (#783).
Each run must stay valid, cover at least 100 omissions with exact gaps, and
match the server's per-class counter. The artifacts must replay exactly.
These large payloads are declared fault-control inputs. The C3 relay cells
still use 96-byte and 1-KiB payloads. Key isolation is pinned: distinct keys
coalesce nothing. Artifacts bumped to schema 2 (gap-report event kind,
class-aware summary). The e2e cells exposed one runner defect during the
red run: a peer's join snapshot can miss the member that joined
concurrently, so receivers now track `PlayerJoined`/`PlayerLeft` to resolve
gap senders.

Live runner cells share a test lock under plain `cargo test`. The coverage
and MSRV suites can otherwise run many generators at once and exhaust a
cell's lag bound. The lock matches nextest's process-spawning isolation;
the workload and validation limits stay the same.

Each connection polls reads alongside one persistent scheduled write. Ready
reads and writes take turns, so overdue sends cannot starve receipts. Sender
pauses hold only the write future. Reader resumption, churn, and quiescence
wake independently of a pending write. Churn drops the old write and both
socket halves before rejoining. The runner cancels and awaits its owned
tasks before artifact capture. One immutable interval snapshot supplies
both the published samples and the returned final counters (#783).

Windows CI exposed a failed-write termination race (#799) in session 362.
After the declared server kill, a socket write could fail before read EOF.
The runner stopped without disconnect evidence, so the oracle classified
one cutoff-tail delivery as missing. Transport write failures now stop
outbound work and drain buffered receipts with the existing read hooks and
quiescence deadline. Only EOF, close, or read error records a disconnect.
A pending drain produces a deadline fault. Ordinary write failures keep
`SendFailed`; the declared kill keeps `ServerTerminated`. Generator stops
retain their existing outcome. A deterministic failed-write control was
red when queued receipts were discarded. The fixed control covers terminal
and deadline paths, exact disconnected prefixes, interior holes, and
serialized evidence. The live termination cell checks artifact replay.
No oracle rule, artifact schema, or workload limit changed.

The runner builds its metrics client and prepares every peer before arming.
Each peer registers its initial identity and reports readiness. Only then
does the runner publish one shared measurement epoch. Setup consumes no
send-lag budget. Scheduling delays after arming still count against the
declared bound. Deterministic controls cover delayed setup and delayed
traffic. Invalid pressure runs print recent send and sample timestamps.

[PR #789](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/789)
landed the readiness gate. [Fresh main CI](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37610558591)
on `af9616a8` passed Linux, macOS, Windows, MSRV, and coverage verification.
C3 capacity measurements remain pending.

**C2 next runner PR: churn/reconnect schedules — LANDED** (third runner
PR, 2026-10-05). `ChurnSchedule` carries the reconnect-burst storm (the C3
reconnect cell): at `start`, the seed-chosen `fraction_percent` of peers
disconnect; each rejoins at a seed-staggered instant inside `[start,
start + window)`, and every victim's post-disconnect sends shift by its
offline window (a scheduled gap is workload shape, never generator lag).
The stream identity across a storm is the runner's own incarnation index —
one join per connection, registered per `PlayerId` in a shared sender
registry that receiving tasks resolve every inbound frame and gap report
through — because the server's relay stamps are per connection and a fresh
rejoin is a new member whose `(epoch, seq)` restarts at `(1, 1)`. The
oracle validates per `(recipient, sender, incarnation)` streams: rejoin
snapshots' per-member `(id, seq tail)` stamps raise each stream's owed
floor (the loud away window), stale-epoch deliveries after a sender's
rejoin and below-tail deliveries after a recipient's rejoin are misroutes,
and a planned storm that never fires invalidates the run. Artifacts bumped
to schema 3 (receipts carry the sending incarnation and per-connection
server sequence, sends carry their incarnation, churn events record the
disconnect/rejoin cycle with the rejoin snapshot tails). One real-socket
storm cell (50% of a four-peer room, reliable) passes with replay
equality; deterministic controls pin each new permitted outcome, and the
red proof neuters the stale-epoch check (the control fails). A schedule
control pins the offline-window shift exactly (moved sends land at or
after their reconnect instant with count and spacing preserved,
non-victims untouched), and adversarial-review hardening keeps snapshot
floors authoritative over later rejoin events, requires the rejoin half
of every planned cycle, keeps the stagger window below the generator-lag
bound, and refuses run-level fault hooks in churn cells. The bot review
round hardened identity further: rejoin snapshot tails are recorded
UNRESOLVED and resolved against the registry recorded at end of run (the
per-frame registry lookups are race-free because a peer registers its id
before its first send; the per-tail lookups were not, and a wrong guess
floored the wrong incarnation), a member omitted from a rejoin snapshot
(the documented join-snapshot race under concurrent rejoins) is neither
closed nor unfloored — its away window is derived from the sends that
completed at or before the rejoin instant, a rule that errs safe —
derived floors are bookkeeping only (only snapshot tails are
server-enforced watermarks, so only they turn a redelivery into a
misroute), closed streams are finished (nothing further owed, arrivals
still misroute), and the default burst window (200 ms) sits strictly
below the default generator-lag bound (250 ms) so the default env config
runs. Churn runs require the v3 wire and exclude the socket-owning and
generator-latency hooks.

**C2 next runner PR: room-replacement churn — LANDED** (fourth runner
PR, 2026-10-05). `ChurnSchedule` carries the room-replacement shape (the
C3 churn cell): on every wave `start + k * interval` — while the wave fits
the scheduled-send span — a seed-chosen `fraction_percent` of whole ROOMS
cycles: every member disconnects at the wave instant and rejoins, seeded-
staggered inside the window, into the room's NEXT generation. The
generation is a fresh six-character alphanumeric room code
(`RunConfig::room_code_for_generation`: generation 0 keeps the decimal
`{prefix}{room:03}` code; generation g ≥ 1 is
`{prefix}{letter}{room in two base-36 digits}` — the leading lowercase
letter is a character class the decimal generation-0 suffixes never start
with, so the classes are structurally disjoint, `(letter, room)` is
injective for rooms below 36², and the alphabet caps a room at 26
generations), so the replacement creates a genuinely new room and the
now-empty old room tears down — room creation/destruction churn, not a
rejoin. Because the whole room moves together, the member roster per room
is unchanged, so the oracle's static co-room checks and
per-`(recipient, sender, incarnation)` stream machinery validate
replacement runs unchanged; the member set a seat expects never changes,
only the code. Replaced-room stream identity, fresh-room rejoin snapshots
(the first seat's snapshot is empty — the room starts fresh; a later
seat's snapshot tails the members that rejoined before it), and the
derived-floor rule for members a snapshot omits all ride the existing
controls. The multi-wave shift composes correctly: a send moves by the
offline duration of exactly the waves whose disconnect it was originally
due past (the comparison runs on the original timeline, so a send due
between two waves does not inherit the later wave's shift), pinned per
send. Coherence refusals: the window stays below the interval (waves
never overlap, so a room is whole again before the next selection), the
first wave and every wave must fit the scheduled-send span, a room's
deterministic victimization count stays within its code space (the cap
rides the built plan's per-room count, not the wave count, so the C3
wide-low-fraction shape is never refused for a bound no room reaches),
and clock overflow is a refusal, not a panic; the
lag-bound refusal now covers every churn shape's window, and the
rejoin-half requirement counts one rejoin per planned victimization (a
two-wave plan with one rejoin per member is named invalid per member).
Two real-socket cells pass with replay equality: a two-room run where
half the rooms are replaced per wave (whole rooms cycle to new
incarnations while the other rooms keep serving, no cross-room leakage)
and the burst storm cell; deterministic controls pin the replacement
plan shape (whole-room victim sets, wave instants, in-window stagger,
determinism), the composed shift, generation-code uniqueness over the
full enforced room range plus the structural class disjointness, the C3
shape admission, and the three outcome families (valid replacement,
missing rejoin half, stale-generation misroute). Red proofs: the
missing-rejoin control fails when the per-victimization count is
weakened to any-rejoin, and the plan control plus the e2e cell fail when
the replacement plan builder is neutered to an empty plan; the
stale-generation control rides the stale-epoch check the third PR
red-proved. An adversarial review round caught the first encoding draft
(`g * 1000 + room` in three base-36 digits) aliasing live generation-0
codes — base-36 values at and past 36² can be all-decimal, so
`(room 296, gen 1)` collided with room 100's initial code; the
letter-first encoding replaced it, and the uniqueness control now sweeps
every room 0..999 × generation 0..=26 instead of sampled rooms.

**C2 next runner PR: richer resource counters — LANDED** (fifth runner PR,
2026-10-05). Every interval sample now carries the resource counters the C3
capacity claims are read from, beside the delivery counters: the
ingress/egress byte pair and the queue-posture gauges.
`signal_fish_websocket_egress_bytes_total` counts the application payload
length of every successfully written frame — charged at the write leaf after
the sink accepted the frame, so an egress byte is a byte that reached the
connection; ping/pong/close carry no application payload and are not counted,
mirroring the sender-side `signal_fish_relay_bytes_total` admission counter.
`signal_fish_websocket_queue_depth` (items resident across every live
classified outbound queue) and
`signal_fish_websocket_queue_oldest_age_milliseconds` (age of the oldest
resident item against the scrape instant) are walked over live connections
at scrape time — the runner samples them exactly when the endpoint is hit,
and nothing on the write path maintains them. Unavailable counters stay
recorded-as-null; the spawned binary exposes all four, so the acceptance
scenario now pins per-sample presence, byte-counter progress, and fan-out
amplification (egress strictly above ingress for the four-peer room). Red
proofs: neutering the write-path increment failed the exact-egress socket
test (counter must equal the payload bytes the client observed); neutering
the queue walk failed the connection-manager sample test (depth, oldest-item
stamp, and scrape-instant age); renaming the rendered series failed the
gauge render test.

**C2 next runner PR: server and generator CPU-time accounting — LANDED**
(sixth runner PR, 2026-10-06). Every interval sample now carries
`server_cpu_seconds` and `generator_cpu_seconds` beside the RSS pair:
cumulative `utime + stime` from `/proc/<pid>/stat` at the kernel's fixed
`USER_HZ = 100` stub, read off-path at scrape time for the spawned server
PID and the runner process itself. This is the C2 resource-collection
remainder's CPU pair — the C3 comparison of generator cost against server
saturation needs both processes' consumed CPU, and the runner's own cost
must be distinguishable from the server's. Unavailable values stay recorded
as null (off-Linux hosts). Artifacts bumped to schema 4 (additive interval
fields; a manifest naming schema 3 or older is refused by the validator).
Red proof: neutering the sampler wiring failed the acceptance scenario's
per-sample presence pin; the fixture parser test pins the spaced-comm field
positions, and the live-sampler test pins strict advancement under real CPU
work with absent PIDs recorded as null.

**C2 next runner PR: unsupported-format contract-experiment cells — LANDED**
(seventh runner PR, 2026-10-06). The runner gains a labeled `Experiment`
input (`unsupported-format`): peer 0 of every room negotiates opaque `rkyv`
(overlay knob `protocol.enable_rkyv_game_data`, refused at config time if
absent and re-verified against the server's advertised `ProtocolInfo` list
so a silent downgrade cannot hollow the cell) and sends raw binary frames;
every other peer stays JSON. The oracle validates the cross-format refusal
family per the delivery contract instead of reliable semantics: the opaque
stream must reach NO recipient as a payload (any arrival, or any binary
frame anywhere, is the `unsupported_format_leak` class), every omission must
be covered by exactly one exact `unsupported_format` gap report (a hole
without its report is `missing_deliveries`), a foreign reason on the opaque
stream is invalid, the gap contract stays closed on the text streams, and
the rate-limited advisories are recorded evidence bounded at one per opaque
sender per second (`unsupported_notice_flood` names the exact count and
bound). Inbound classification is exact: advisories at cross-format
observers are notices, the same advisory at the opaque sender (and any other
error code) stays a rejection. The run is refused outside v3/reliable/
churn-free shapes; binary frames carry no delivery class. Summary and
manifest carry the experiment label, notices are summed in the summary, and
artifacts bump to schema 5 (new event kind `unsupported_notice`, additive
summary fields; older schemas refused by the validator). The real-socket
cell (1 room, 1 opaque + 3 JSON observers) pins contract legality, exact
coverage (`gap_covered == observers × sends`), zero opaque receipts, the
labeled replay, and exact server-counter agreement
(`class_outcome_unsupported_format == observers × sends`, now sampled for
experiment runs too). Six deterministic controls carry red proofs: the
leak rule, the opaque-stream coverage rule, the foreign-reason scope, the
text-stream scope, the notice-cadence bound, and the inbound
classification each fail their control when neutered. Remaining
unsupported-format ground (tracked for later cells, not silently skipped):
v2 observer cohorts (advisory-only, no reports to validate), same-format
opaque twins (binary-envelope decoding), and churn × experiment composition.

**C2 next runner PR: live-state, backlog, maintenance, and socket-memory
resources — LANDED** (eighth runner PR, 2026-10-07). The resource-collection
remainder is closed; every interval sample now carries the four missing
resource families beside the delivery counters and the RSS/CPU/cgroup set:

- **Live objects** (scrape-time, never write-path): `signal_fish_rooms_live`,
  `signal_fish_room_occupants_live`, `signal_fish_reconnection_pending`,
  `signal_fish_replay_rooms_retained`, and
  `signal_fish_replay_events_retained`, read under short read locks when
  the endpoint is hit (`GameDatabase::live_room_counts`, default `None` for
  embedder backends that cannot answer cheaply, and
  `ReconnectionManager::replay_sample`). An unavailable count renders no
  series — absence stays distinguishable from a fabricated zero.
- **Cleanup backlog**: `signal_fish_cleanup_pending_publications`, the
  rooms in the pending-publication lifecycle at scrape time (in-flight
  creation or awaiting repair; maintenance drains the abandoned ones).
- **Maintenance cost**: `signal_fish_maintenance_sweeps_total` (completed
  sweeps) and `signal_fish_maintenance_last_duration_milliseconds` (the
  most recent completed sweep's monotonic wall duration) — the C3
  maintenance-complexity signal. Drain-aborted passes are not counted.
- **Socket memory**: `server_socket_tcp_mem_pages` and
  `server_socket_udp_mem_pages` in every interval sample — TCP+TCP6 and
  UDP+UDP6 `mem` from `/proc/<pid>/net/sockstat`, in kernel memory pages
  (the page size is a capacity-host environment fact), `null` off-Linux.
  A `FRAG:` line's `memory` field is never swept into the totals.

Artifacts bumped to schema 6 (additive interval fields). Red proof:
neutering the sweep recording call fails
`maintenance_sweep_accounting_records_every_completed_sweep` (the counter
never advances). Presence pins: the spawned binary's run asserts all eight
new series as u64 in every interval sample on every platform, and the
socket-memory pair as recorded on Linux (null elsewhere, mirroring the
CPU-pair contract); renaming any rendered series fails the render pins.

## C3 measurement prerequisite review — 2026-10-07

In the runner shipped by
[PR #768](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/768),
the latency oracle subtracted completed socket-send time from receipt time.
This excluded generator lag and socket-send wait from the
scheduled-send SLO. Both histogram paths also clipped samples above 60 seconds.
[#774](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/774)
fixes this measurement defect. No server runtime defect is claimed.

Schema 7 uses intended send time for aggregate, per-recipient, and histogram
latency. Histogram bounds cover the observed range; the summary maximum is
exact. Replay rejects earlier schemas so historical results cannot silently
acquire corrected semantics. The deterministic control covers all three delivery
classes, warm-up exclusion, delayed sends, receipt before completed send, and
70-second stalls. The real-socket pause control requires the delay in each
recipient's latency and verifies replay equality. The deterministic test failed
on the old calculation before the fix.

Large C3 runs remain gated on
[#775](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/775)
(generator memory) and
[#776](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/776)
(effective configuration and payload-size provenance). Required encoding and
idle-lobby cohorts, constrained-host setup, and external-host resource collection
also remain. No capacity point or deployment claim is accepted by this review.

## C3 generator latency memory experiment — 2026-10-07

The first part of [#775](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/775)
removes per-delivery recipient clones and latency vectors. The oracle now feeds
aggregate and recipient HDR buckets in one pass. The artifact writer also
streams samples. Recipient buckets are allocated only when measured receipts
arrive. Schema 7, exact maximum latency, delivery verdicts, and replay stay the
same.

The registered experiment compared main after
[PR #777](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/777)
with this change. Both debug test binaries used the same synthetic complete
reliable fan-out: 16 peers, with 100, 1,000, or 5,000 sends per peer. Each cell
ran three times in a fresh process, baseline first, then candidate. Linux
`os.wait4` supplied peak child RSS in KiB. The initial GNU time method was
replaced before measurements because that tool was absent. All attempts were
retained. Full summary JSON matched the baseline for every candidate run.

| Receipts | Baseline median KiB (range) | Candidate median KiB (range) | Reduction |
| --- | --- | --- | --- |
| 24,000 | 15,364 (15,360–16,676) | 14,052 (14,048–14,052) | 8.5% |
| 240,000 | 68,692 (68,560–68,764) | 52,276 (52,020–52,336) | 23.9% |
| 1,200,000 | 312,756 (312,756–312,824) | 223,852 (223,832–223,960) | 28.4% |

The host used ARM64 Linux under WSL2 and rustc 1.91.0. These numbers measure
synthetic generator memory in the debug profile. They establish no server
capacity point. The generator had no CPU or memory constraint.

To repeat, build `cargo test --test capacity_runner --no-run` and run its
executable in a fresh process under a peak-RSS profiler:

```sh
CAPACITY_MEMORY_PROBE_SENDS=5000 <test-executable> \
  generator_memory_profile_on_complete_fanout --exact --ignored --nocapture
```

Use send counts 100, 1000, and 5000. The ignored probe prints the complete
summary, collection RSS, and oracle time. Registrations, raw outputs, resource
records, and comparisons are retained locally in
`/tmp/signal-fish-c3-generator-memory-20261007/`.

Raw event storage, sender schedules, and delivery-validation indexes still
grow with the workload. Issue #775 still requires those structures to stream
or stay bounded before the required long C3 cells can run.

## C3 spawned-server configuration provenance — 2026-10-07

The configuration part of
[#776](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/776)
now records the full typed settings for each spawned server. Schema 8 retains
compiled defaults, the harness base, the declared overlay, the final effective
configuration, and its SHA-256 over compact JSON. The manifest also hashes the
actual launched binary. The overlay hash remains a hash of declared input.

The harness launches a hard-linked binary, or a copy when linking fails, in its
isolated temporary directory. This prevents a `config.json` beside the build
output from contributing unrecorded map entries. The child environment removes
inherited `SIGNAL_FISH*` variables. The reserved port overrides any overlay
port. Legacy app-access keys retain the production loader's meaning.

Capacity runs reject file-backed app registries and connect-token public keys
before starting. Those sources change configuration after JSON merging; their
contents are not yet recorded. Use inline values for these runs. Replay checks
config hashes, layer reconstruction, port agreement, and server identity. These
checks detect inconsistent artifacts; they are not signatures.

External endpoints are labeled `unknown_external`, with no binary or effective
configuration evidence. Their delivery results can remain valid and replay
exactly, but they cannot support an accepted capacity point. External-host
evidence intake remains in #776, along with exact application payload sizing
and encoded ingress/egress frame sizes.

The real-binary regression compares the snapshot with the production
`--print-config` loader under the same isolated configuration and environment.
It pins defaults, harness overrides, legacy keys, and the reserved port.
Additional controls reject malformed overlays, conflicting aliases, file-backed
sources, altered hashes or layers, and old schema 7 artifacts. The manifest
presence check failed against the previous runner before acceptance.

## C3 exact application and encoded frame-body sizes — 2026-10-07

The payload part of
[#776](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/776)
now uses exact application byte counts. `payload_bytes` includes the compact
JSON bytes of the complete ledger document: sender, sequence, JSON keys, and
padding. Padding shrinks as sequence numbers gain digits. A run is refused
before artifacts, connections, or child processes when its target cannot hold
the largest scheduled metadata document.

Schema 9 records application bytes and actual encoded WebSocket message-body
bytes on every completed send and observed ledger receipt. Encoded bytes include
protocol envelopes, class/key fields, sender IDs, and server stamps. They exclude
WebSocket headers and masking, TCP, and TLS. The opaque experiment sends the
ledger document directly, so its ingress application and body sizes are equal.

The summary separates warm-up and measured ingress/egress. Unknown keys and
unidentified GameData remain in unmatched egress. Each population records count,
total bytes, and minimum/maximum sizes. Duplicate arrivals still contribute
bytes. Missing or forged ledger identities produce structured faults with their
observed sizes. Every observed application size must match the configured target;
invalid body sizes and total overflow invalidate the run. Overflow totals saturate
with an explicit fault so JSON serialization remains exact.

Receipt time is captured when the runner receives the frame, before decoding and
byte measurement. Scheduled-send latency still includes generator and socket
wait. Schema 9 identifies this receipt-boundary change and the byte evidence;
replay rejects earlier schemas and reconstructs the same summary from raw events.
Hashes and counters do not replace the recorded per-frame sizes.

The baseline real-socket control observed 141 application bytes in a declared
96-byte cell and failed the exact-size assertion. The corrected eight-cell
control covers 96 and 1,024 bytes for v2 reliable JSON and v3 reliable/latest/
volatile JSON. It verifies exact application sizes, encoded ingress/egress
lengths including metadata, phase totals, and replay equality. Deterministic
controls cover decimal sequence boundaries, escaped sender names, maximum ledger
sequence, an initial sequence that fits but later metadata that does not, wrong
warm-up sizes, duplicate and unknown arrivals, unidentified frames, and overflow.
Pressure and unsupported-format controls retain their delivery contracts.

External-host configuration and binary evidence intake remains in #776.
Generator memory, encoding cohorts, resource constraints, and the long capacity
cells remain separate prerequisites. No server capacity point is claimed here.

### C3 generator schedule memory — registered experiment

Issue #775 remains open. This experiment isolates schedule construction. It
compares main `735734ca` with compact schedules using the same ignored
`generator_schedule_memory_profile` probe. Each size runs in three fresh Linux
processes. Python `os.wait4` records peak child RSS in KiB. Record every attempt,
exit status, raw output, and wall time. Do not retry failed trials silently.

Use seed 1, two rooms, 16 players per room, 60 sends/s, 120 seconds of warm-up,
and measured durations of 60, 600, and 3,600 seconds. The workload uses the
runner's integer-microsecond period. Compare full schedule counts, a checksum
of every scheduled event, and exact first, phase-boundary, and final events for
every sender. Require identical output between baseline and candidate.

Then build the candidate's 999-room, 16-player shape with 120 seconds of
warm-up and 600 seconds of measurement. Sample the same event positions and
record its count and RSS. This constructs a ten-minute schedule; it does not
send traffic or prove a ten-minute live capacity cell. Event retention, oracle
indexes, replay inputs, and churn plans require separate memory controls.

The experiment ran on ARM64 Linux under WSL2 with rustc 1.91.0 in the debug
test profile. All 21 processes exited successfully. Each paired size produced
identical probe output in all six baseline/candidate runs.

| Scheduled sends | Baseline median KiB (range) | Compact median KiB (range) | Reduction |
| --- | --- | --- | --- |
| 345,632 | 16,608 (16,352–16,608) | 8,904 (8,848–8,904) | 46.4% |
| 1,382,464 | 40,916 (40,912–40,928) | 9,272 (9,068–9,480) | 77.3% |
| 7,142,688 | 175,840 (175,840–175,840) | 9,868 (9,676–10,148) | 94.4% |

The larger candidate shape contains 690,540,768 scheduled sends. Three fresh
processes had median peak RSS of 201,916 KiB (range 201,872–201,924). The probe
retains and serializes four sampled events per sender. Peak RSS includes this
artifact buffer and process overhead; it is not a count of schedule bytes.
The integer-microsecond period produces 43,202 sends per sender for this shape.
Raw outputs, resource records, and binary hashes remain under
`/tmp/signal-fish-c3-schedule-memory-20261007/`.

Each sender now stores scalar schedule inputs and its churn windows. Indexed
SplitMix access preserves the original timestamp and phase at every sequence.
Churn shifts compare against the original timestamp. Payload-size validation
and first-measured-send lookup use direct indexed access. Invalid clock and
peer inputs fail before plans are built. Impossible replacement-wave shapes
fail before wave allocation.

An independent frozen eager reference covers seeded jitter, warm-up boundaries,
partial periods, indexed access, and churn. It rejected an intentional PRNG
index mutation. Fifteen focused controls passed, including real-socket pressure,
reconnect, replacement, exact payload sizes, and artifact replay. Retained events,
oracle indexes, replay memory, and the ten-minute live cell remain open in #775.

### C3 replay input memory — registered experiment

Issue #775 remains open. This experiment isolates JSONL input buffering. Compare
main `5414c7fe` with streaming readers using the same ignored
`generator_memory_profile_on_complete_fanout` probe. Generate each input artifact
once, outside the measured reader processes, from 16 peers with 100, 1,000, and
5,000 sends each. The files contain 24,000, 240,000, and 1,200,000 receipts plus
the corresponding sent events. Record file sizes and SHA-256 hashes.

For each input, run three fresh baseline readers, then three candidate readers.
Use Python `os.wait4` to record peak child RSS in KiB. Each reader rebuilds the
same compact plans, reads the same artifact, and runs the exact oracle. Require
full summary JSON equality across all paired runs. Retain every attempt, exit
status, raw output, and wall time. Do not retry failed trials silently.

The input is synthetic reliable fan-out. No server traffic or capacity point is
measured. Streaming removes the whole-file byte buffer; retained records, oracle
indexes, and the largest JSONL line remain separate memory costs.

The experiment ran on ARM64 Linux under WSL2 with rustc 1.91.0 in the debug
test profile. All three fixture producers and all 18 measured readers exited
successfully. Every reader reproduced the producer's full summary JSON.

| Receipts | Input bytes | Baseline median KiB (range) | Streaming median KiB (range) | Reduction |
| --- | --- | --- | --- | --- |
| 24,000 | 4,270,383 | 16,712 (16,572–16,712) | 15,864 (15,736–15,864) | 5.1% |
| 240,000 | 43,421,095 | 90,056 (89,928–90,312) | 57,400 (57,344–57,408) | 36.3% |
| 1,200,000 | 220,485,095 | 419,912 (419,896–420,040) | 243,568 (243,552–243,956) | 42.0% |

Input hashes, binary hashes, producer records, raw reader output, and every
measured attempt remain under `/tmp/signal-fish-c3-replay-memory-20261007/`.
Both JSONL readers now reuse one line buffer. The parsing rules, event order,
arrival order, and schema remain unchanged. The buffer retains the capacity of
the largest line; parsed records and oracle indexes still grow with workload.

Controls cover all event kinds, empty lines, CRLF, a final line without a
newline, repeated registry entries, duplicate JSON keys, malformed events,
large Unicode lines, and late read errors. A mutation that reads the whole file
failed the malformed-prefix control because it read the protected tail.
Real-socket payload, churn, and replay controls also passed. No live ten-minute
cell or accepted server capacity point is proved by this experiment.

The same I/O review found [#793](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/793):
both JSONL writers could hide a final buffered write failure. A Linux `/dev/full`
control showed both writers returning success before the fix. Both now flush
explicitly and report errors with the artifact path. The repository sweep found
two other Rust buffered writers, both already flushing. The failure control and
real-socket artifact/interval replay control passed after the fix.

### C2 generator delay investigation — #795

The scheduled main run
[37619144139](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37619144139)
failed the macOS payload-cell test on commit `5414c7fe`. The
`V3Json / Volatile / 1024` cell recorded 417,628 microseconds of generator lag
against a 250,000-microsecond limit. The test rejected the run. Its temporary
artifacts did not survive the test failure.

The full PR run
[37621168700](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37621168700)
and the main run for commit `38b76ee8`
[37623685271](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37623685271)
passed the same test on macOS. These passes do not explain or fix the earlier
delay. [#795](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/795)
remains open.

Two independent source reviews found no confirmed cause. The task readiness
gate precedes the shared epoch. Read and write polling alternates, and lifecycle
deadlines take precedence. The nextest process group already isolates capacity
tests. On macOS, in-run CPU and RSS probes return unavailable values; manifest
subprocesses and binary hashing finish before the epoch.

The old failure message cannot distinguish the runner's rejection before a
write from the oracle's rejection after a completed send. Both report
`GeneratorSaturated`. The runner now emits structured stderr evidence at both
paths. It records sender, sequence, phase, intended time, observed time, and
bound. Completed writes also record preparation and write elapsed times. Write
elapsed time includes task scheduling; it does not prove socket backpressure.

Before artifact I/O, an invalid result emits the run identity, workload, runtime
worker count, declared fault hooks, fault and reason counts, and bounded context. It keeps the first,
last, and worst completed send for every configured peer, including peers with
no completed sends. It keeps the largest sampler gap and three recent samples.
Unavailable CPU and RSS values stay null. Fault and reason previews contain at
most 16 entries each, with total counts to make truncation explicit. Endpoint,
server config overlay, and full counter maps are excluded. Free-form error
details are omitted from diagnostic previews. Join errors keep their count,
and samples report scrape failure as a flag. The artifacts retain full errors.

The lag limit, offered schedule, invalid-run checks, payload assertions, replay
results, and schema remain unchanged. This evidence supports investigation;
it does not establish a cause.

Six data-driven controls cover both timing paths, every configured peer,
empty peers, unavailable resources, sampler gaps, preview bounds, and declared
hooks. A mutation that read only the final eight sent records failed the
all-peer control. The old binary passed the stall and replay control but emitted
no timing evidence; the new binary captured all three stalled peers.

A temporary 600-ms delay before the final socket write captured four completed
write observations. No raw runner fault was recorded; the oracle rejected the
completed-send lag against the unchanged 250-ms limit. The source was restored
byte for byte. All eight healthy payload and replay cells passed and emitted no
failure diagnostics. These controls prove capture and validation, not the
cause of the earlier macOS delay. Raw proof files remain under
`/tmp/session361-*.log` and `/tmp/session361-*-proof.json`.

### C2 inbound evidence after generator stop — #814

The runner returned from the peer task when scheduled-send lag or frame
preparation stopped outbound work. This dropped both socket halves before
quiescence. The oracle still treated that recipient as connected because no
termination was observed. This could truncate receipt evidence; it does not
establish a server delivery defect.

The peer now stops outbound work for its entire lifetime. It keeps the socket,
normal inbound handling, read pauses, and declared churn until shared
quiescence or observed termination. A rejoin keeps the stop state. Transport
write failures retain their existing drain and termination deadline.

The sweep covers lag refusal, application-data preparation, key conversion,
and frame encoding. Completed writes still record their send and size data.
The offered schedule, unsent accounting, generator invalidation, receipt
clock, artifact schema, and oracle rules remain unchanged.

A real-socket peer-task control fails before the fix at the session-lifetime
assertion. Six cases cover preparation failure and saturation with normal
quiescence, server close, and rejoin. They retain held and later receipts,
exact gap reports, and server rejections. They check original-clock receipt
times, stopped sends after rejoin, unchanged saturation lag, and event-artifact
round trips. Existing saturation and termination cells check full replay.

Test progress reads event counts without taking the evidence. The former
`snapshot` method now has the name `take_records`, which makes its ownership
transfer explicit. The final capture still transfers each record once after
all tasks finish.

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
| Potential player impact | If a stale room-A terminal unroute follows a move to room B, room B can stop receiving broadcasts. A move to the roomless lobby can lose directed responses. Top-level race reachability is not yet established. |
| Source and revision | `src/server.rs::unroute_local_client_with_tail` and `src/server/connection_manager.rs::clear_room_assignment_with_tail`, reviewed at `b7e11dea`. |
| Invariant | Refusal to clear a foreign room assignment must preserve the current room route or roomless delivery handle. The old coordinator swept every room route and removed the handle even when the assignment callback returned no terminal tail. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(stale_terminal_unroute_preserves_new_room_route)'` failed before the fix with `left: Some([]), right: Some([player_id])` after room A to B. The same test covers A to roomless after the fix. |
| Disposition | A terminal unroute with no tail now removes only the named room route and leaves the direct delivery handle until explicit unregister. The focused routing tests pass (#647). [#685](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/685) tracks proof or exclusion of the top-level race. |

### ARM-C021 — Removed socket leave departs a restored player

| Field | Finding |
| --- | --- |
| State, severity | Fixed at the server operation seam, high |
| Player impact | An old leave can remove a player's restored room seat and send a second departure event after reconnect. |
| Source and revision | `src/server/room_service.rs::leave_room_owned`, `src/server/message_router.rs`, and `src/websocket/connection.rs`, reviewed at `5b79ac98`. |
| Invariant | An old socket's leave must not act on a lifecycle later installed under the same player ID. The old missing-lifecycle fallback could reach the restored seat. A frame already in the receive task can also resume after its send task unregisters and capture the new lifecycle by ID. |
| Confidence and reproduction | `cargo nextest run --lib -E 'test(leave_from_removed_socket_cannot_depart_reconnected_lifecycle)'` failed before the fix: the old leave paused after observing no lifecycle, reconnect restored the seat, and the leave removed it (`left: None`, `right: Some(room_id)`). `leave_from_old_socket_cannot_use_replacement_lifecycle` checks the stale physical socket Arc through the owned transaction and both router leave forms. These are server tests; the WebSocket ordering follows from its independent send and receive tasks and the router's await before dispatch. |
| Disposition | The missing-lifecycle branch returns. WebSocket dispatch now carries its socket lifecycle to the router and the owned leave, which locks that exact Arc and verifies it still owns the player ID. The green tests check assignment, durable membership, routing, departure count, peer and player messages, and a valid leave from the new socket. [#685](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/685) still tracks the room-A to room-B order from ARM-C020. [#686](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/686) tracks other old-socket operations. |

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

These findings cover room-code rotation, player names, transport status, and spectator,
reconnect, room-creation drain, and terminal routing seams. The rest of the C1 room and storage
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
| Room and player storage: `src/database/**` | Membership and room limits stay atomic and app isolated | `tests/integration_tests.rs`, `tests/model_based_state_machines.rs`; C1 admission-limit review above | Other adapters, rollback, and leave/disconnect races remain | Unreviewed |
| Room lifecycle and moderation: `src/server/room_service.rs`, `moderation.rs`, `spectator_service.rs`, `spectator_handlers.rs` | Join, leave, kick, ban, spectator state and ownership agree | `tests/lobby_integration_tests.rs`, `src/server/room_service_tests.rs`; C1 admission-limit and leave/disconnect ordering reviews above | ARM-C001–C004 fixed in spectator and room-code seams; other join-only paths, leave/disconnect interleavings, kick/ban, and authority races remain | Unreviewed |
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

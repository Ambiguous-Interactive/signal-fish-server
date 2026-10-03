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
| Disposition | An iterative depth scanner (`msgpack_depth_within`, `MSGPACK_MAX_NESTING_DEPTH = 128`) walks the structure with an explicit stack before any recursive decode and refuses deeper trees as an undeliverable conversion, reusing the existing exact report and advisory accounting. The same scan guards the token-bound binary envelope (`parse_binary_message`). Malformed input stays a decoder error; the scanner only answers depth. Differential review: a spec-faithful reference parser agreed with the scanner on 200k random well-formed payloads at limits {1, 2, 3, 4, 8, 128}, exhaustive 1- and 2-byte marker spaces, and all truncations; 100k mutation fuzz found no scanner-refused-but-decoder-accepted case. Pins: `depth_scanner_matches_the_limit_boundary`, `depth_scanner_counts_map_entries_as_two_slots`, `message_pack_depth_limit_is_exact`, `depth_scanner_accepts_shallow_wires_and_skips_payload_bytes`, `depth_scanner_is_conservative_on_malformed_input`. |

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
| Config and reload: `src/config/**` | Defaults, validation, and reload preserve one coherent policy | `tests/config_and_endpoints_tests.rs`, `tests/config_validation_coverage_scan.rs`; C1 allowlist and key reload boundary review above | SIGHUP key/allowlist swap order and invalid reload are reviewed and pinned; default coherence and validation breadth (malformed documents, env-override interactions) remain | Unreviewed |
| Authentication: `src/auth/**`, `src/rate_limit.rs` | Unauthorized traffic cannot enter a room; limits count refusals | `tests/auth_integration_tests.rs`, `formal/tla/RateLimitWindow.tla`; C1 authentication admission boundary review above | Flood posture, budget-before-credential ordering, refusal closes, the absolute activity-immune auth deadline (`pre_handshake_activity_does_not_extend_the_auth_deadline`), and concurrent ceiling conservation (`concurrent_handshakes_conserve_the_app_ceiling_and_count_every_rejection`) are reviewed and pinned; room-side budget charge paths verified against their earlier pins | Reviewed |
| Security: `src/security/**`, `src/websocket/token_binding.rs` | Token, origin, TLS, and TURN credential checks fail closed | `tests/mtls_token_binding_e2e.rs`, `fuzz/fuzz_targets/fuzz_reconnect_tokens.rs`; C1 token rotation boundary review above | Rotation ordering and concurrent-claim refusals are reviewed and pinned; TLS variants and connect-token claim boundaries remain | Unreviewed |
| Protocol: `src/protocol/**`, `src/trace_validation.rs` | V2/V3 decoding, wire bytes, and delivery class match contract | `tests/v2_wire_golden.rs`, `tests/v3_wire_properties.rs`, `fuzz/fuzz_targets/decode_protocol.rs` | Malformed/deep frames, mixed format boundaries | Unreviewed |
| Room and player storage: `src/database/**` | Membership and room limits stay atomic and app isolated | `tests/integration_tests.rs`, `tests/model_based_state_machines.rs`; C1 admission-limit review above | Other adapters, rollback, and leave/disconnect races remain | Unreviewed |
| Room lifecycle and moderation: `src/server/room_service.rs`, `moderation.rs`, `spectator_service.rs`, `spectator_handlers.rs` | Join, leave, kick, ban, spectator state and ownership agree | `tests/lobby_integration_tests.rs`, `src/server/room_service_tests.rs`; C1 admission-limit, leave/disconnect ordering, and identity-slice completion reviews above | ARM-C001–C004 fixed in spectator and room-code seams; identity cases (concurrent limits, join-only, leave/disconnect, spectator transitions, kick/ban races, application isolation) reviewed and pinned or derived; storage-fault interleavings on other adapters remain | Partially reviewed |
| Readiness and gameplay: `src/server/ready_state.rs`, `authority.rs`, `session_policy.rs`, `signaling.rs` | Membership and transport changes invalidate stale plans/readiness | `tests/v3_session_plan_e2e.rs`, `formal/tla/SignalFishSession.tla`; C1 gameplay-transitions review above | Start/leave, authority loss, v2/v3 negotiation, capability intersections, stale reports, downgrade reconnects, and publication order are reviewed and pinned (including the spectator start-authorization coupling); shared `src/server.rs` state seams remain | Partially reviewed |
| Relay routing: `src/server/game_data.rs`, `message_router.rs`, `messaging.rs`, `relay_policy.rs` | Each permitted message reaches only valid peers with correct sequence/class | `tests/v3_game_data_sequencing_e2e.rs`, `tests/mixed_encoding_relay_e2e.rs`; C1 cross-room stall fairness, mixed encoding/unsupported conversion, and permitted volatile loss reviews above | Slow-recipient isolation, cross-room stall fairness, the mixed encoding/unsupported conversion matrix (direct cohorts, lossless fallback, opaque refusal with exact gap plus advisory, pre-v3 advisory-only wire), and real-socket volatile eviction with exact reports plus a non-zero per-connection `dropped_for_you` (`flooded_nonreading_recipient_observes_exact_volatile_gaps_and_dropped_for_you`) are reviewed and pinned | Reviewed |
| Coordination and queues: `src/coordination/**`, `src/distributed.rs`; the in-memory coordinator seams in `src/server.rs` | Transaction and queue failure is explicit; one room cannot strand another | `tests/relay_backpressure_e2e.rs`, `formal/tla/RoomMessageTransaction.tla`; C1 room-event duplicate-delivery, latest coalescing keys/generations, and transaction reservation/commit cancellation/panic reviews above | Lane job exactly-once and no lease re-run are dispositioned and pinned (`interleaved_awaits_deliver_each_lobby_broadcast_exactly_once`); cross-epoch gap ranges stay distinct per epoch (`cross_epoch_gaps_of_one_sender_stay_distinct_ranges`); latest key composition, generation shielding, supersession, saturation, and counter conservation are reviewed and pinned; cancellation/panic at reservation and commit are reviewed, the silent panic-accounting class is fixed, and all three fixed seams are pinned (`panicking_commit_hook_releases_and_accounts_every_reservation`, `panicking_phase_callback_accounts_remaining_frames_and_never_delivers_phase_one`, `panicking_broadcast_replay_hook_releases_and_accounts_every_reservation`); other `src/server.rs` state seams remain with their own rows | Reviewed |
| WebSocket ingress and egress: `src/websocket/**` | Bounded frames, priority control, close and drain semantics hold | `tests/transport_frame_limits_e2e.rs`, `tests/slow_consumer_no_cascade_e2e.rs`; C1 parser-boundary review above (ARM-C030 fixed: bounded-depth MessagePack conversion and token-bound envelope decode) | Slow reader, batching age, TLS close paths | Partially reviewed |
| Reconnect and retry: `src/reconnection.rs`, `src/retry.rs`, `src/server/reconnection_service.rs` | Claims have one owner; replay and stale routes cannot leak or misroute | `tests/reconnect_window_races_e2e.rs`, `formal/tla/ReconnectionClaimLifecycle.tla`; C1 reaper-ordering, claim-expiry, failed-restore, token-rotation, and reconnect epoch/sequence reviews above | Simultaneous claim, expiry during claim, failed restore/retry, rotation boundaries, and reconnect epoch/sequence transitions (including cross-epoch gap accounting) are reviewed and pinned; `src/retry.rs` backoff seams and multi-failure detach accounting on failing backends remain | Partially reviewed |
| Maintenance and deadlines: `src/server/maintenance.rs`, `heartbeat.rs`, `dashboard_cache.rs`, `src/deadline.rs` | Expiry and cleanup are bounded; live state survives sweeps | `formal/tla/RoomLifecycleGC.tla`, `tests/clock_source_scan.rs`; C1 maintenance and deadlines expiry boundary review above | Expiry boundaries and clock sources are reviewed and pinned (reaper pair boundary, zero-timeout disable, monotonic windows, wall-clock-step immunity, overflow); churn growth and dashboard cost remain measurement work | Partially reviewed |
| Metrics and logging: `src/metrics.rs`, `src/logging.rs`, `src/websocket/metrics.rs`, `prometheus.rs` | Counters report outcomes; labels and logs stay bounded and safe | `tests/config_and_endpoints_tests.rs`, `tests/websocket_test_helpers/prometheus_scrape.rs` | Cardinality and logging pressure under floods | Unreviewed |
| Admin and shutdown: `src/server/admin.rs`, `shutdown.rs`, `connection_manager.rs` | Drain closes all owned tasks and reports queued work accurately | `tests/close_code_semantics_e2e.rs`, `formal/tla/ConnectionTeardown.tla`; C1 drain/shutdown review above | The drain choreography, the reconnect-commit fence, close ordering with queued data, and the drain reservation accounting are reviewed, fixed where defective, and pinned; a distinct 4000-close counter remains follow-up observability | Partially reviewed |
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

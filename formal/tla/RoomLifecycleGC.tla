------------------------------ MODULE RoomLifecycleGC ------------------------------
(***************************************************************************)
(* The room garbage-collection contract (BUG-1). Models the                *)
(* maintenance sweep (`src/server/maintenance.rs cleanup_task`) against the *)
(* room store (`src/database/mod.rs cleanup_empty_rooms /                   *)
(* cleanup_expired_rooms`, `src/protocol/room_state.rs Room::is_expired`)   *)
(* and the reconnection manager (`src/reconnection.rs`).                    *)
(*                                                                          *)
(* THE CONTRACT the fix must hold:                                          *)
(*   (1) ActiveRoomNeverReaped — a room whose members are genuinely active  *)
(*       (activity within `inactive_room_timeout`) is never deleted by GC.  *)
(*   (2) ReconnectWindowRespected — a room holding an UNEXPIRED reconnection *)
(*       record is never deleted (else a still-valid reconnection token     *)
(*       fails `RoomNotFound`).                                             *)
(*                                                                          *)
(* THE BUG (pre-fix, reproduced by StaleActivityBug = TRUE): `last_activity`*)
(* is written only at room creation and never refreshed (both refreshers    *)
(* had zero call sites), and GC has no reconnection guard. So a room with    *)
(* actively-communicating players is reaped `inactive_room_timeout` after    *)
(* CREATION mid-game (violates (1)), and a room that emptied via disconnect  *)
(* is reaped off its stale creation time before its reconnection window      *)
(* elapses (violates (2)).                                                  *)
(*                                                                          *)
(* THE FIX (StaleActivityBug = FALSE): every activity (join, leave,          *)
(* disconnect, ping/relay) refreshes `last_activity`; the empty-room clock   *)
(* keys off `last_activity` (not `created_at`); and GC skips any room with   *)
(* an unexpired reconnection record (the `protected` set).                  *)
(*                                                                          *)
(* Mapping to the implementation:                                           *)
(*   Tick                discrete time advancing (the source of staleness); *)
(*                       `chrono::Utc::now()` moving forward between sweeps  *)
(*   Activity            a ping / relayed game-data refresh                  *)
(*                       (`maybe_update_last_seen` -> `update_room_activity`)*)
(*   JoinPlayer          `add_player_to_room` (refreshes last_activity)      *)
(*   LeaveVoluntary      `remove_player_from_room` on a voluntary leave      *)
(*                       (refreshes last_activity; no reconnection record)   *)
(*   Disconnect          `unregister_client` -> `remove_player_from_room`    *)
(*                       PLUS `register_disconnection` (leaves a record)     *)
(*   Reconnect           a successful claim inside the window                *)
(*   RecordExpireSweep   `ReconnectionManager::cleanup_expired`              *)
(*   GC                  `cleanup_empty_rooms` / `cleanup_expired_rooms`     *)
(*                       consulting the `protected` set                      *)
(*                                                                          *)
(* `clock` is the GC-visible `last_activity`; `trueActivity` is the ground   *)
(* truth of when the room was last active. The fix keeps them equal (every   *)
(* activity refreshes clock); the bug pins clock at creation (0) while       *)
(* trueActivity still moves — that gap is exactly what makes GC reap an      *)
(* active room.                                                             *)
(*                                                                          *)
(* NON-VACUITY (verified during development, keep reproducible): setting     *)
(* StaleActivityBug = TRUE makes clock never refresh and GC ignore the       *)
(* reconnection guard. TLC then reports BOTH ActiveRoomNeverReaped and       *)
(* ReconnectWindowRespected violated (traces: (1) create -> Tick past        *)
(* InactiveTimeout while Activity keeps trueActivity fresh -> GC reaps a     *)
(* room whose clock is stuck at 0; (2) Disconnect leaving a fresh record ->  *)
(* Tick past EmptyTimeout -> GC reaps the empty room while the record is     *)
(* still valid). The checked configurations pin StaleActivityBug = FALSE.    *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MaxTime,          \* time horizon (keep tiny: 6)
    InactiveTimeout,  \* room-with-players staleness bound (inactive_room_timeout)
    EmptyTimeout,     \* empty-room staleness bound (empty_room_timeout)
    ReconnectWindow,  \* reconnection record validity (reconnection_window)
    StaleActivityBug  \* TRUE: last_activity never refreshed AND no reconnection
                      \* guard (the pre-fix code). Must violate both named
                      \* invariants; FALSE in checked configs.

ASSUME /\ MaxTime \in Nat \ {0}
       /\ InactiveTimeout \in Nat
       /\ EmptyTimeout \in Nat
       /\ ReconnectWindow \in Nat
       /\ StaleActivityBug \in BOOLEAN

VARIABLES
    time,          \* current tick, 0..MaxTime
    exists,        \* room still in the store
    present,       \* a player currently occupies the room
    clock,         \* GC-visible last_activity (created_at is 0)
    trueActivity,  \* ground-truth last activity time
    hasRecord,     \* an (unclaimed) reconnection record exists for the room
    recordAt,      \* tick the reconnection record was created
    reapedActive,  \* violation flag: GC reaped a genuinely-active room
    reapedInWindow \* violation flag: GC reaped a room inside a valid window

vars == <<time, exists, present, clock, trueActivity, hasRecord, recordAt,
          reapedActive, reapedInWindow>>

(* The last_activity refresh: the fix writes `time`; the bug leaves it. *)
Refresh == IF StaleActivityBug THEN clock ELSE time

Init ==
    /\ time = 0
    /\ exists = TRUE
    /\ present = TRUE       \* a room is created with its creator present
    /\ clock = 0            \* created_at == last_activity == 0
    /\ trueActivity = 0
    /\ hasRecord = FALSE
    /\ recordAt = 0
    /\ reapedActive = FALSE
    /\ reapedInWindow = FALSE

(* Time advances between sweeps — the sole source of staleness. *)
Tick ==
    /\ time < MaxTime
    /\ time' = time + 1
    /\ UNCHANGED <<exists, present, clock, trueActivity, hasRecord, recordAt,
                   reapedActive, reapedInWindow>>

(* A ping / relayed game-data frame from a present member. Refreshes the *)
(* ground-truth activity always, and the GC clock unless the bug is set.  *)
Activity ==
    /\ exists
    /\ present
    /\ trueActivity' = time
    /\ clock' = Refresh
    /\ UNCHANGED <<time, exists, present, hasRecord, recordAt, reapedActive,
                   reapedInWindow>>

(* add_player_to_room: a join is activity and refreshes last_activity. *)
JoinPlayer ==
    /\ exists
    /\ ~present
    /\ present' = TRUE
    /\ trueActivity' = time
    /\ clock' = Refresh
    /\ UNCHANGED <<time, exists, hasRecord, recordAt, reapedActive,
                   reapedInWindow>>

(* remove_player_from_room on a voluntary leave: refreshes last_activity *)
(* (starts the empty-room clock at the departure), no reconnection record. *)
LeaveVoluntary ==
    /\ exists
    /\ present
    /\ present' = FALSE
    /\ trueActivity' = time
    /\ clock' = Refresh
    /\ UNCHANGED <<time, exists, hasRecord, recordAt, reapedActive,
                   reapedInWindow>>

(* unregister_client: a disconnect removes the player AND leaves a pending *)
(* reconnection record. Also refreshes last_activity (a departure). *)
Disconnect ==
    /\ exists
    /\ present
    /\ present' = FALSE
    /\ hasRecord' = TRUE
    /\ recordAt' = time
    /\ trueActivity' = time
    /\ clock' = Refresh
    /\ UNCHANGED <<time, exists, reapedActive, reapedInWindow>>

(* A successful reconnection claim inside the window: the player returns *)
(* and the record is consumed. *)
Reconnect ==
    /\ exists
    /\ hasRecord
    /\ time - recordAt <= ReconnectWindow
    /\ present' = TRUE
    /\ hasRecord' = FALSE
    /\ trueActivity' = time
    /\ clock' = Refresh
    /\ UNCHANGED <<time, exists, recordAt, reapedActive, reapedInWindow>>

(* ReconnectionManager::cleanup_expired drops a record whose window elapsed, *)
(* so it stops protecting the room. *)
RecordExpireSweep ==
    /\ hasRecord
    /\ time - recordAt > ReconnectWindow
    /\ hasRecord' = FALSE
    /\ UNCHANGED <<time, exists, present, clock, trueActivity, recordAt,
                   reapedActive, reapedInWindow>>

(* A room is expired per Room::is_expired: empty rooms off the empty-room *)
(* clock (created_at=0 under the bug, last_activity under the fix), rooms *)
(* with players off last_activity. *)
EmptyClock  == IF StaleActivityBug THEN 0 ELSE clock
EmptyExpired  == ~present /\ (time - EmptyClock > EmptyTimeout)
ActiveExpired ==  present /\ (time - clock      > InactiveTimeout)
IsExpired == EmptyExpired \/ ActiveExpired

(* The reconnection guard (the `protected` set): only present under the fix. *)
ProtectedByReconnect ==
    /\ ~StaleActivityBug
    /\ hasRecord
    /\ time - recordAt <= ReconnectWindow

(* The maintenance sweep reaps an expired, unprotected room. The two flags *)
(* record whether that reap was WRONG: against a genuinely-active room, or *)
(* against a room still inside a valid reconnection window. *)
GC ==
    /\ exists
    /\ IsExpired
    /\ ~ProtectedByReconnect
    /\ exists' = FALSE
    /\ reapedActive' =
         (reapedActive \/ (present /\ (time - trueActivity <= InactiveTimeout)))
    /\ reapedInWindow' =
         (reapedInWindow \/ (hasRecord /\ (time - recordAt <= ReconnectWindow)))
    /\ UNCHANGED <<time, present, clock, trueActivity, hasRecord, recordAt>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful *)
(* (matches the ConnectionTeardown / SignalFishSession convention). Enabled *)
(* only at the time horizon, where Tick is disabled. *)
Done ==
    /\ time = MaxTime
    /\ UNCHANGED vars

Next ==
    \/ Tick
    \/ Activity
    \/ JoinPlayer
    \/ LeaveVoluntary
    \/ Disconnect
    \/ Reconnect
    \/ RecordExpireSweep
    \/ GC
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ time \in 0..MaxTime
    /\ exists \in BOOLEAN
    /\ present \in BOOLEAN
    /\ clock \in 0..MaxTime
    /\ trueActivity \in 0..MaxTime
    /\ hasRecord \in BOOLEAN
    /\ recordAt \in 0..MaxTime
    /\ reapedActive \in BOOLEAN
    /\ reapedInWindow \in BOOLEAN

(* (1) A room whose members are genuinely active (activity within the *)
(* inactive-room timeout) is never reaped. The bug sets this by reaping a *)
(* room whose GC clock is stuck at creation while its true activity is fresh. *)
ActiveRoomNeverReaped == ~reapedActive

(* (2) A room holding an unexpired reconnection record is never reaped. The *)
(* bug sets this by reaping an emptied room off its stale creation time *)
(* before the reconnection window elapses. *)
ReconnectWindowRespected == ~reapedInWindow

(* Sanity: the GC clock never runs ahead of ground-truth activity, and under *)
(* the fix they coincide (every activity refreshes both). *)
ClockNeverAheadOfTruth == clock <= trueActivity

=============================================================================

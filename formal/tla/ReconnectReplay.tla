------------------------------ MODULE ReconnectReplay ------------------------------
(***************************************************************************)
(* The reconnection-replay truncation contract                             *)
(* (`reconnection::EventBuffer` + `ReconnectionManager::get_missed_events`,*)
(* src/reconnection.rs):                                                   *)
(*                                                                         *)
(*   A reconnecting player's replay status is HONEST. `truncated` is       *)
(*   reported exactly when the room's bounded replay ring evicted an event *)
(*   the player needed (an event with a sequence above the player's        *)
(*   `last_sequence` snapshot) — never because of a mere sequence GAP.     *)
(*   Sequence numbers are GLOBAL across rooms (one `next_sequence` counter *)
(*   shared by every room's ring), so another room's events create benign  *)
(*   gaps in this room's buffered sequences; only the ring's explicit      *)
(*   eviction watermark may prove a replay incomplete.                     *)
(*                                                                         *)
(* Two rooms share the one global counter. Room A holds the single pending *)
(* disconnected player (its `last_sequence` snapshots the counter at       *)
(* disconnect registration); room B holds some other pending player, so    *)
(* its events are recorded too and consume global sequence numbers         *)
(* interleaved with room A's. Room B's ring contents are irrelevant to     *)
(* room A's status and are not stored — only its counter consumption is.   *)
(*                                                                         *)
(* Mapping to the implementation (src/reconnection.rs):                    *)
(*   Disconnect     register_disconnection: last_sequence := *next_sequence*)
(*                  and room A's (empty) ring is created — ring existence  *)
(*                  IS the "someone is pending" gate                       *)
(*   EventA         record_room_event on room A: assign the next global    *)
(*                  sequence, push into the ring, evict the oldest past    *)
(*                  capacity and raise `evicted_watermark`                 *)
(*   EventB         record_room_event on room B: consumes a global         *)
(*                  sequence number only (room B also has a pending player,*)
(*                  so its ring exists and records)                        *)
(*   Reconnect      get_missed_events(room_A, last_sequence): events with  *)
(*                  seq > last_sequence, truncated iff                     *)
(*                  evicted_watermark > last_sequence                      *)
(*                                                                         *)
(* Deliberately NOT modeled (documented, all state-free):                  *)
(*   - Rooms with nobody pending: `record_room_event` returns before       *)
(*     `push_event`, so such an event neither consumes a sequence number   *)
(*     nor changes any state — a pure no-op that adds no behaviors. Room   *)
(*     A's pre-disconnect events are exactly this no-op (only room B       *)
(*     advances the counter while A's player is still connected).          *)
(*   - event_buffer_size = 0 (`ReplayStatus::Unavailable`): with the ring  *)
(*     disabled no buffer is ever created and `register_disconnection`     *)
(*     skips the gate, so `unavailable` is a configuration constant, not a *)
(*     reachable-state property — there is nothing to model-check.         *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible): setting   *)
(* the `NaiveGapPredicateBug` constant to TRUE computes truncation from    *)
(* the naive sequence-gap predicate `oldest_retained_seq > last_sequence   *)
(* + 1` instead of the eviction watermark. TLC then reports a              *)
(* `StatusHonest` violation within a handful of steps; the minimal trace   *)
(* (verified manually) is 4 actions:                                       *)
(*                                                                         *)
(*   Disconnect (lastSeq = 0) -> EventB (global seq 1 goes to room B)      *)
(*   -> EventA (room A records seq 2; nothing evicted) -> Reconnect:       *)
(*   the naive predicate sees oldest retained = 2 > lastSeq + 1 = 1 and    *)
(*   reports "truncated", yet evictedA = {} — a FALSE truncation created   *)
(*   purely by room B's interleaved global sequence number. (The mirror    *)
(*   direction — naive "complete" despite a needed eviction — is           *)
(*   structurally unreachable: an evicted seq e > lastSeq forces the       *)
(*   oldest retained seq above e, hence above lastSeq + 1.) This proves    *)
(*   the watermark design NECESSARY: no gap-based predicate can be honest  *)
(*   under a global sequence counter. The checked configuration pins       *)
(*   `NaiveGapPredicateBug = FALSE`; flip it locally to watch the          *)
(*   invariant catch the bug.                                              *)
(*                                                                         *)
(* SINGLE-INSTANCE THEOREM (ARCH-10). StatusHonest rests on the WHOLE      *)
(* premise of this module: ONE global `next_sequence` counter and ONE      *)
(* replay ring per room. Behind a load balancer that lets a room span two  *)
(* instances (join-with-unknown-code CREATES a fresh room — the `Ok(None)` *)
(* arm of `join_room_with_coordination`, `src/server/room_service.rs:519`), *)
(* each instance has its OWN                                                *)
(* next_sequence counter and its OWN ring, and a reconnection token is     *)
(* stranded on the instance that minted it. The `SplitBrainCounterBug`     *)
(* constant models exactly this: when TRUE, the player's token and its     *)
(* buffered/evicted stream stay on instance 1, but the reconnect is served *)
(* by instance 2 — which join-created room A fresh under a SECOND           *)
(* next_sequence counter and therefore has an empty ring and a zero        *)
(* watermark. Instance 2 can only ever answer "complete" with an empty     *)
(* replay, which is a LIE the moment instance 1 retained OR evicted an     *)
(* event the player needed. The split brain breaks BOTH honesty            *)
(* invariants; TLC reports the shallowest one, `ReplayFaithful`, in a       *)
(* 3-action trace (verified during development):                           *)
(*                                                                         *)
(*   Disconnect (lastSeq = 0) -> EventA (seq 1, retained) -> Reconnect      *)
(*   served by instance 2: replaySet = {} yet event 1 (> lastSeq, never    *)
(*   evicted) was needed — the empty replay silently dropped it.           *)
(*                                                                         *)
(* `StatusHonest` is violated too, at the 5-action eviction trace: after   *)
(* Disconnect -> EventA (seq 1) -> EventA (seq 2) -> EventA (seq 3, evicts  *)
(* seq 1: watermarkA = 1), the instance-2 Reconnect reports "complete"      *)
(* over evictedA = {1} with 1 > lastSeq — `complete` while a needed event  *)
(* was evicted. (Flip only this constant, not NaiveGapPredicateBug, to see  *)
(* it: the two bugs are independent and both pinned FALSE when checked.)    *)
(*                                                                         *)
(* This is executable documentation that replay honesty is a single-node   *)
(* CP guarantee requiring LB room-affinity (reconnects must land on the    *)
(* instance that owns the room). See D6 in PLAN.md and the                 *)
(* "single-instance theorems" section of formal/README.md. The checked     *)
(* configuration pins `SplitBrainCounterBug = FALSE`.                      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    EVENT_BUDGET,        \* total recorded events across both rooms (keep tiny: 5)
    RING_CAPACITY,       \* room A's replay ring slots (keep tiny: 2)
    NaiveGapPredicateBug,\* TRUE decides truncation from the sequence-gap
                         \* predicate instead of the eviction watermark (must
                         \* violate StatusHonest; FALSE in checked configs)
    SplitBrainCounterBug \* TRUE serves the reconnect from a SECOND instance
                         \* whose independent next_sequence join-created room A
                         \* fresh (empty ring, zero watermark) — a room-spanning
                         \* split brain; must violate StatusHonest; FALSE in
                         \* checked configs

ASSUME /\ EVENT_BUDGET \in Nat \ {0}
       /\ RING_CAPACITY \in Nat \ {0}
       /\ NaiveGapPredicateBug \in BOOLEAN
       /\ SplitBrainCounterBug \in BOOLEAN

VARIABLES
    nextSeq,    \* the ONE global sequence counter shared by both rooms
    ringA,      \* room A's ring: ascending sequence numbers still retained
    watermarkA, \* highest room-A sequence ever evicted (0 = None)
    recordedA,  \* ghost: every room-A sequence ever recorded
    evictedA,   \* ghost: every room-A sequence ever evicted
    phase,      \* room A's player: "Connected" -> "Pending" -> "Reconnected"
    lastSeq,    \* the player's last_sequence snapshot (taken at Disconnect)
    status,     \* "none" until reconnect, then "complete" | "truncated"
    replaySet   \* sequence numbers of the events returned by the replay

vars == <<nextSeq, ringA, watermarkA, recordedA, evictedA, phase, lastSeq,
          status, replaySet>>

(* Sequence numbers a TLA+ sequence currently holds, as a set. *)
Range(s) == {s[i] : i \in DOMAIN s}

Init ==
    /\ nextSeq = 0
    /\ ringA = <<>>
    /\ watermarkA = 0
    /\ recordedA = {}
    /\ evictedA = {}
    /\ phase = "Connected"
    /\ lastSeq = 0
    /\ status = "none"
    /\ replaySet = {}

(* register_disconnection: snapshot the GLOBAL counter as last_sequence and *)
(* open room A's replay gate. Any room-B events that already ran consumed   *)
(* sequence numbers the player DID see (lastSeq covers them).              *)
Disconnect ==
    /\ phase = "Connected"
    /\ phase' = "Pending"
    /\ lastSeq' = nextSeq
    /\ UNCHANGED <<nextSeq, ringA, watermarkA, recordedA, evictedA, status,
                   replaySet>>

(* record_room_event on room A while its player is pending: assign the next *)
(* global sequence number, push into the ring, evict the oldest entry past  *)
(* capacity and raise the watermark (EventBuffer::push).                    *)
EventA ==
    /\ phase = "Pending"
    /\ nextSeq < EVENT_BUDGET
    /\ LET seq == nextSeq + 1
           appended == Append(ringA, seq)
       IN /\ nextSeq' = seq
          /\ recordedA' = recordedA \cup {seq}
          /\ IF Len(appended) > RING_CAPACITY
                 THEN /\ ringA' = Tail(appended)
                      /\ watermarkA' = IF Head(appended) > watermarkA
                                           THEN Head(appended)
                                           ELSE watermarkA
                      /\ evictedA' = evictedA \cup {Head(appended)}
                 ELSE /\ ringA' = appended
                      /\ UNCHANGED <<watermarkA, evictedA>>
    /\ UNCHANGED <<phase, lastSeq, status, replaySet>>

(* record_room_event on room B (which has its own pending player, so its    *)
(* ring exists and records): consumes a global sequence number, creating a  *)
(* benign gap in room A's buffered sequences. Room B's ring contents never  *)
(* influence room A's status, so they are not stored.                       *)
EventB ==
    /\ nextSeq < EVENT_BUDGET
    /\ nextSeq' = nextSeq + 1
    /\ UNCHANGED <<ringA, watermarkA, recordedA, evictedA, phase, lastSeq,
                   status, replaySet>>

(* get_missed_events(room_A, last_sequence): the replay is the retained     *)
(* events with seq > last_sequence; the status is truncated iff the         *)
(* watermark proves a needed event was evicted. The NaiveGapPredicateBug    *)
(* arm decides truncation from the retained-sequence gap instead — the      *)
(* design this model proves WRONG.                                          *)
Reconnect ==
    /\ phase = "Pending"
    /\ phase' = "Reconnected"
    /\ IF SplitBrainCounterBug
           \* Split brain (ARCH-10): the token and this player's buffered /
           \* evicted stream are stranded on instance 1, but the reconnect is
           \* served by instance 2, which join-created room A fresh under its
           \* OWN next_sequence counter. Instance 2's ring is empty and its
           \* watermark is 0, so it can only answer "complete" with an empty
           \* replay — regardless of what instance 1 evicted.
           THEN /\ replaySet' = {}
                /\ status' = "complete"
           ELSE /\ replaySet' = {seq \in Range(ringA) : seq > lastSeq}
                /\ LET truncated ==
                           IF NaiveGapPredicateBug
                               THEN ringA # <<>> /\ Head(ringA) > lastSeq + 1
                               ELSE watermarkA > lastSeq
                   IN status' = IF truncated THEN "truncated" ELSE "complete"
    /\ UNCHANGED <<nextSeq, ringA, watermarkA, recordedA, evictedA, lastSeq>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful       *)
(* (matches the SignalFishSession convention): once the player reconnected  *)
(* the checked verdict is frozen; the system idles.                         *)
Done ==
    /\ phase = "Reconnected"
    /\ UNCHANGED vars

Next ==
    \/ Disconnect
    \/ EventA
    \/ EventB
    \/ Reconnect
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ nextSeq \in 0..EVENT_BUDGET
    /\ ringA \in Seq(1..EVENT_BUDGET)
    /\ Len(ringA) <= RING_CAPACITY
    /\ watermarkA \in 0..EVENT_BUDGET
    /\ recordedA \subseteq 1..EVENT_BUDGET
    /\ evictedA \subseteq recordedA
    /\ phase \in {"Connected", "Pending", "Reconnected"}
    /\ lastSeq \in 0..EVENT_BUDGET
    /\ status \in {"none", "complete", "truncated"}
    /\ replaySet \subseteq recordedA

(* THE replay-honesty invariant, two-sided: the reported status is          *)
(* "truncated" EXACTLY when an event the player needed (recorded in room A  *)
(* with seq > last_sequence) was evicted from the ring. Never "complete"    *)
(* over a lossy replay; never "truncated" over a benign cross-room gap.     *)
StatusHonest ==
    phase = "Reconnected" =>
        LET neededEvicted == \E seq \in evictedA : seq > lastSeq
        IN /\ (status = "complete") => ~neededEvicted
           /\ (status = "truncated") => neededEvicted

(* The replay returns exactly the retained needed events: every recorded,   *)
(* never-evicted room-A event above last_sequence — and nothing else (no    *)
(* already-seen events, nothing from room B).                               *)
ReplayFaithful ==
    phase = "Reconnected" =>
        replaySet = {seq \in recordedA \ evictedA : seq > lastSeq}

(* Ghost coupling: the watermark is precisely the maximum evicted sequence  *)
(* (0 when nothing was evicted) — the invariant under which the Z3          *)
(* obligation `truncation_predicate_sound_and_complete`                     *)
(* (formal/z3/protocol_invariants.py) proves the watermark predicate        *)
(* equivalent to the spec-level "a needed event was evicted".               *)
WatermarkIsMaxEvicted ==
    IF evictedA = {}
        THEN watermarkA = 0
        ELSE /\ watermarkA \in evictedA
             /\ \A seq \in evictedA : seq <= watermarkA

=============================================================================

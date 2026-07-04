------------------------------ MODULE SequencedRelay ------------------------------
(***************************************************************************)
(* The protocol v4 per-sender relay-sequencing contract (the DESIGN being  *)
(* implemented for spec/ protocol v4; the delivery substrate is the #131   *)
(* contract already modeled in DeliveryContract.tla):                      *)
(*                                                                         *)
(*   The server stamps every relayed game-data message with a strictly     *)
(*   contiguous per-(sender, room) sequence number starting at 1. The      *)
(*   counter RESETS when the sender leaves and rejoins the room. A         *)
(*   recipient therefore observes each sender's stream as consecutive      *)
(*   sequence numbers — and every gap or regression it can ever observe is *)
(*   ACCOUNTABLE: bracketed by a notification the recipient received (its  *)
(*   own slow-consumer eviction + reconnect, or the sender's PlayerLeft /  *)
(*   PlayerReconnected broadcast). No reachable state leaves a recipient   *)
(*   holding an unexplained gap.                                           *)
(*                                                                         *)
(* One sender, two recipients, bounded per-recipient queues. The queue     *)
(* abstraction is the DeliveryContract shape simplified to its contractual *)
(* outcomes: a delivery either enqueues (a full queue backpressures the    *)
(* sender, realized here as interleaving — Send is enabled only when every *)
(* connected recipient has a slot) or the stuck recipient is EVICTED       *)
(* (`Evict`, the slow-consumer disconnect, enabled exactly when its queue  *)
(* is full), losing its whole queue together with its connection. Per-     *)
(* recipient queues preserve order, so control notifications and data ride *)
(* the same stream exactly as broadcast + relay share the per-connection   *)
(* outbound queue in src/server.rs.                                        *)
(*                                                                         *)
(* Mapping to the implementation/design:                                   *)
(*   Send           relay stamping: seq := counter + 1 per (sender, room)  *)
(*   SenderLeave    leave_room: PlayerLeft broadcast to remaining members  *)
(*   SenderRejoin   rejoin: counter reset to 0 (next stamp is 1) +         *)
(*                  PlayerJoined/PlayerReconnected broadcast               *)
(*   Evict          CloseReason::SlowConsumer eviction of a full recipient *)
(*                  (deliver_or_disconnect grace expiry; queue abandoned   *)
(*                  with the connection — DeliveryContract.tla)            *)
(*   Reconnect      the evicted recipient reconnects; its own disconnect + *)
(*                  reconnect is a bracket it was BY CONSTRUCTION notified *)
(*                  of (it re-authenticated)                               *)
(*   Deliver        the recipient's socket writer draining one queued      *)
(*                  message (order-preserving)                             *)
(*                                                                         *)
(* The GapAccountable check is encoded operationally: each recipient       *)
(* carries a `justified` flag — set by observing a bracket (own reconnect, *)
(* sender PlayerLeft/PlayerReconnected), cleared by consuming a data       *)
(* message — and a ghost `accountable` flag that latches FALSE the moment  *)
(* a data message arrives whose seq is neither `lastObs + 1` nor covered   *)
(* by an unconsumed bracket. The invariant is that `accountable` never     *)
(* goes FALSE in any reachable state.                                      *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible): setting   *)
(* the `NoResetNotificationBug` constant to TRUE makes SenderLeave /       *)
(* SenderRejoin reset the counter WITHOUT broadcasting the PlayerLeft /    *)
(* PlayerReconnected notifications. TLC then reports a `GapAccountable`    *)
(* violation within a handful of steps; the minimal trace (verified        *)
(* manually) is 6 actions:                                                 *)
(*                                                                         *)
(*   Send (seq 1) -> Deliver r1 (lastObs 1) -> SenderLeave (silent) ->     *)
(*   SenderRejoin (counter reset, silent) -> Send (seq 1 again) ->         *)
(*   Deliver r1: seq 1 is a REGRESSION (not lastObs + 1 = 2) with no       *)
(*   bracket observed — an unexplained gap, `accountable[r1] = FALSE`.     *)
(*                                                                         *)
(* This proves the reset NOTIFICATION is what makes the counter reset      *)
(* sound: sequencing alone cannot distinguish a legal epoch restart from   *)
(* relay loss. The checked configuration pins                              *)
(* `NoResetNotificationBug = FALSE`; flip it locally to watch the          *)
(* invariant catch the bug.                                                *)
(*                                                                         *)
(* SINGLE-INSTANCE THEOREM (ARCH-10). GapAccountable is a theorem of a     *)
(* SINGLE relay instance: the whole per-(sender, room) contract rests on   *)
(* ONE authoritative stamp counter. Behind a load balancer that lets a     *)
(* room span two instances (join-with-unknown-code CREATES the room — the  *)
(* `Ok(None)` arm of `join_room_with_coordination`,                        *)
(* `src/server/room_service.rs:519`), the same sender's stream is stamped  *)
(* by two INDEPENDENT counters, and a recipient whose reads interleave the *)
(* two sees duplicate / regressing seq with no bracket. The                *)
(* `SplitBrainStampBug` constant models exactly this: when TRUE, a second  *)
(* instance (`SendSplit`, stamping from the independent `counter2`) may    *)
(* also relay the sender's stream. TLC then reports a `GapAccountable`     *)
(* violation; the minimal trace (verified during development) is 4 actions:*)
(*                                                                         *)
(*   Send (instance 1, seq 1) -> SendSplit (instance 2, seq 1 again) ->    *)
(*   Deliver r1 (lastObs 1) -> Deliver r1: seq 1 is a REGRESSION with no   *)
(*   bracket observed — `accountable[r1] = FALSE`.                         *)
(*                                                                         *)
(* This is executable documentation that the sequencing contract does NOT  *)
(* survive multi-instance split-brain: it is a single-node CP guarantee    *)
(* requiring LB room-affinity (one home per room). See D6 in PLAN.md and   *)
(* the "single-instance theorems" section of formal/README.md. The checked *)
(* configuration pins `SplitBrainStampBug = FALSE`.                        *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Recipients,            \* nonempty set of recipient identities (keep tiny: 2)
    SendBudget,            \* total relayed messages across all epochs (keep tiny: 3)
    QueueCapacity,         \* bounded per-recipient queue slots (keep tiny: 2)
    CycleBudget,           \* sender leave+rejoin cycles (keep tiny: 1)
    ReconnectBudget,       \* per-recipient eviction+reconnect cycles (keep tiny: 1)
    NoResetNotificationBug,\* TRUE resets the counter without notifying (must
                           \* violate GapAccountable; FALSE in checked configs)
    SplitBrainStampBug     \* TRUE lets a second instance stamp the same
                           \* sender's stream from an independent counter (a
                           \* split-brain relay under a room-spanning LB); must
                           \* violate GapAccountable; FALSE in checked configs

ASSUME /\ Recipients # {}
       /\ SendBudget \in Nat \ {0}
       /\ QueueCapacity \in Nat \ {0}
       /\ CycleBudget \in Nat
       /\ ReconnectBudget \in Nat
       /\ NoResetNotificationBug \in BOOLEAN
       /\ SplitBrainStampBug \in BOOLEAN

VARIABLES
    senderIn,    \* whether the sender is currently seated in the room
    counter,     \* the per-(sender, room) stamp counter for the current epoch
    counter2,    \* a SECOND instance's independent stamp counter for the same
                 \* (sender, room) — only ever advanced under SplitBrainStampBug
                 \* (models a room-spanning split brain); stays 0 otherwise
    epoch,       \* ghost: epoch id (bumped by every counter reset)
    sends,       \* relayed messages so far (budget)
    cycles,      \* sender leave+rejoin cycles consumed
    conn,        \* [Recipients -> {"Connected", "Dropped"}]
    queue,       \* [Recipients -> Seq(Msg)]: bounded outbound queues
    reconnects,  \* [Recipients -> Nat]: eviction+reconnect cycles consumed
    lastObs,     \* [Recipients -> Nat]: last observed data sequence number
    justified,   \* [Recipients -> BOOLEAN]: a bracket observed since last data
    accountable  \* [Recipients -> BOOLEAN] ghost: latches FALSE on an
                 \* unexplained gap — the checked verdict

vars == <<senderIn, counter, counter2, epoch, sends, cycles, conn, queue,
          reconnects, lastObs, justified, accountable>>

DataMsgs == [kind : {"data"}, seq : 1..SendBudget]
CtrlMsgs == [kind : {"left", "rejoin"}]
Msg == DataMsgs \cup CtrlMsgs

(* Broadcast a control notification onto every connected recipient's queue  *)
(* (order-preserving, same stream as data). Under NoResetNotificationBug    *)
(* the notification is silently skipped — the injected design bug.          *)
BroadcastCtrl(kind) ==
    IF NoResetNotificationBug
        THEN queue' = queue
        ELSE queue' = [r \in Recipients |->
                          IF conn[r] = "Connected"
                              THEN Append(queue[r], [kind |-> kind])
                              ELSE queue[r]]

(* Every connected recipient can absorb one more message. A full connected  *)
(* recipient backpressures the broadcast (interleaving: Deliver frees a     *)
(* slot, or Evict removes the recipient) exactly per the #131 contract.     *)
AllConnectedHaveRoom ==
    \A r \in Recipients :
        conn[r] = "Connected" => Len(queue[r]) < QueueCapacity

Init ==
    /\ senderIn = TRUE
    /\ counter = 0
    /\ counter2 = 0
    /\ epoch = 1
    /\ sends = 0
    /\ cycles = 0
    /\ conn = [r \in Recipients |-> "Connected"]
    /\ queue = [r \in Recipients |-> <<>>]
    /\ reconnects = [r \in Recipients |-> 0]
    /\ lastObs = [r \in Recipients |-> 0]
    /\ justified = [r \in Recipients |-> FALSE]
    /\ accountable = [r \in Recipients |-> TRUE]

(* The sender relays one message: the server stamps seq = counter + 1       *)
(* (strictly contiguous within the epoch) and enqueues it for every         *)
(* connected recipient.                                                     *)
Send ==
    /\ senderIn
    /\ sends < SendBudget
    /\ AllConnectedHaveRoom
    /\ counter' = counter + 1
    /\ sends' = sends + 1
    /\ queue' = [r \in Recipients |->
                    IF conn[r] = "Connected"
                        THEN Append(queue[r], [kind |-> "data", seq |-> counter + 1])
                        ELSE queue[r]]
    /\ UNCHANGED <<senderIn, counter2, epoch, cycles, conn, reconnects, lastObs,
                   justified, accountable>>

(* A SECOND relay instance stamps the SAME sender's stream from its own      *)
(* independent counter (`counter2`). Enabled only under SplitBrainStampBug — *)
(* the room-spanning split brain of ARCH-10, where join-with-unknown-code    *)
(* creates a second live room on another instance. Both instances draw from  *)
(* the shared send budget, so the model stays bounded; the two counters are  *)
(* wholly independent, so a recipient's reads can interleave duplicate or    *)
(* regressing seq with NO bracket — exactly the unaccountable gap.           *)
SendSplit ==
    /\ SplitBrainStampBug
    /\ senderIn
    /\ sends < SendBudget
    /\ AllConnectedHaveRoom
    /\ counter2' = counter2 + 1
    /\ sends' = sends + 1
    /\ queue' = [r \in Recipients |->
                    IF conn[r] = "Connected"
                        THEN Append(queue[r], [kind |-> "data", seq |-> counter2 + 1])
                        ELSE queue[r]]
    /\ UNCHANGED <<senderIn, counter, epoch, cycles, conn, reconnects, lastObs,
                   justified, accountable>>

(* The sender leaves the room; PlayerLeft is broadcast to the remaining     *)
(* members (skipped by the injected bug). Guarded on queue room only when   *)
(* actually broadcasting — the backpressured broadcast completes or evicts  *)
(* before the leave finishes.                                               *)
SenderLeave ==
    /\ senderIn
    /\ cycles < CycleBudget
    /\ NoResetNotificationBug \/ AllConnectedHaveRoom
    /\ senderIn' = FALSE
    /\ cycles' = cycles + 1
    /\ BroadcastCtrl("left")
    /\ UNCHANGED <<counter, counter2, epoch, sends, conn, reconnects, lastObs,
                   justified, accountable>>

(* The sender rejoins: the per-(sender, room) counter RESETS (the next      *)
(* stamp is 1 — a new epoch) and PlayerReconnected/PlayerJoined is          *)
(* broadcast (skipped by the injected bug).                                 *)
SenderRejoin ==
    /\ ~senderIn
    /\ NoResetNotificationBug \/ AllConnectedHaveRoom
    /\ senderIn' = TRUE
    /\ counter' = 0
    /\ epoch' = epoch + 1
    /\ BroadcastCtrl("rejoin")
    /\ UNCHANGED <<counter2, sends, cycles, conn, reconnects, lastObs, justified,
                   accountable>>

(* Slow-consumer eviction: a recipient whose queue is full may be           *)
(* disconnected (DeliveryContract's GraceExpired + CloseFinish collapsed),  *)
(* abandoning its queued messages together with its connection.             *)
Evict(r) ==
    /\ conn[r] = "Connected"
    /\ Len(queue[r]) = QueueCapacity
    /\ conn' = [conn EXCEPT ![r] = "Dropped"]
    /\ queue' = [queue EXCEPT ![r] = <<>>]
    /\ UNCHANGED <<senderIn, counter, counter2, epoch, sends, cycles, reconnects,
                   lastObs, justified, accountable>>

(* The evicted recipient reconnects. Its own disconnect + reconnect is a    *)
(* bracket it was by construction notified of, so any forward gap (missed   *)
(* messages) or regression (an epoch reset it never saw notified) across    *)
(* the outage is accounted for.                                             *)
Reconnect(r) ==
    /\ conn[r] = "Dropped"
    /\ reconnects[r] < ReconnectBudget
    /\ conn' = [conn EXCEPT ![r] = "Connected"]
    /\ reconnects' = [reconnects EXCEPT ![r] = @ + 1]
    /\ justified' = [justified EXCEPT ![r] = TRUE]
    /\ UNCHANGED <<senderIn, counter, counter2, epoch, sends, cycles, queue,
                   lastObs, accountable>>

(* The recipient's writer drains one message, in order. A data message is   *)
(* legal iff contiguous (seq = lastObs + 1) or covered by an unconsumed     *)
(* bracket; anything else latches the accountable verdict FALSE. A control  *)
(* message (PlayerLeft / PlayerReconnected) arms the bracket.               *)
Deliver(r) ==
    /\ conn[r] = "Connected"
    /\ queue[r] # <<>>
    /\ LET m == Head(queue[r])
       IN /\ queue' = [queue EXCEPT ![r] = Tail(@)]
          /\ IF m.kind = "data"
                 THEN /\ accountable' = [accountable EXCEPT
                              ![r] = @ /\ (m.seq = lastObs[r] + 1 \/ justified[r])]
                      /\ lastObs' = [lastObs EXCEPT ![r] = m.seq]
                      /\ justified' = [justified EXCEPT ![r] = FALSE]
                 ELSE /\ justified' = [justified EXCEPT ![r] = TRUE]
                      /\ UNCHANGED <<lastObs, accountable>>
    /\ UNCHANGED <<senderIn, counter, counter2, epoch, sends, cycles, conn,
                   reconnects>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful       *)
(* (matches the SignalFishSession convention): all budgets spent, every     *)
(* queue drained, every dropped recipient out of reconnects.                *)
AllResolved ==
    /\ sends = SendBudget
    /\ senderIn
    /\ cycles = CycleBudget
    /\ \A r \in Recipients :
           IF conn[r] = "Connected"
               THEN queue[r] = <<>>
               ELSE reconnects[r] = ReconnectBudget

Done ==
    /\ AllResolved
    /\ UNCHANGED vars

Next ==
    \/ Send
    \/ SendSplit
    \/ SenderLeave
    \/ SenderRejoin
    \/ \E r \in Recipients : Evict(r) \/ Reconnect(r) \/ Deliver(r)
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ senderIn \in BOOLEAN
    /\ counter \in 0..SendBudget
    /\ counter2 \in 0..SendBudget
    /\ epoch \in 1..(CycleBudget + 1)
    /\ sends \in 0..SendBudget
    /\ cycles \in 0..CycleBudget
    /\ conn \in [Recipients -> {"Connected", "Dropped"}]
    /\ queue \in [Recipients -> Seq(Msg)]
    /\ \A r \in Recipients : Len(queue[r]) <= QueueCapacity
    /\ reconnects \in [Recipients -> 0..ReconnectBudget]
    /\ lastObs \in [Recipients -> 0..SendBudget]
    /\ justified \in [Recipients -> BOOLEAN]
    /\ accountable \in [Recipients -> BOOLEAN]

(* THE v4 sequencing invariant: no reachable state leaves any recipient     *)
(* holding an unexplained gap — every observed non-contiguity was bracketed *)
(* by a notification the recipient received (its own eviction + reconnect,  *)
(* or the sender's PlayerLeft / PlayerReconnected).                         *)
GapAccountable ==
    \A r \in Recipients : accountable[r]

(* An eviction is never partial: a dropped recipient holds no queue — its   *)
(* messages were abandoned together with the connection (never delivered    *)
(* out of order later).                                                     *)
DroppedQueueEmpty ==
    \A r \in Recipients : conn[r] = "Dropped" => queue[r] = <<>>

(* Server-side stamping soundness: every data message still queued carries  *)
(* a stamp from the current epoch's contiguous range (<= counter). Queued   *)
(* stale-epoch stamps cannot exceed the budget-bounded domain either.       *)
QueuedStampsWereIssued ==
    \A r \in Recipients :
        \A i \in DOMAIN queue[r] :
            queue[r][i].kind = "data" => queue[r][i].seq \in 1..SendBudget

=============================================================================

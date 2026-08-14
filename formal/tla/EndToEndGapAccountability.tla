------------------------ MODULE EndToEndGapAccountability ------------------------
(***************************************************************************)
(* The flagship end-to-end gap-accountability contract: the                *)
(* protocol v3 promise that a client, driven ONLY by what the server puts   *)
(* on its socket, can classify every sequence discontinuity it ever         *)
(* observes — and that the reconnection SNAPSHOT heals the tail the server   *)
(* dropped when it evicted the client. This composes three previously       *)
(* separate contracts into one behavior:                                    *)
(*                                                                          *)
(*   - SequencedRelay.tla   per-(sender, room) contiguous stamping + the     *)
(*                          "every gap is bracketed" accountability check    *)
(*   - ReconnectReplay.tla  the bounded replay ring + eviction watermark     *)
(*                          (`reconnection::EventBuffer`) that a reconnect    *)
(*                          serves, possibly truncated                       *)
(*   - ConnectionTeardown   slow-consumer eviction that abandons a           *)
(*                          recipient's queued messages with its connection  *)
(*                                                                          *)
(* Why this module exists (three things the single-contract specs cannot     *)
(* see, each pinned by a seeded-bug constant that MUST violate an invariant):*)
(*                                                                          *)
(*  1. TWO SENDERS. SequencedRelay's `justified` bracket is a SINGLE         *)
(*     per-recipient flag. With one sender that is sound. With two it is     *)
(*     NOT: a bracket armed for sender A (an observed PlayerLeft /           *)
(*     PlayerReconnected of A) is CONSUMED by the next data message from     *)
(*     ANY sender — so a perfectly contiguous frame from B silently spends   *)
(*     A's bracket, and A's legitimate epoch reset then reads as an          *)
(*     unexplained regression. The sound design tracks justification         *)
(*     PER (recipient, sender) pair. `SingleFlagBug` collapses the per-pair  *)
(*     flag back to one shared flag and TLC catches the false-positive.      *)
(*                                                                          *)
(*  2. A per-recipient SOCKET BUFFER between the server's writer and the     *)
(*     client's observation. Written-to-socket is NOT processed-by-client:   *)
(*     `WriterDrain` moves a frame from the outbound queue into `sockBuf`,   *)
(*     and only `ClientObserve` makes the client act on it. An `Evict` wipes *)
(*     BOTH the queue AND `sockBuf` (the kernel send/receive buffers die     *)
(*     with the TCP connection), so a frame the server already handed to the *)
(*     OS can still be lost. `DroppedNeverObserved` is the guarantee that a  *)
(*     wiped frame never resurfaces out of order.                           *)
(*                                                                          *)
(*  3. The reconnection SNAPSHOT as the authoritative heal (validates E5).   *)
(*     On reconnect the server sends the client an authoritative view — the  *)
(*     current member set (`RoomJoined.current_players`) AND each live       *)
(*     sender's current (epoch, seq) high-water mark                         *)
(*     (`Reconnected.sender_watermarks`). The client must                    *)
(*     use it two ways:                                                      *)
(*       (a) REPLACE its membership with the snapshot's member set — an      *)
(*           upsert, not a replay of deltas — so a PlayerReconnected that was *)
(*           dropped from the wiped buffer (or truncated from the replay     *)
(*           ring) cannot leave the client's membership out of sync with the *)
(*           room. `NoSnapshotReconcileBug` applies only the (possibly       *)
(*           incomplete) delta replay and TLC catches the divergence at      *)
(*           quiescence. NOTE the direction the terminal check proves: since *)
(*           `AllResolved` requires every sender seated, the exposed defect  *)
(*           is the client UNDER-counting — still believing a now-seated     *)
(*           sender left (a dropped PlayerReconnected). The symmetric phantom *)
(*           (retaining a departed member) is out of the terminal state's    *)
(*           reach; the upsert heals both directions by construction, but    *)
(*           only the under-count is machine-checked here.                   *)
(*       (b) RE-BASELINE each per-sender (epoch, seq) expectation from the    *)
(*           watermark set, so the next frame from every sender is           *)
(*           contiguous against a fresh baseline rather than a stale one     *)
(*           from before the outage. `NoBaselineResetBug` skips the          *)
(*           re-baseline and TLC catches the unclassifiable gap. This is the *)
(*           executable proof that E5's watermarks are NECESSARY: without    *)
(*           them a reconnecting client cannot tell an outage gap (its own   *)
(*           eviction dropped the tail) from relay loss.                     *)
(*                                                                          *)
(* Topology: ONE recipient observing TWO senders. A second recipient adds    *)
(* nothing D4 needs (the observe-vs-evict contrast between two recipients is *)
(* SequencedRelay's job); D4's novelty is entirely on the per-SENDER axis    *)
(* and the socket-buffer / snapshot-heal axis, so the model spends its state *)
(* budget there. The spec is written generically over `Recipients` and the   *)
(* checked configuration instantiates a single one. That single-recipient     *)
(* instantiation is also where the model is FAITHFUL: `AllConnectedHaveRoom`   *)
(* is a global backpressure guard, so at |Recipients| >= 2 one full queue      *)
(* would stall the whole room's relay (global head-of-line blocking) rather    *)
(* than isolating and evicting just the slow consumer (#131). At one recipient *)
(* the two coincide, so the checked config models the per-connection contract  *)
(* exactly; a multi-recipient run would need per-recipient backpressure.       *)
(*                                                                          *)
(* Mapping to the implementation / design:                                   *)
(*   SendData        relay stamping: seq := counter[s] + 1 per (sender, room)*)
(*   SenderLeave     leave_room: PlayerLeft broadcast to members AND recorded*)
(*                   in the reconnection EventBuffer (ring)                  *)
(*   SenderRejoin    rejoin: counter reset to 0 (next stamp 1), epoch bump,  *)
(*                   PlayerReconnected broadcast AND recorded in the ring    *)
(*   WriterDrain     the socket writer moving one queued frame to the kernel *)
(*                   send buffer (queue -> sockBuf)                          *)
(*   ClientObserve   the client actually reading/applying one frame          *)
(*                   (sockBuf -> classification / membership delta)          *)
(*   Evict           CloseReason::SlowConsumer: full queue -> disconnect,    *)
(*                   queue AND sockBuf abandoned, replay cursor snapshotted   *)
(*                   from the global ctrl counter (Seam 1)                   *)
(*   ClientReconnect Reconnected: authoritative snapshot (member set +       *)
(*                   per-sender watermarks) + bounded replay of ring events  *)
(*                   above the cursor (truncated iff the watermark proves an *)
(*                   event was evicted)                                      *)
(*                                                                          *)
(* NON-VACUITY (verified during development, keep reproducible). Each seeded  *)
(* constant is FALSE in the checked configuration; flip exactly one to TRUE  *)
(* locally to watch the paired invariant catch the design bug.               *)
(*                                                                          *)
(*   SingleFlagBug -> ClientCanClassify. One shared justification flag       *)
(*   instead of one per (recipient, sender): a contiguous frame from s2       *)
(*   spends the bracket s1 needs for its epoch reset. Trace (TLC-verified):   *)
(*     SenderLeave s1 -> SenderRejoin s1 (epoch2; the "left s1"/"rejoin s1"   *)
(*     ctrl frames queue in order) -> ClientObserve "left s1" then            *)
(*     "rejoin s1" (shared flag armed for BOTH senders) ->                    *)
(*     SendData s2 (seq1) -> ClientObserve s2/seq1 (contiguous, but the       *)
(*     shared flag is spent for BOTH) -> SendData s1 (epoch2 seq1) ->         *)
(*     ClientObserve: s1's epoch2/seq1 is not contiguous with its ep1/seq0    *)
(*     baseline and the flag it needed was consumed by s2 -> accountable      *)
(*     FALSE. The per-pair flag keeps s2's traffic from spending s1's bracket.*)
(*                                                                          *)
(*   NoBaselineResetBug -> ClientCanClassify. Reconnect does NOT re-baseline  *)
(*   per-sender (epoch, seq) from the snapshot watermarks. Trace              *)
(*   (TLC-verified):                                                          *)
(*     SendData s1 (seq1) -> Evict (queue full; the frame the client never   *)
(*     saw dies with the socket, cursor snapshotted) ->                       *)
(*     ClientReconnect (buggy: s1 baseline left at ep1/seq0 instead of the    *)
(*     ep1/seq1 watermark) -> SendData s1 (seq2) -> ClientObserve: seq2       *)
(*     against a stale seq0 baseline is a gap with no armed bracket ->        *)
(*     accountable FALSE. The client cannot tell its own outage gap from      *)
(*     relay loss without the watermark.                                      *)
(*                                                                          *)
(*   NoSnapshotReconcileBug -> MembershipEventuallyHonest. Reconnect applies  *)
(*   only the (possibly incomplete) delta replay instead of replacing        *)
(*   membership with the authoritative snapshot set. Trace (TLC-verified):    *)
(*     SenderLeave s1 -> ClientObserve "left s1" (client drops s1) ->        *)
(*     SenderRejoin s1 -> Evict (the "rejoin s1" frame dies in the wiped     *)
(*     socket buffer; the cursor snapshots AT that event's seq, so the        *)
(*     replay serves nothing above it) -> SendData s1 x3 to the send budget   *)
(*     -> ClientReconnect (buggy: empty above-cursor replay, membership       *)
(*     stays {s2}) -> quiescence: all senders seated but the client still     *)
(*     believes s1 left -> MembershipEventuallyHonest FALSE.                  *)
(*                                                                          *)
(* SINGLE-INSTANCE THEOREM (ARCH-10). Every invariant here rests on ONE      *)
(* authoritative relay: one stamp counter per (sender, room), one global     *)
(* ctrl-sequence counter, one replay ring, and a reconnect served by the     *)
(* instance that owns the room. Behind a load balancer that lets a room span *)
(* two instances the composition breaks exactly as SequencedRelay.tla's      *)
(* `SplitBrainStampBug` and ReconnectReplay.tla's `SplitBrainCounterBug`     *)
(* already demonstrate; D4 does not re-derive that (it is the paired specs'  *)
(* single-instance theorems, catalogued in formal/README.md), it composes    *)
(* the single-instance behaviors to prove the client-facing end-to-end       *)
(* contract and to DERIVE the E5 client-SDK re-baseline obligation.          *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Senders,               \* the senders sharing the room (MANDATORY: 2 — the
                           \* single-sender justified-flag abstraction is unsound
                           \* at >= 2 senders; see SingleFlagBug)
    Recipients,            \* the observing clients (checked config: 1)
    SendBudget,            \* total relayed data messages across all senders/epochs
    QCAP,                  \* per-recipient outbound queue slots (keep tiny: 1)
    SOCKCAP,               \* per-recipient socket-buffer slots (keep tiny: 1)
    RINGCAP,               \* replay-ring slots for control events (keep tiny: 1)
    CycleBudget,           \* total sender leave+rejoin cycles (keep tiny: 1)
    ReconnectBudget,       \* per-recipient eviction+reconnect cycles (keep tiny: 1)
    SingleFlagBug,         \* TRUE collapses per-(recipient,sender) justification to
                           \* one shared flag (must violate ClientCanClassify)
    NoBaselineResetBug,    \* TRUE makes reconnect skip the per-sender watermark
                           \* re-baseline (must violate ClientCanClassify)
    NoSnapshotReconcileBug \* TRUE makes reconnect apply only the delta replay
                           \* instead of the authoritative member set (must
                           \* violate MembershipEventuallyHonest)

\* Two control events (leave + rejoin) are the most a single cycle records.
CtrlBudget == 2 * CycleBudget
MaxEpoch   == CycleBudget + 1

ASSUME /\ Senders # {}
       /\ Recipients # {}
       /\ SendBudget \in Nat \ {0}
       /\ QCAP \in Nat \ {0}
       /\ SOCKCAP \in Nat \ {0}
       /\ RINGCAP \in Nat \ {0}
       /\ CycleBudget \in Nat
       /\ ReconnectBudget \in Nat
       /\ SingleFlagBug \in BOOLEAN
       /\ NoBaselineResetBug \in BOOLEAN
       /\ NoSnapshotReconcileBug \in BOOLEAN

VARIABLES
    \* --- server: authoritative relay state ---
    senderIn,      \* [Senders -> BOOLEAN]: whether each sender is seated
    counter,       \* [Senders -> Nat]: per-(sender, room) stamp counter, this epoch
    epoch,         \* [Senders -> Nat]: per-sender epoch id (bumped by every reset)
    sends,         \* Nat: data messages relayed so far (budget)
    cycles,        \* Nat: sender leave+rejoin cycles consumed (budget)
    nextCtrlSeq,   \* Nat: ONE global control-event sequence counter
    ring,          \* Seq of ctrl records still retained (ascending seq)
    watermark,     \* Nat: highest control seq ever evicted from the ring (0 = none)
    \* --- per recipient: transport ---
    conn,          \* [Recipients -> {"Connected", "Dropped"}]
    queue,         \* [Recipients -> Seq(Msg)]: bounded outbound queue
    sockBuf,       \* [Recipients -> Seq(Msg)]: written-to-socket, unobserved
    reconnects,    \* [Recipients -> Nat]: eviction+reconnect cycles consumed
    snapSeq,       \* [Recipients -> Nat]: replay cursor snapshotted at eviction
    \* --- per recipient: client-side observation ---
    obsEpoch,      \* [Recipients -> [Senders -> Nat]]: last observed epoch per sender
    obsSeq,        \* [Recipients -> [Senders -> Nat]]: last observed seq per sender
    justified,     \* [Recipients -> [Senders -> BOOLEAN]]: per-pair bracket armed
    clientMembers, \* [Recipients -> SUBSET Senders]: the client's believed membership
    accountable    \* [Recipients -> BOOLEAN] ghost: latches FALSE on an
                   \* unexplained gap — the checked verdict

vars == <<senderIn, counter, epoch, sends, cycles, nextCtrlSeq, ring, watermark,
          conn, queue, sockBuf, reconnects, snapSeq, obsEpoch, obsSeq, justified,
          clientMembers, accountable>>

DataMsg == [kind : {"data"}, from : Senders, ep : 1..MaxEpoch, seq : 1..SendBudget]
CtrlMsg == [kind : {"ctrl"}, ev : {"left", "rejoin"}, who : Senders]
Msg     == DataMsg \cup CtrlMsg

CtrlRec == [seq : 1..CtrlBudget, ev : {"left", "rejoin"}, who : Senders]

\* Every connected recipient can absorb one more queued frame; a full connected
\* recipient backpressures the broadcast (interleaving: WriterDrain/ClientObserve
\* frees a slot, or Evict removes the recipient) — the #131 slow-consumer contract.
AllConnectedHaveRoom ==
    \A r \in Recipients :
        conn[r] = "Connected" => Len(queue[r]) < QCAP

\* Broadcast a control notification onto every connected recipient's live queue
\* (order-preserving, same stream as data — broadcast and relay share the
\* per-connection outbound queue in src/server.rs).
BroadcastCtrl(ev, who) ==
    [r \in Recipients |->
        IF conn[r] = "Connected"
            THEN Append(queue[r], [kind |-> "ctrl", ev |-> ev, who |-> who])
            ELSE queue[r]]

\* Record a control event in the bounded reconnection ring: assign the next
\* global sequence, push, and evict the oldest past capacity (raising the
\* watermark — the ring is FIFO so the evicted head is the highest evicted).
RecordedRing(ev, who) ==
    LET s        == nextCtrlSeq + 1
        appended == Append(ring, [seq |-> s, ev |-> ev, who |-> who])
    IN IF Len(appended) > RINGCAP
           THEN [ring |-> Tail(appended), watermark |-> Head(appended).seq]
           ELSE [ring |-> appended, watermark |-> watermark]

Init ==
    /\ senderIn = [s \in Senders |-> TRUE]
    /\ counter = [s \in Senders |-> 0]
    /\ epoch = [s \in Senders |-> 1]
    /\ sends = 0
    /\ cycles = 0
    /\ nextCtrlSeq = 0
    /\ ring = <<>>
    /\ watermark = 0
    /\ conn = [r \in Recipients |-> "Connected"]
    /\ queue = [r \in Recipients |-> <<>>]
    /\ sockBuf = [r \in Recipients |-> <<>>]
    /\ reconnects = [r \in Recipients |-> 0]
    /\ snapSeq = [r \in Recipients |-> 0]
    /\ obsEpoch = [r \in Recipients |-> [s \in Senders |-> 1]]
    /\ obsSeq = [r \in Recipients |-> [s \in Senders |-> 0]]
    /\ justified = [r \in Recipients |-> [s \in Senders |-> FALSE]]
    /\ clientMembers = [r \in Recipients |-> {s \in Senders : TRUE}]
    /\ accountable = [r \in Recipients |-> TRUE]

(* The server relays one message from sender s: stamp seq = counter[s] + 1     *)
(* (strictly contiguous within the epoch) and enqueue it for every connected   *)
(* recipient.                                                                  *)
SendData(s) ==
    /\ senderIn[s]
    /\ sends < SendBudget
    /\ AllConnectedHaveRoom
    /\ counter' = [counter EXCEPT ![s] = @ + 1]
    /\ sends' = sends + 1
    /\ queue' = [r \in Recipients |->
                    IF conn[r] = "Connected"
                        THEN Append(queue[r],
                                 [kind |-> "data", from |-> s,
                                  ep |-> epoch[s], seq |-> counter[s] + 1])
                        ELSE queue[r]]
    /\ UNCHANGED <<senderIn, epoch, cycles, nextCtrlSeq, ring, watermark, conn,
                   sockBuf, reconnects, snapSeq, obsEpoch, obsSeq, justified,
                   clientMembers, accountable>>

(* Sender s leaves: PlayerLeft is broadcast to remaining members AND recorded  *)
(* in the reconnection ring for later replay.                                  *)
SenderLeave(s) ==
    /\ senderIn[s]
    /\ cycles < CycleBudget
    /\ AllConnectedHaveRoom
    /\ senderIn' = [senderIn EXCEPT ![s] = FALSE]
    /\ cycles' = cycles + 1
    /\ queue' = BroadcastCtrl("left", s)
    /\ nextCtrlSeq' = nextCtrlSeq + 1
    /\ LET recorded == RecordedRing("left", s)
       IN /\ ring' = recorded.ring
          /\ watermark' = recorded.watermark
    /\ UNCHANGED <<counter, epoch, sends, conn, sockBuf, reconnects, snapSeq,
                   obsEpoch, obsSeq, justified, clientMembers, accountable>>

(* Sender s rejoins: the per-(sender, room) counter RESETS (next stamp is 1 —  *)
(* a new epoch) and PlayerReconnected is broadcast AND recorded in the ring.    *)
SenderRejoin(s) ==
    /\ ~senderIn[s]
    /\ AllConnectedHaveRoom
    /\ senderIn' = [senderIn EXCEPT ![s] = TRUE]
    /\ counter' = [counter EXCEPT ![s] = 0]
    /\ epoch' = [epoch EXCEPT ![s] = @ + 1]
    /\ queue' = BroadcastCtrl("rejoin", s)
    /\ nextCtrlSeq' = nextCtrlSeq + 1
    /\ LET recorded == RecordedRing("rejoin", s)
       IN /\ ring' = recorded.ring
          /\ watermark' = recorded.watermark
    /\ UNCHANGED <<sends, cycles, conn, sockBuf, reconnects, snapSeq, obsEpoch,
                   obsSeq, justified, clientMembers, accountable>>

(* The socket writer moves one queued frame into the kernel send buffer.        *)
(* Written-to-socket is not yet processed-by-client (that is ClientObserve).    *)
WriterDrain(r) ==
    /\ conn[r] = "Connected"
    /\ queue[r] # <<>>
    /\ Len(sockBuf[r]) < SOCKCAP
    /\ queue' = [queue EXCEPT ![r] = Tail(@)]
    /\ sockBuf' = [sockBuf EXCEPT ![r] = Append(@, Head(queue[r]))]
    /\ UNCHANGED <<senderIn, counter, epoch, sends, cycles, nextCtrlSeq, ring,
                   watermark, conn, reconnects, snapSeq, obsEpoch, obsSeq,
                   justified, clientMembers, accountable>>

(* The client reads/applies one frame from its socket buffer. A data frame is   *)
(* accountable iff contiguous with the client's per-sender baseline OR covered  *)
(* by a bracket armed for THAT sender; anything else latches the verdict FALSE. *)
(* A control frame applies the membership delta and arms the sender's bracket.  *)
(* Under SingleFlagBug arming/consumption touch ALL senders — one shared flag.  *)
ClientObserve(r) ==
    /\ conn[r] = "Connected"
    /\ sockBuf[r] # <<>>
    /\ LET m == Head(sockBuf[r])
       IN /\ sockBuf' = [sockBuf EXCEPT ![r] = Tail(@)]
          /\ IF m.kind = "data"
                 THEN LET s          == m.from
                          contiguous == m.ep = obsEpoch[r][s] /\ m.seq = obsSeq[r][s] + 1
                          ok         == contiguous \/ justified[r][s]
                      IN /\ accountable' = [accountable EXCEPT ![r] = @ /\ ok]
                         /\ obsEpoch' = [obsEpoch EXCEPT ![r][s] = m.ep]
                         /\ obsSeq' = [obsSeq EXCEPT ![r][s] = m.seq]
                         /\ justified' =
                                [justified EXCEPT ![r] =
                                    IF SingleFlagBug
                                        THEN [t \in Senders |-> FALSE]
                                        ELSE [@ EXCEPT ![s] = FALSE]]
                         /\ UNCHANGED clientMembers
                 ELSE LET s == m.who
                      IN /\ clientMembers' =
                                [clientMembers EXCEPT ![r] =
                                    IF m.ev = "left" THEN @ \ {s} ELSE @ \cup {s}]
                         /\ justified' =
                                [justified EXCEPT ![r] =
                                    IF SingleFlagBug
                                        THEN [t \in Senders |-> TRUE]
                                        ELSE [@ EXCEPT ![s] = TRUE]]
                         /\ UNCHANGED <<accountable, obsEpoch, obsSeq>>
    /\ UNCHANGED <<senderIn, counter, epoch, sends, cycles, nextCtrlSeq, ring,
                   watermark, conn, queue, reconnects, snapSeq>>

(* Slow-consumer eviction: a recipient whose queue is full is disconnected. Its *)
(* queue AND its socket buffer die with the TCP connection (kernel-buffer       *)
(* loss), and the replay cursor is snapshotted from the global ctrl counter     *)
(* (register_disconnection: last_sequence := next_sequence — Seam 1).           *)
Evict(r) ==
    /\ conn[r] = "Connected"
    /\ Len(queue[r]) = QCAP
    /\ conn' = [conn EXCEPT ![r] = "Dropped"]
    /\ queue' = [queue EXCEPT ![r] = <<>>]
    /\ sockBuf' = [sockBuf EXCEPT ![r] = <<>>]
    /\ snapSeq' = [snapSeq EXCEPT ![r] = nextCtrlSeq]
    /\ UNCHANGED <<senderIn, counter, epoch, sends, cycles, nextCtrlSeq, ring,
                   watermark, reconnects, obsEpoch, obsSeq, justified,
                   clientMembers, accountable>>

\* Fold the ring's above-cursor control events onto a membership set, oldest
\* first (the delta-replay path the snapshot reconcile is meant to make
\* unnecessary — modeled only to exhibit NoSnapshotReconcileBug).
RECURSIVE ApplyReplay(_, _, _)
ApplyReplay(mem, s, cursor) ==
    IF s = <<>>
        THEN mem
        ELSE LET e    == Head(s)
                 mem2 == IF e.seq > cursor
                             THEN IF e.ev = "left" THEN mem \ {e.who} ELSE mem \cup {e.who}
                             ELSE mem
             IN ApplyReplay(mem2, Tail(s), cursor)

(* The evicted recipient reconnects and receives the authoritative Reconnected  *)
(* snapshot. Membership is REPLACED by the current member set (upsert, not a     *)
(* delta replay) so a dropped PlayerLeft cannot strand a phantom member. Each    *)
(* per-sender (epoch, seq) baseline is RE-BASELINED from the snapshot            *)
(* watermarks (E5) so the next frame from every sender is contiguous. All        *)
(* pending brackets are cleared — the snapshot is the fresh baseline. The two    *)
(* buggy arms model an SDK that skips one heal.                                  *)
ClientReconnect(r) ==
    /\ conn[r] = "Dropped"
    /\ reconnects[r] < ReconnectBudget
    /\ conn' = [conn EXCEPT ![r] = "Connected"]
    /\ reconnects' = [reconnects EXCEPT ![r] = @ + 1]
    /\ clientMembers' =
           [clientMembers EXCEPT ![r] =
               IF NoSnapshotReconcileBug
                   THEN ApplyReplay(@, ring, snapSeq[r])
                   ELSE {s \in Senders : senderIn[s]}]
    /\ obsEpoch' =
           [obsEpoch EXCEPT ![r] =
               IF NoBaselineResetBug THEN @ ELSE [s \in Senders |-> epoch[s]]]
    /\ obsSeq' =
           [obsSeq EXCEPT ![r] =
               IF NoBaselineResetBug THEN @ ELSE [s \in Senders |-> counter[s]]]
    /\ justified' = [justified EXCEPT ![r] = [s \in Senders |-> FALSE]]
    /\ UNCHANGED <<senderIn, counter, epoch, sends, cycles, nextCtrlSeq, ring,
                   watermark, queue, sockBuf, snapSeq, accountable>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful (matches  *)
(* the house convention): all budgets spent, every sender seated (else a rejoin *)
(* is enabled), every connected recipient drained, every dropped recipient out  *)
(* of reconnects.                                                               *)
AllResolved ==
    /\ \A s \in Senders : senderIn[s]
    /\ sends = SendBudget
    /\ cycles = CycleBudget
    /\ \A r \in Recipients :
           \/ (conn[r] = "Connected" /\ queue[r] = <<>> /\ sockBuf[r] = <<>>)
           \/ (conn[r] = "Dropped" /\ reconnects[r] = ReconnectBudget)

Done ==
    /\ AllResolved
    /\ UNCHANGED vars

Next ==
    \/ \E s \in Senders : SendData(s) \/ SenderLeave(s) \/ SenderRejoin(s)
    \/ \E r \in Recipients :
           WriterDrain(r) \/ ClientObserve(r) \/ Evict(r) \/ ClientReconnect(r)
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ senderIn \in [Senders -> BOOLEAN]
    /\ counter \in [Senders -> 0..SendBudget]
    /\ epoch \in [Senders -> 1..MaxEpoch]
    /\ sends \in 0..SendBudget
    /\ cycles \in 0..CycleBudget
    /\ nextCtrlSeq \in 0..CtrlBudget
    /\ ring \in Seq(CtrlRec)
    /\ Len(ring) <= RINGCAP
    /\ watermark \in 0..CtrlBudget
    /\ conn \in [Recipients -> {"Connected", "Dropped"}]
    /\ queue \in [Recipients -> Seq(Msg)]
    /\ \A r \in Recipients : Len(queue[r]) <= QCAP
    /\ sockBuf \in [Recipients -> Seq(Msg)]
    /\ \A r \in Recipients : Len(sockBuf[r]) <= SOCKCAP
    /\ reconnects \in [Recipients -> 0..ReconnectBudget]
    /\ snapSeq \in [Recipients -> 0..CtrlBudget]
    /\ obsEpoch \in [Recipients -> [Senders -> 0..MaxEpoch]]
    /\ obsSeq \in [Recipients -> [Senders -> 0..SendBudget]]
    /\ justified \in [Recipients -> [Senders -> BOOLEAN]]
    /\ clientMembers \in [Recipients -> SUBSET Senders]
    /\ accountable \in [Recipients -> BOOLEAN]

(* THE flagship end-to-end invariant: driven only by what the server put on its *)
(* socket, no recipient is ever left holding a data discontinuity it cannot     *)
(* classify — every non-contiguity was bracketed for the RIGHT sender (a        *)
(* PlayerLeft/PlayerReconnected of that sender) or healed by the reconnection   *)
(* snapshot's per-sender watermark re-baseline.                                 *)
ClientCanClassify ==
    \A r \in Recipients : accountable[r]

(* Eviction is never partial and never lossy-out-of-order: a dropped recipient  *)
(* holds no queue AND no socket buffer — every un-observed frame died with the  *)
(* connection and cannot resurface later.                                       *)
DroppedNeverObserved ==
    \A r \in Recipients :
        conn[r] = "Dropped" => queue[r] = <<>> /\ sockBuf[r] = <<>>

(* Ring / snapshot soundness (Seam 1): retained control events are strictly     *)
(* ascending, everything evicted sits strictly below everything retained (the   *)
(* watermark bounds the retained tail from below), and no cursor or watermark   *)
(* ever runs ahead of the global counter that issued the sequences.             *)
RingSnapshotSound ==
    /\ \A i, j \in DOMAIN ring : i < j => ring[i].seq < ring[j].seq
    /\ (ring # <<>> => watermark < ring[1].seq)
    /\ watermark <= nextCtrlSeq
    /\ \A r \in Recipients : snapSeq[r] <= nextCtrlSeq

(* At quiescence the client's believed membership equals the room's actual      *)
(* membership for every still-connected recipient — the authoritative snapshot  *)
(* reconcile heals any PlayerLeft/PlayerReconnected the client missed during an  *)
(* outage. Encoded as a safety obligation on the terminal (stutter) states, the *)
(* house convention for an eventually-property under TLC. `AllResolved` forces  *)
(* every sender seated, so the RHS is the full `Senders` set and the checked    *)
(* direction is the client UNDER-counting (still believing a now-seated sender  *)
(* left — the dropped-PlayerReconnected defect `NoSnapshotReconcileBug` seeds);  *)
(* the reconcile heals the phantom direction too, but no terminal state has a   *)
(* departed sender for it to be observed against.                               *)
MembershipEventuallyHonest ==
    AllResolved =>
        \A r \in Recipients :
            conn[r] = "Connected" => clientMembers[r] = {s \in Senders : senderIn[s]}

=============================================================================

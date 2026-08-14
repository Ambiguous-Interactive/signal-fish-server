--------------------------- MODULE ControlPriorityDelivery ---------------------------
(***************************************************************************)
(* Spec-FIRST for the protocol-v3 delivery revision (the design is          *)
(* not yet implemented — this module is merged BEFORE the code and pins the *)
(* two properties the queue split must satisfy):                            *)
(*                                                                         *)
(*   1. CONTROL PRIORITY. Control frames (PlayerJoined/Left/Reconnected,    *)
(*      SessionPlan, errors) ride a SEPARATE per-connection queue drained   *)
(*      STRICTLY BEFORE the data queue, so a control frame is never stuck   *)
(*      behind a data backlog. (`websocket.control_queue_capacity`.)        *)
(*   2. SOJOURN EVICTION. A frame whose AGE (time since enqueue) exceeds    *)
(*      `websocket.max_sojourn_ms` closes the connection ("Stale"),         *)
(*      which is what makes "enqueued ~> written or closed" TRUE even       *)
(*      against a peer that pings (so the activity reaper is happy) but      *)
(*      never READS (so the writer never drains) — the limbo a plain        *)
(*      backpressure queue cannot escape.                                   *)
(*                                                                         *)
(* This COMPOSES WITH DeliveryContract.tla (the #131 deliver-or-disconnect  *)
(* substrate: bounded queue, backpressure, grace expiry, conservation). It  *)
(* does NOT re-derive the data slow-consumer grace close (that is           *)
(* DeliveryContract's subject); it adds the control/data split and the      *)
(* sojourn clock on top. The consumer (the socket writer) is deliberately   *)
(* UNFAIR — the contract must hold against a peer that never reads.         *)
(*                                                                         *)
(* TIME is discrete and tracked as per-frame AGE (ticks since enqueue),     *)
(* the discrete-time house rule with sojourn as an absolute-age guard.      *)
(* There is no separate wall clock: ages are bounded by SojournBound and    *)
(* sends by the budgets, so the state space is finite without one, and a    *)
(* frame can always age to the bound (a wall-clock horizon would falsely    *)
(* strand a late-enqueued frame below the bound and break the liveness).    *)
(* `Tick` ages every queued frame one step, capped so no head exceeds the   *)
(* bound.                                                                   *)
(*                                                                         *)
(* Messages are modeled by CLASS (data | ctrl) rather than by sender: the   *)
(* recipient's two queues do not distinguish senders, and per-class         *)
(* conservation is the contract.                                            *)
(*                                                                         *)
(* Mapping to the (planned) implementation:                                 *)
(*   SendData          a `reliable` data frame enqueued on the data queue    *)
(*                     (backpressure = the send waits while the queue is     *)
(*                     full — the DeliveryContract park, abstracted)         *)
(*   SendCtrl          a control frame enqueued on the control queue         *)
(*                     (SingleQueueBug misroutes it onto the DATA queue —    *)
(*                     the pre-split behavior)                               *)
(*   WriterDrainCtrl   the send task draining the CONTROL queue head — done  *)
(*   First             strictly before any data (UNFAIR: a stalled peer      *)
(*                     means TLC never schedules it)                         *)
(*   Tick              one unit of wall time; every queued frame ages one    *)
(*                     tick (capped at SojournBound)                         *)
(*   SojournEvict      the oldest head aged to `max_sojourn_ms`: close       *)
(*                     "Stale" (WF — this is the liveness guarantee;         *)
(*                     NoSojournEvictionBug disables it)                     *)
(*   LifecycleClose    any other eviction requesting the close (reaper /     *)
(*                     idle timeout)                                         *)
(*   ShutdownClose     E3 drain close: may supersede Stale/Lifecycle and is  *)
(*                     then terminally stable                                *)
(*   CloseFinish       teardown: every still-queued frame is counted         *)
(*                     dropped against the closing connection (per class)    *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible), two seeded  *)
(* bugs, each pinned FALSE in the checked config:                           *)
(*                                                                         *)
(*   - SingleQueueBug = TRUE routes control onto the DATA FIFO (no priority  *)
(*     queue). A control frame enqueued behind data is then written after    *)
(*     it, so TLC latches the `ctrlDelayedByData` ghost and reports a        *)
(*     `ControlAgeBounded` violation. Minimal shape: SendData -> SendCtrl    *)
(*     (both onto dataQ) -> WriterDrainCtrlFirst writes the data head while  *)
(*     the ctrl frame waits behind it -> ControlAgeBounded FALSE.           *)
(*   - NoSojournEvictionBug = TRUE disables SojournEvict. A frame enqueued   *)
(*     for a peer that never drains (WriterDrain is unfair) then ages to the *)
(*     bound and STAYS — no writer, no sojourn close — so TLC reports a      *)
(*     `DeliveryEventuallyResolves` (temporal) violation: the non-empty      *)
(*     queue never empties.                                                  *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    DataBudget,          \* data frames offered over the run (keep tiny: 2)
    CtrlBudget,          \* control frames offered over the run (keep tiny: 1)
    DataCap,             \* data-queue capacity (keep tiny: 2)
    CtrlCap,             \* control-queue capacity (keep tiny: 1)
    SojournBound,        \* max head age before a Stale close (keep tiny: 2)
    SingleQueueBug,      \* TRUE misroutes control onto the data FIFO (must
                         \* violate ControlAgeBounded; FALSE in checked cfgs)
    NoSojournEvictionBug \* TRUE disables sojourn eviction (must violate the
                         \* DeliveryEventuallyResolves liveness; FALSE checked)

ASSUME /\ DataBudget \in Nat \ {0}
       /\ CtrlBudget \in Nat \ {0}
       /\ DataCap \in Nat \ {0}
       /\ CtrlCap \in Nat \ {0}
       /\ SojournBound \in Nat \ {0}
       /\ SingleQueueBug \in BOOLEAN
       /\ NoSojournEvictionBug \in BOOLEAN

Class == {"data", "ctrl"}
Entry == [class : Class, age : 0..SojournBound]

VARIABLES
    dataQ,          \* Seq(Entry): the data queue (holds ctrl too under the bug)
    ctrlQ,          \* Seq(Entry): the priority control queue
    sentD,          \* data frames offered
    sentC,          \* control frames offered
    writtenD,       \* data frames written to the socket
    writtenC,       \* control frames written to the socket
    droppedD,       \* data frames abandoned with a closing connection
    droppedC,       \* control frames abandoned with a closing connection
    connState,      \* "Open" | "CloseRequested" | "Closed"
    closeReason,    \* "None" | "Stale" | "Lifecycle" | "Shutdown"
    shutdownRequested, \* E3 process drain has requested priority close
    ctrlDelayedByData \* ghost: latches TRUE if data is written while a control
                      \* frame is pending — the starvation ControlAgeBounded forbids

vars == <<dataQ, ctrlQ, sentD, sentC, writtenD, writtenC, droppedD,
          droppedC, connState, closeReason, shutdownRequested,
          ctrlDelayedByData>>

(* Count queued frames of a class across both queues (ctrlQ never holds data). *)
CountClass(q, c) == Len(SelectSeq(q, LAMBDA e: e.class = c))
DataInQueues == CountClass(dataQ, "data") + CountClass(ctrlQ, "data")
CtrlInQueues == CountClass(dataQ, "ctrl") + CountClass(ctrlQ, "ctrl")

(* Every age currently held, and the oldest of them (0 when both empty). The  *)
(* head of each FIFO queue is its oldest frame; the max over all frames is    *)
(* the global oldest. *)
AllAges == {dataQ[i].age : i \in DOMAIN dataQ} \cup {ctrlQ[i].age : i \in DOMAIN ctrlQ}
MaxAge == IF AllAges = {} THEN 0 ELSE CHOOSE a \in AllAges : \A b \in AllAges : b <= a

(* A control frame is pending anywhere OTHER than the data-queue head (the     *)
(* frame WriterDrainCtrlFirst is about to write): used to detect data written  *)
(* while control waits. *)
CtrlPendingBehindDataHead ==
    \/ ctrlQ # <<>>
    \/ \E i \in DOMAIN dataQ : i > 1 /\ dataQ[i].class = "ctrl"

(* Ordinary close reasons are first-wins; E3 Shutdown is the priority       *)
(* exception and supersedes an already-recorded lifecycle reason.           *)
RequestClose(reason) ==
    closeReason' =
        IF reason = "Shutdown"
          THEN "Shutdown"
          ELSE IF closeReason = "None" THEN reason ELSE closeReason

Init ==
    /\ dataQ = <<>>
    /\ ctrlQ = <<>>
    /\ sentD = 0
    /\ sentC = 0
    /\ writtenD = 0
    /\ writtenC = 0
    /\ droppedD = 0
    /\ droppedC = 0
    /\ connState = "Open"
    /\ closeReason = "None"
    /\ shutdownRequested = FALSE
    /\ ctrlDelayedByData = FALSE

(* A reliable data frame is enqueued on the data queue. Backpressure is        *)
(* modeled by enablement: while the queue is full the send simply waits (the   *)
(* DeliveryContract park). Frames keep landing after a close is REQUESTED —    *)
(* CloseFinish accounts whatever never reaches the socket. *)
SendData ==
    /\ sentD < DataBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ Len(dataQ) < DataCap
    /\ dataQ' = Append(dataQ, [class |-> "data", age |-> 0])
    /\ sentD' = sentD + 1
    /\ UNCHANGED <<ctrlQ, sentC, writtenD, writtenC, droppedD, droppedC,
                   connState, closeReason, shutdownRequested,
                   ctrlDelayedByData>>

(* A control frame is enqueued. Normally onto the priority control queue;      *)
(* under SingleQueueBug onto the data FIFO instead (the pre-split behavior     *)
(* that lets control starve behind data). *)
SendCtrl ==
    /\ sentC < CtrlBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ IF SingleQueueBug
           THEN /\ Len(dataQ) < DataCap
                /\ dataQ' = Append(dataQ, [class |-> "ctrl", age |-> 0])
                /\ UNCHANGED ctrlQ
           ELSE /\ Len(ctrlQ) < CtrlCap
                /\ ctrlQ' = Append(ctrlQ, [class |-> "ctrl", age |-> 0])
                /\ UNCHANGED dataQ
    /\ sentC' = sentC + 1
    /\ UNCHANGED <<sentD, writtenD, writtenC, droppedD, droppedC,
                   connState, closeReason, shutdownRequested,
                   ctrlDelayedByData>>

(* The send task writes ONE frame, control queue strictly first. Deliberately  *)
(* UNFAIR (no fairness): a stalled peer means TLC may never schedule it. When  *)
(* it does drain a DATA-class frame while a control frame is still pending      *)
(* (only reachable under SingleQueueBug, where control shares the data FIFO),   *)
(* the ctrlDelayedByData ghost latches — the starvation the split prevents. *)
WriterDrainCtrlFirst ==
    /\ connState = "Open"
    /\ ctrlQ # <<>> \/ dataQ # <<>>
    /\ IF ctrlQ # <<>>
           THEN /\ writtenC' = writtenC + 1
                /\ ctrlQ' = Tail(ctrlQ)
                /\ UNCHANGED <<dataQ, writtenD, ctrlDelayedByData>>
           ELSE LET h == Head(dataQ)
                IN /\ dataQ' = Tail(dataQ)
                   /\ UNCHANGED ctrlQ
                   /\ IF h.class = "ctrl"
                          THEN /\ writtenC' = writtenC + 1
                               /\ UNCHANGED <<writtenD, ctrlDelayedByData>>
                          ELSE /\ writtenD' = writtenD + 1
                               /\ UNCHANGED writtenC
                               /\ ctrlDelayedByData' =
                                      (ctrlDelayedByData \/ CtrlPendingBehindDataHead)
    /\ UNCHANGED <<sentD, sentC, droppedD, droppedC, connState, closeReason,
                   shutdownRequested>>

(* The oldest head has aged to the sojourn bound: close "Stale" (the liveness  *)
(* guarantee). Disabled by NoSojournEvictionBug. *)
SojournEvict ==
    /\ ~NoSojournEvictionBug
    /\ connState = "Open"
    /\ MaxAge >= SojournBound
    /\ connState' = "CloseRequested"
    /\ RequestClose("Stale")
    /\ UNCHANGED <<dataQ, ctrlQ, sentD, sentC, writtenD, writtenC,
                   droppedD, droppedC, shutdownRequested, ctrlDelayedByData>>

(* A distinct non-sojourn close path (activity reaper, idle timeout). The two   *)
(* ordinary close actions cannot contest because both require Open; the         *)
(* ShutdownClose action below can intentionally supersede either while          *)
(* teardown remains pending.                                                   *)
LifecycleClose ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<dataQ, ctrlQ, sentD, sentC, writtenD, writtenC,
                   droppedD, droppedC, shutdownRequested, ctrlDelayedByData>>

(* E3 priority exception: process drain upgrades an earlier lifecycle close *)
(* while teardown is still pending. Once selected, Shutdown cannot change.  *)
ShutdownClose ==
    /\ connState \in {"Open", "CloseRequested"}
    /\ closeReason # "Shutdown"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Shutdown")
    /\ shutdownRequested' = TRUE
    /\ UNCHANGED <<dataQ, ctrlQ, sentD, sentC, writtenD, writtenC,
                   droppedD, droppedC, ctrlDelayedByData>>

(* Teardown completes: every still-queued frame is counted dropped against     *)
(* the closing connection, per class, and the queues are emptied.             *)
CloseFinish ==
    /\ connState = "CloseRequested"
    /\ connState' = "Closed"
    /\ droppedD' = droppedD + DataInQueues
    /\ droppedC' = droppedC + CtrlInQueues
    /\ dataQ' = <<>>
    /\ ctrlQ' = <<>>
    /\ UNCHANGED <<sentD, sentC, writtenD, writtenC, closeReason,
                   shutdownRequested, ctrlDelayedByData>>

(* One unit of wall time; every queued frame ages one tick. Enabled only with  *)
(* a frame to age and while open, and capped so the oldest head never exceeds  *)
(* SojournBound (SojournEvict becomes enabled AT the bound, before any frame   *)
(* ages past it — so StalenessBounded is a safety invariant, not merely        *)
(* eventual). *)
AgeOne(q) == [i \in DOMAIN q |-> [q[i] EXCEPT !.age = @ + 1]]
Tick ==
    /\ connState = "Open"
    /\ dataQ # <<>> \/ ctrlQ # <<>>
    /\ MaxAge < SojournBound
    /\ dataQ' = AgeOne(dataQ)
    /\ ctrlQ' = AgeOne(ctrlQ)
    /\ UNCHANGED <<sentD, sentC, writtenD, writtenC, droppedD, droppedC,
                   connState, closeReason, shutdownRequested,
                   ctrlDelayedByData>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful: every   *)
(* budget spent and either both queues drained or the connection closed. Under *)
(* NoSojournEvictionBug the liveness violation is a fair STUTTER at a stuck     *)
(* non-empty-queue state (WriterDrain is enabled but UNFAIR, so a behavior may  *)
(* never take it, and Tick/SojournEvict are then disabled so their WF is        *)
(* vacuous); Done covers the resolved terminal only, so that stuck state is not *)
(* masked here. *)
AllResolved ==
    \/ connState = "Closed"   \* a closed connection accepts no more sends — terminal
    \/ (sentD = DataBudget /\ sentC = CtrlBudget /\ dataQ = <<>> /\ ctrlQ = <<>>)

Done ==
    /\ AllResolved
    /\ UNCHANGED vars

Next ==
    \/ SendData
    \/ SendCtrl
    \/ WriterDrainCtrlFirst
    \/ SojournEvict
    \/ LifecycleClose
    \/ ShutdownClose
    \/ CloseFinish
    \/ Tick
    \/ Done

(* Fairness: ONLY the contract's own guarantees — sojourn eviction always       *)
(* eventually fires for an aged head, a requested close always completes, and   *)
(* frames actually age (Tick). The WRITER (the peer draining) gets NO fairness: *)
(* the whole point is that delivery still resolves against a peer that never     *)
(* reads. *)
Fairness ==
    /\ WF_vars(Tick)
    /\ WF_vars(SojournEvict)
    /\ WF_vars(CloseFinish)

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ dataQ \in Seq(Entry)
    /\ ctrlQ \in Seq(Entry)
    /\ Len(dataQ) <= DataCap
    /\ Len(ctrlQ) <= CtrlCap
    /\ sentD \in 0..DataBudget
    /\ sentC \in 0..CtrlBudget
    /\ connState \in {"Open", "CloseRequested", "Closed"}
    /\ closeReason \in {"None", "Stale", "Lifecycle", "Shutdown"}
    /\ shutdownRequested \in BOOLEAN
    /\ ctrlDelayedByData \in BOOLEAN

(* PER-CLASS CONSERVATION: every offered frame of each class is, at every       *)
(* reachable state, queued, written, or dropped with a closing connection —      *)
(* never silently lost, and the two classes are accounted independently.        *)
PerClassConservation ==
    /\ sentD = DataInQueues + writtenD + droppedD
    /\ sentC = CtrlInQueues + writtenC + droppedC

(* Control drops are LOUD: control is abandoned only on the close path.          *)
CtrlDropsAreLoud ==
    droppedC > 0 => (connState \in {"CloseRequested", "Closed"} /\ closeReason # "None")

(* CONTROL PRIORITY: control is never starved behind a data backlog — no data   *)
(* frame is ever written while a control frame is still pending. A theorem of    *)
(* the two-queue split; SingleQueueBug (control on the data FIFO) breaks it.     *)
ControlAgeBounded ==
    ~ctrlDelayedByData

(* STALENESS BOUNDED: while the connection is open, no queued frame has aged     *)
(* past the sojourn bound — the sojourn close fires AT the bound (safety, given  *)
(* the Tick cap), not merely eventually.                                        *)
StalenessBounded ==
    connState = "Open" => MaxAge <= SojournBound

(* Once the E3 drain request is represented, the priority reason must have  *)
(* been installed in the same transition.                                  *)
ShutdownWins == shutdownRequested => closeReason = "Shutdown"

----------------------------------------------------------------------------
(* Temporal *)

(* E3 close priority: an ordinary reason may only remain itself or upgrade to *)
(* Shutdown; Shutdown itself is terminally stable.                            *)
ShutdownPriorityStable ==
    [][ /\ closeReason = "Shutdown" => closeReason' = "Shutdown"
        /\ closeReason \in {"Stale", "Lifecycle"} =>
             closeReason' \in {closeReason, "Shutdown"} ]_vars

(* THE sojourn liveness: a non-empty queue never stays non-empty forever — it    *)
(* drains, or (against a peer that never reads) the sojourn close fires and       *)
(* teardown empties it. Holds WITHOUT any writer fairness; NoSojournEvictionBug   *)
(* is exactly what makes it FALSE.                                              *)
DeliveryEventuallyResolves ==
    (dataQ # <<>> \/ ctrlQ # <<>>) ~> (dataQ = <<>> /\ ctrlQ = <<>>)

=============================================================================

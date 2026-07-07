------------------------------ MODULE DeliveryClasses ------------------------------
(***************************************************************************)
(* Spec-FIRST for the protocol-v3 P10.E2 delivery-classes revision (the     *)
(* design is NOT yet implemented — this module is merged BEFORE the code and *)
(* pins the class-accounting contract the queue split must satisfy). It is   *)
(* the wire-contract twin of `ServerMessage::DeliveryReport`. It is a         *)
(* STANDALONE module — CONSISTENT WITH / analogous to the #131                *)
(* `DeliveryContract.tla` substrate (bounded queue, backpressure,             *)
(* deliver-or-disconnect) and the `ControlPriorityDelivery.tla` control/data  *)
(* split, but with NO formal refinement mapping to them; it re-uses their     *)
(* conventions rather than importing their state.                            *)
(*                                                                         *)
(* A single recipient's per-connection outbound DATA queue carrying three    *)
(* delivery CLASSES. Messages are modeled by [class, key, id] (NO senders —  *)
(* the recipient's one queue does not distinguish them); ids are GLOBALLY     *)
(* UNIQUE and issued monotonically (checked by `UniqueIds`), so a frame can   *)
(* be tracked through every disposition and the cardinality-based             *)
(* conservation laws are sound. Control frames ride a SEPARATE queue (that    *)
(* split, and its sojourn liveness, are `ControlPriorityDelivery.tla`'s       *)
(* subject) and are therefore OUT of this data-queue model —                 *)
(* `CoalesceNeverTouchesReliable` is the reliable-side half of E2's           *)
(* "coalescing never touches control OR reliable" (control being             *)
(* structurally unreachable here, on its own queue).                         *)
(*                                                                         *)
(* CLASS semantics (PLAN.md E2 / Appendix O.5):                             *)
(*   reliable  today's semantics. BACKPRESSURES while the queue is full      *)
(*             (modeled as ENABLEMENT: the send simply waits — the #131      *)
(*             park/grace/4002 escalation is DeliveryContract's subject and  *)
(*             is abstracted away here). It is the ONLY class that parks the *)
(*             sender. Leaves the queue ONLY by being written or             *)
(*             dropped-with-a-closing-connection. NEVER coalesced (never     *)
(*             superseded, volatile-dropped, or latest-dropped). Conserves.  *)
(*   latest    keyed current-value. NEVER parks the sender. Sending a latest *)
(*             frame for key k SUPERSEDES the undelivered same-key latest     *)
(*             already queued: the old id moves to `superseded` (counted) and *)
(*             the successor replaces it IN PLACE (same slot, no growth — a   *)
(*             hot key never grows the queue). The supersession is accounted  *)
(*             INLINE into the ledger `reported`, atomically with the         *)
(*             coalesce. At most ONE queued latest per key. A brand-new key   *)
(*             enqueues on free space; on a FULL queue it still never parks — *)
(*             it drops the oldest VOLATILE to make room (best-effort), or if *)
(*             none exists drops the ARRIVING latest into `latDropped`        *)
(*             (reliable and other-key latest are never displaced).          *)
(*   volatile  best-effort. NEVER parks. Enqueue if space, else              *)
(*             DROP-OLDEST-VOLATILE (counted); if the full queue holds no     *)
(*             volatile to evict, the ARRIVING volatile is dropped (reliable  *)
(*             and latest are never displaced).                              *)
(*                                                                         *)
(* ACCOUNTING — `reported` is the supersession LEDGER, not a scalar:          *)
(*   `reported` is the server's global SET of superseded ids it has accounted *)
(*   (the coalesce records the predecessor's id into it, in place, at         *)
(*   supersede time). `AccountedSupersession` (superseded \subseteq reported) *)
(*   is the property it pins — coalescing is NEVER SILENT. Because the ledger *)
(*   write is atomic with the coalesce, there is NO window, so this is an     *)
(*   ALWAYS invariant (stronger than O.5's at-quiescence phrasing — the "or   *)
(*   always, if you can" it invites).                                        *)
(*                                                                         *)
(*   DELIBERATELY OUT OF SCOPE: the per-successor `supersedes_from` SCALAR    *)
(*   (the lowest superseded id a single successor frame carries, so a client  *)
(*   accepts the range [supersedes_from, seq)) and the client-side gap        *)
(*   RANGE-classification it drives. That is the per-sender WATERMARK concern  *)
(*   (E5 `sender_watermarks` / E6 accountability-rules rewrite), to be modeled  *)
(*   by the D4 flagship `EndToEndGapAccountability.tla` (Appendix O.4, not yet   *)
(*   built) — NOT here. This module verifies LEDGER COMPLETENESS only —        *)
(*   that every coalesced-away id is accounted somewhere — NOT the per-frame  *)
(*   scalar arithmetic. `reported` is a SET; `supersedes_from` is a scalar;   *)
(*   they are different objects and only the former is checked here.          *)
(*                                                                         *)
(*   `lastReport` is the separate OUT-OF-BAND `ServerMessage::DeliveryReport`  *)
(*   cumulative per-class counter snapshot; `ReportHonest` pins that it never  *)
(*   overstates the true monotone counts.                                    *)
(*                                                                         *)
(* Mapping to the (planned) implementation:                                 *)
(*   SendReliable      the `reliable` arm of `deliver_or_disconnect`          *)
(*                     (`src/coordination/mod.rs`); backpressure = the send   *)
(*                     waits while the data queue is full (the #131 park,     *)
(*                     abstracted to enablement)                             *)
(*   SendLatest(k)     the `latest` keyed arm: in-queue same-key replace, old *)
(*                     id -> superseded, ledgered into `reported`; new-key    *)
(*                     never parks (drop-oldest-volatile or drop the arrival) *)
(*                     (`src/coordination/mod.rs` + `src/websocket/connection`*)
(*                     `.rs` send task)                                       *)
(*   SendBestEffort    the `volatile` arm: enqueue-or-drop-oldest-volatile    *)
(*   WriterDrain       the send task writing one queued data frame to the     *)
(*                     socket — deliberately UNFAIR (a stalled peer means TLC *)
(*                     may never schedule it)                                 *)
(*   EmitDeliveryReport  `ServerMessage::DeliveryReport` — the out-of-band    *)
(*                     per-class CUMULATIVE counter snapshot (`src/metrics.rs`*)
(*                     + connection)                                          *)
(*   CloseStale       sojourn eviction (`websocket.max_sojourn_ms`) — the     *)
(*                     pings-but-never-reads close (D2/E2 liveness path)      *)
(*   CloseLifecycle   any other eviction requesting the close (reaper / idle  *)
(*                     timeout)                                               *)
(*   CloseFinish      teardown: every still-queued frame counted dropped      *)
(*                     against the closing connection, per class              *)
(*                     (`finalize_closed_connection`)                        *)
(*   Done             explicit terminal stutter (deadlock checking stays ON)  *)
(*                                                                         *)
(* The consumer (the socket writer) is deliberately UNFAIR and no fairness   *)
(* is asserted: these are SAFETY (accounting) contracts, checked over every  *)
(* reachable state, plus the `ReasonStable` first-reason-wins action property.*)
(* Time / sojourn duration is NOT modeled here (that is                      *)
(* `ControlPriorityDelivery.tla`'s subject); `CloseStale` abstracts the        *)
(* sojourn-close trigger as a nondeterministic action.                       *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible), FOUR seeded  *)
(* bugs, each pinned FALSE in the checked config:                            *)
(*                                                                         *)
(*   - SilentSupersedeBug = TRUE makes the `latest` coalesce record the old   *)
(*     id into `superseded` WITHOUT ledgering it into `reported`. Minimal     *)
(*     trace:                                                                 *)
(*       SendLatest(k1)  [enqueue latest id 0]                                *)
(*       SendLatest(k1)  [supersede id 0 -> superseded = {id 0}, reported {}] *)
(*     -> `AccountedSupersession` violated at step 2.                        *)
(*   - CoalesceReliableBug = TRUE makes the volatile drop-oldest evict the    *)
(*     queue Head regardless of class, so a RELIABLE frame can be coalesced   *)
(*     away. Minimal trace (QCAP = 2):                                        *)
(*       SendReliable    [queue = <<r id 0>>]                                 *)
(*       Send*           [fill the queue: <<r id 0, x id 1>>]                 *)
(*       SendBestEffort  [full; the bug evicts Head (r id 0) into volDropped] *)
(*     -> `ReliableConservation` violated at step 3.                         *)
(*   - MisdropLatestBug = TRUE makes the volatile drop-oldest evict the       *)
(*     oldest LATEST frame into the VOLATILE bucket (a class misroute).       *)
(*     Minimal trace (QCAP = 2):                                             *)
(*       SendLatest(k1)  [queue = <<l id 0>>]                                 *)
(*       SendLatest(k2)  [queue = <<l id 0, l id 1>> — full]                  *)
(*       SendBestEffort  [full; the bug misdrops latest id 0 into volDropped] *)
(*     -> `LatestConservation` violated at step 3.                           *)
(*   - ReportOverstateBug = TRUE makes EmitDeliveryReport publish a supersede *)
(*     count ABOVE the true one. Minimal trace:                              *)
(*       SendLatest(k1)  [enqueue latest id 0]                                *)
(*       SendLatest(k1)  [supersede -> superseded count 1]                    *)
(*       EmitDeliveryReport [publishes sup = 2 > true 1]                      *)
(*     -> `ReportHonest` violated at step 3.                                 *)
(*                                                                         *)
(* All four constants are pinned FALSE in `DeliveryClasses_Small.cfg`; flip   *)
(* any one locally to watch TLC exhibit the counterexample.                  *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Keys,               \* nonempty set of latest-value keys (keep tiny: 2)
    RelBudget,          \* reliable frames offered over the run (keep tiny: 1)
    LatBudget,          \* latest frames offered over the run   (keep tiny: 3 —
                        \* so a 2-level same-key chain id0->id1->id2 is reachable)
    BeBudget,           \* volatile frames offered over the run (keep tiny: 2)
    QCAP,               \* bounded data-queue capacity          (keep tiny: 2)
    SilentSupersedeBug, \* TRUE coalesces without ledgering (must violate
                        \* AccountedSupersession; FALSE in checked cfgs)
    CoalesceReliableBug,\* TRUE lets the volatile drop evict a reliable frame
                        \* (must violate ReliableConservation; FALSE checked)
    MisdropLatestBug,   \* TRUE misdrops a latest into the volatile bucket
                        \* (must violate LatestConservation; FALSE checked)
    ReportOverstateBug  \* TRUE publishes a report above the true counts
                        \* (must violate ReportHonest; FALSE checked)

ASSUME /\ Keys # {}
       /\ "nokey" \notin Keys
       /\ RelBudget \in Nat \ {0}
       /\ LatBudget \in Nat \ {0}
       /\ BeBudget  \in Nat \ {0}
       /\ QCAP      \in Nat \ {0}
       /\ SilentSupersedeBug  \in BOOLEAN
       /\ CoalesceReliableBug \in BOOLEAN
       /\ MisdropLatestBug    \in BOOLEAN
       /\ ReportOverstateBug  \in BOOLEAN

Class == {"reliable", "latest", "volatile"}
MaxId == RelBudget + LatBudget + BeBudget          \* total frames offered; ids 0..MaxId-1
KeyDom == Keys \cup {"nokey"}                       \* "nokey" tags non-latest frames
Entry == [class : Class, key : KeyDom, id : 0..(MaxId - 1)]

VARIABLES
    queue,            \* Seq(Entry): the recipient's bounded outbound data queue
    written,          \* SUBSET Entry: frames drained to the socket
    superseded,       \* SUBSET Entry: latest frames coalesced away by a same-key successor
    volDropped,       \* SUBSET Entry: volatile frames dropped best-effort (drop-oldest)
    latDropped,       \* SUBSET Entry: latest frames dropped best-effort (full, no volatile)
    droppedWithClose, \* SUBSET Entry: frames abandoned with a closing connection (per class)
    reported,         \* SUBSET (0..MaxId-1): supersession LEDGER (accounted superseded ids)
    relSent,          \* Nat: reliable frames offered
    latSent,          \* Nat: latest frames offered
    volSent,          \* Nat: volatile frames offered
    connState,        \* "Open" | "CloseRequested" | "Closed"
    closeReason,      \* "None" | "Stale" | "Lifecycle" (first wins)
    lastReport        \* [sup, vol, lat, closed]: last emitted DeliveryReport snapshot

vars == <<queue, written, superseded, volDropped, latDropped, droppedWithClose,
          reported, relSent, latSent, volSent, connState, closeReason, lastReport>>

(* The set of frames currently sitting in the queue (ids are unique, so the   *)
(* set loses nothing). *)
QueueSet == {queue[i] : i \in DOMAIN queue}

(* Every frame the run has ever admitted lives in exactly one of these. *)
AllEntries ==
    QueueSet \cup written \cup superseded \cup volDropped \cup latDropped
             \cup droppedWithClose

(* Class projections of a set of entries. *)
RelIn(S) == {e \in S : e.class = "reliable"}
LatIn(S) == {e \in S : e.class = "latest"}
VolIn(S) == {e \in S : e.class = "volatile"}

(* The id assigned to the next admitted frame — the running total of offers,  *)
(* so ids are unique and monotone. *)
NextId == relSent + latSent + volSent

(* Cumulative per-class coalesce/drop counters the DeliveryReport publishes. *)
CurSup    == Cardinality(superseded)
CurVol    == Cardinality(volDropped)
CurLat    == Cardinality(latDropped)
CurClosed == Cardinality(droppedWithClose)

(* Is a latest frame for key k currently queued? (At most one — an invariant.) *)
HasLatest(k) == \E i \in DOMAIN queue : queue[i].class = "latest" /\ queue[i].key = k
LatestIdx(k) == CHOOSE i \in DOMAIN queue : queue[i].class = "latest" /\ queue[i].key = k

(* Is any volatile queued, and the index of the oldest (lowest) one? *)
HasVol == \E i \in DOMAIN queue : queue[i].class = "volatile"
OldestVolIdx ==
    CHOOSE i \in DOMAIN queue :
        /\ queue[i].class = "volatile"
        /\ \A j \in DOMAIN queue : queue[j].class = "volatile" => i <= j

(* Is any latest queued, and the index of the oldest (lowest) one? *)
HasLat == \E i \in DOMAIN queue : queue[i].class = "latest"
OldestLatIdx ==
    CHOOSE i \in DOMAIN queue :
        /\ queue[i].class = "latest"
        /\ \A j \in DOMAIN queue : queue[j].class = "latest" => i <= j

(* Drop the element at index i from a sequence (order-preserving). *)
Remove(seq, i) == SubSeq(seq, 1, i - 1) \o SubSeq(seq, i + 1, Len(seq))

(* First close reason wins — later requests must not overwrite it. *)
RequestClose(reason) ==
    closeReason' = (IF closeReason = "None" THEN reason ELSE closeReason)

Init ==
    /\ queue = <<>>
    /\ written = {}
    /\ superseded = {}
    /\ volDropped = {}
    /\ latDropped = {}
    /\ droppedWithClose = {}
    /\ reported = {}
    /\ relSent = 0
    /\ latSent = 0
    /\ volSent = 0
    /\ connState = "Open"
    /\ closeReason = "None"
    /\ lastReport = [sup |-> 0, vol |-> 0, lat |-> 0, closed |-> 0]

(* A reliable frame is admitted. BACKPRESSURE is modeled by enablement: while *)
(* the queue is full the send simply waits (the #131 park, abstracted).       *)
(* Reliable is the ONLY class that parks. Frames keep landing after a close is *)
(* REQUESTED — CloseFinish accounts whatever never reaches the socket. *)
SendReliable ==
    /\ relSent < RelBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ Len(queue) < QCAP
    /\ queue' = Append(queue, [class |-> "reliable", key |-> "nokey", id |-> NextId])
    /\ relSent' = relSent + 1
    /\ UNCHANGED <<written, superseded, volDropped, latDropped, droppedWithClose,
                   reported, latSent, volSent, connState, closeReason, lastReport>>

(* A latest frame for key k. If a same-key latest is queued, SUPERSEDE it in  *)
(* place (no growth — never parks for a hot key): the old id moves to          *)
(* `superseded` and is LEDGERED into `reported` (SilentSupersedeBug drops the  *)
(* ledger write). A brand-new key NEVER parks either: enqueue on free space,   *)
(* else drop the oldest volatile to make room, else drop the ARRIVING latest   *)
(* into `latDropped` — reliable and other-key latest are never displaced. *)
SendLatest(k) ==
    /\ latSent < LatBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ latSent' = latSent + 1
    /\ UNCHANGED <<written, droppedWithClose, relSent, volSent, connState,
                   closeReason, lastReport>>
    /\ LET new == [class |-> "latest", key |-> k, id |-> NextId] IN
         IF HasLatest(k)
           THEN LET i == LatestIdx(k)
                    old == queue[i]
                IN /\ queue' = [queue EXCEPT ![i] = new]
                   /\ superseded' = (superseded \cup {old})
                   /\ reported' = (IF SilentSupersedeBug
                                     THEN reported
                                     ELSE (reported \cup {old.id}))
                   /\ UNCHANGED <<volDropped, latDropped>>
           ELSE IF Len(queue) < QCAP
                  THEN /\ queue' = Append(queue, new)
                       /\ UNCHANGED <<superseded, reported, volDropped, latDropped>>
                  ELSE IF HasVol
                         THEN LET j == OldestVolIdx IN
                                /\ queue' = Append(Remove(queue, j), new)
                                /\ volDropped' = (volDropped \cup {queue[j]})
                                /\ UNCHANGED <<superseded, reported, latDropped>>
                         ELSE /\ queue' = queue
                              /\ latDropped' = (latDropped \cup {new})
                              /\ UNCHANGED <<superseded, reported, volDropped>>

(* A volatile frame. Enqueue if space; else drop-oldest-VOLATILE and enqueue   *)
(* (or, if the full queue holds no volatile, drop the ARRIVING one — reliable  *)
(* and latest are never displaced). CoalesceReliableBug instead evicts the     *)
(* queue Head regardless of class (so a reliable can be dropped); MisdropLatest*)
(* Bug instead evicts the oldest LATEST into the volatile bucket. *)
SendBestEffort ==
    /\ volSent < BeBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ volSent' = volSent + 1
    /\ UNCHANGED <<written, superseded, reported, latDropped, droppedWithClose,
                   relSent, latSent, connState, closeReason, lastReport>>
    /\ LET new == [class |-> "volatile", key |-> "nokey", id |-> NextId] IN
         IF Len(queue) < QCAP
           THEN /\ queue' = Append(queue, new)
                /\ UNCHANGED volDropped
           ELSE IF CoalesceReliableBug
                  THEN /\ queue' = Append(Tail(queue), new)
                       /\ volDropped' = (volDropped \cup {Head(queue)})
                  ELSE IF (MisdropLatestBug /\ HasLat)
                         THEN LET j == OldestLatIdx IN
                                /\ queue' = Append(Remove(queue, j), new)
                                /\ volDropped' = (volDropped \cup {queue[j]})
                         ELSE IF HasVol
                                THEN LET j == OldestVolIdx IN
                                       /\ queue' = Append(Remove(queue, j), new)
                                       /\ volDropped' = (volDropped \cup {queue[j]})
                                ELSE /\ queue' = queue
                                     /\ volDropped' = (volDropped \cup {new})

(* The send task writes one queued frame to the socket. Deliberately UNFAIR:  *)
(* a stalled peer means TLC never schedules this. *)
WriterDrain ==
    /\ connState = "Open"
    /\ queue # <<>>
    /\ written' = (written \cup {Head(queue)})
    /\ queue' = Tail(queue)
    /\ UNCHANGED <<superseded, volDropped, latDropped, droppedWithClose, reported,
                   relSent, latSent, volSent, connState, closeReason, lastReport>>

(* ServerMessage::DeliveryReport: publish the CUMULATIVE per-class counters.   *)
(* Out-of-band and observational (the in-band ledger `reported` already        *)
(* accounts every coalesce). Enabled only when a counter has advanced beyond   *)
(* the last snapshot, so it always makes progress — never a self-loop.         *)
(* ReportOverstateBug inflates the published supersede count above the truth.  *)
EmitDeliveryReport ==
    /\ connState \in {"Open", "CloseRequested"}
    /\ CurSup + CurVol + CurLat + CurClosed
         > lastReport.sup + lastReport.vol + lastReport.lat + lastReport.closed
    /\ lastReport' = (IF ReportOverstateBug
                        THEN [sup |-> CurSup + 1, vol |-> CurVol,
                              lat |-> CurLat, closed |-> CurClosed]
                        ELSE [sup |-> CurSup, vol |-> CurVol,
                              lat |-> CurLat, closed |-> CurClosed])
    /\ UNCHANGED <<queue, written, superseded, volDropped, latDropped,
                   droppedWithClose, reported, relSent, latSent, volSent,
                   connState, closeReason>>

(* Sojourn eviction (`websocket.max_sojourn_ms`): close "Stale". The trigger   *)
(* time is abstracted — this nondeterministic action stands in for the         *)
(* pings-but-never-reads sojourn close (whose clock is ControlPriorityDelivery's*)
(* subject). *)
CloseStale ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Stale")
    /\ UNCHANGED <<queue, written, superseded, volDropped, latDropped,
                   droppedWithClose, reported, relSent, latSent, volSent, lastReport>>

(* A distinct non-sojourn close path (activity reaper, idle timeout). First-   *)
(* reason-wins is preserved by RequestClose (the house pattern), though in     *)
(* this abstraction the two close actions cannot actually contest it — both    *)
(* require connState = "Open", which is left one-way, so the reason is set     *)
(* exactly once. *)
CloseLifecycle ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<queue, written, superseded, volDropped, latDropped,
                   droppedWithClose, reported, relSent, latSent, volSent, lastReport>>

(* Teardown completes: every still-queued frame is counted dropped against the *)
(* closing connection (per class — reliable lands in the legal droppedWithClose*)
(* bucket), and the queue is emptied. *)
CloseFinish ==
    /\ connState = "CloseRequested"
    /\ connState' = "Closed"
    /\ droppedWithClose' = (droppedWithClose \cup QueueSet)
    /\ queue' = <<>>
    /\ UNCHANGED <<written, superseded, volDropped, latDropped, reported,
                   relSent, latSent, volSent, closeReason, lastReport>>

(* Explicit terminal stutter so TLC's deadlock check stays ON without flagging *)
(* the natural terminals. Deadlock-freedom is otherwise STRUCTURAL here: while *)
(* "Open" the two close actions are always enabled, and while "CloseRequested" *)
(* CloseFinish is — only the "Closed" terminal (which accepts no more sends)   *)
(* needs this stutter, so any OTHER deadlock TLC reports is a real modeling    *)
(* bug. *)
AllResolved ==
    \/ connState = "Closed"
    \/ (relSent = RelBudget /\ latSent = LatBudget /\ volSent = BeBudget /\ queue = <<>>)

Done ==
    /\ AllResolved
    /\ UNCHANGED vars

Next ==
    \/ SendReliable
    \/ \E k \in Keys : SendLatest(k)
    \/ SendBestEffort
    \/ WriterDrain
    \/ EmitDeliveryReport
    \/ CloseStale
    \/ CloseLifecycle
    \/ CloseFinish
    \/ Done

(* No fairness: these are SAFETY (accounting) contracts, checked over every    *)
(* reachable state. The writer gets no fairness — the contract must hold        *)
(* against a peer that never reads. *)
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ queue \in Seq(Entry)
    /\ Len(queue) <= QCAP
    /\ written \subseteq Entry
    /\ superseded \subseteq Entry
    /\ volDropped \subseteq Entry
    /\ latDropped \subseteq Entry
    /\ droppedWithClose \subseteq Entry
    /\ reported \subseteq (0..(MaxId - 1))
    /\ relSent \in 0..RelBudget
    /\ latSent \in 0..LatBudget
    /\ volSent \in 0..BeBudget
    /\ connState \in {"Open", "CloseRequested", "Closed"}
    /\ closeReason \in {"None", "Stale", "Lifecycle"}
    /\ lastReport \in [sup : 0..LatBudget, vol : 0..BeBudget,
                       lat : 0..LatBudget, closed : 0..MaxId]
    \* only latest frames carry a real key; everything else is tagged "nokey"
    /\ \A e \in AllEntries : (e.class = "latest") <=> (e.key \in Keys)

(* Ids are GLOBALLY UNIQUE: no two distinct admitted frames share an id. This  *)
(* is what makes the cardinality-based conservation laws sound (a set built    *)
(* from the queue or a disposition bucket never collapses two frames). *)
UniqueIds ==
    \A e1 \in AllEntries : \A e2 \in AllEntries : (e1.id = e2.id) => (e1 = e2)

(* RELIABLE CONSERVATION: every reliable frame offered is, at every reachable  *)
(* state, queued OR written OR dropped-with-a-closing-connection — and NEVER   *)
(* coalesced (superseded, volatile-dropped, or latest-dropped). The count      *)
(* equation catches a reliable frame that leaked into a coalescing bucket (the *)
(* legal-bucket total falls short of relSent); the emptiness clauses name the  *)
(* illegal buckets directly. CoalesceReliableBug breaks it. *)
ReliableConservation ==
    /\ relSent = Cardinality(RelIn(QueueSet))
                 + Cardinality(RelIn(written))
                 + Cardinality(RelIn(droppedWithClose))
    /\ RelIn(superseded) = {}
    /\ RelIn(volDropped) = {}
    /\ RelIn(latDropped) = {}

(* The reliable-side half of E2's "coalescing never touches control OR         *)
(* reliable" (control rides its own queue — structurally unreachable here). A   *)
(* dedicated restatement of the illegal-bucket clauses; CoalesceReliableBug     *)
(* breaks it too. *)
CoalesceNeverTouchesReliable ==
    /\ RelIn(superseded) = {}
    /\ RelIn(volDropped) = {}
    /\ RelIn(latDropped) = {}

(* LATEST CONSERVATION: every latest frame offered is queued OR written OR      *)
(* superseded OR latest-dropped OR dropped-with-close — and NEVER misrouted     *)
(* into the volatile bucket. MisdropLatestBug (a latest evicted into volDropped)*)
(* breaks it. *)
LatestConservation ==
    /\ latSent = Cardinality(LatIn(QueueSet))
                 + Cardinality(LatIn(written))
                 + Cardinality(LatIn(superseded))
                 + Cardinality(LatIn(latDropped))
                 + Cardinality(LatIn(droppedWithClose))
    /\ LatIn(volDropped) = {}

(* VOLATILE CONSERVATION: every volatile frame offered is queued OR written OR  *)
(* volatile-dropped OR dropped-with-close — and NEVER superseded or            *)
(* latest-dropped (those coalescing buckets belong to the latest class). *)
VolatileConservation ==
    /\ volSent = Cardinality(VolIn(QueueSet))
                 + Cardinality(VolIn(written))
                 + Cardinality(VolIn(volDropped))
                 + Cardinality(VolIn(droppedWithClose))
    /\ VolIn(superseded) = {}
    /\ VolIn(latDropped) = {}

(* LATEST = LAST WRITE WINS (among the QUEUED representative vs superseded ∪     *)
(* written): at most one queued latest per key, and that queued frame's id is    *)
(* the maximum among all latest frames for the key seen so far (every            *)
(* superseded/written same-key latest has a strictly lower id — coalescing is    *)
(* monotone, an older value never displaces a newer one, across a chain). This   *)
(* is deliberately NOT an end-to-end "newest value always delivered" guarantee:   *)
(* it excludes `latDropped`, so a NEWER same-key latest dropped under queue       *)
(* pressure (latest never parks / never displaces reliable) can leave an OLDER    *)
(* written value the last delivered — accounted in `latDropped`, by design. *)
LatestValueLastWrite ==
    /\ \A k \in Keys :
         Cardinality({i \in DOMAIN queue :
                        queue[i].class = "latest" /\ queue[i].key = k}) <= 1
    /\ \A qi \in DOMAIN queue :
         queue[qi].class = "latest" =>
           \A e \in (superseded \cup written) :
             (e.class = "latest" /\ e.key = queue[qi].key) => e.id < queue[qi].id

(* ACCOUNTED SUPERSESSION: coalescing is never silent — every superseded id is  *)
(* in the ledger `reported`. Held ALWAYS (not merely at quiescence) because the *)
(* ledger write is atomic with the coalesce. SilentSupersedeBug breaks it. *)
AccountedSupersession ==
    \A e \in superseded : e.id \in reported

(* REPORT HONESTY: the published DeliveryReport never OVERSTATES the true        *)
(* cumulative counts (it is a snapshot of monotonically growing counters).      *)
(* ReportOverstateBug breaks it (via the `sup` conjunct). NOTE the `closed`      *)
(* conjunct is structurally INERT in this single-connection model: droppedWith   *)
(* Close is populated only at CloseFinish (-> Closed), after which no            *)
(* EmitDeliveryReport can fire (it is gated to {Open, CloseRequested}), so       *)
(* lastReport.closed is always 0. It is kept for contract symmetry; a            *)
(* terminal/multi-report abstraction would give it teeth. *)
ReportHonest ==
    /\ lastReport.sup    <= CurSup
    /\ lastReport.vol    <= CurVol
    /\ lastReport.lat    <= CurLat
    /\ lastReport.closed <= CurClosed

----------------------------------------------------------------------------
(* Temporal *)

(* First reason wins: once recorded, the close reason never changes. *)
ReasonStable == [][closeReason # "None" => closeReason' = closeReason]_vars

=============================================================================

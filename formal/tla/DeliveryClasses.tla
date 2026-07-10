------------------------------ MODULE DeliveryClasses ------------------------------
(***************************************************************************)
(* Spec-first model for the protocol-v3 P10.E2 delivery classes and their  *)
(* wire-accountability contract. One implicit sender in one fixed epoch     *)
(* relays to one recipient, so `seq` is that sender's GLOBAL monotone room  *)
(* sequence across reliable,                                                *)
(* keyed-latest, and volatile data. Two distinct latest keys are required:  *)
(* this is the smallest model that exposes the interaction between keyed   *)
(* coalescing and the global wire order.                                    *)
(*                                                                         *)
(* Class behavior:                                                         *)
(*   reliable  Enqueues only with capacity (the DeliveryContract park). It *)
(*             is never coalesced or best-effort dropped.                  *)
(*   latest    Replaces an undelivered same-key latest without parking. In *)
(*             the correct design the predecessor is REMOVED and the new  *)
(*             frame APPENDED, preserving global seq order among survivors.*)
(*             A new key uses free space, evicts only the oldest volatile, *)
(*             or drops the arrival when neither is possible.             *)
(*   volatile  Enqueues with space; otherwise evicts only the oldest       *)
(*             volatile, or drops the arrival. It never parks.             *)
(*                                                                         *)
(* Every supersession or best-effort drop atomically appends an exact       *)
(* DeliveryReport gap range to `reportQ`. `WriterDrainReport` has strict    *)
(* priority over `WriterDrainData`, so the client receives the explanation *)
(* before any later data frame can expose that gap. `accountedGaps` is the *)
(* durable server ledger; `reportedOnWire` is what the client has actually *)
(* received. Close-time abandonment is deliberately not a gap: the socket *)
(* closes loudly and no later data is delivered on that connection.        *)
(*                                                                         *)
(* The old scalar/in-place proposal is executable as ScalarInPlaceBug.     *)
(* When TRUE, the model forces the minimal prelude:                         *)
(*                                                                         *)
(*   SendLatest("A") -> A:seq1 queued                                      *)
(*   SendLatest("B") -> B:seq2 queued                                      *)
(*   SendLatest("A") -> A:seq3 replaces A:seq1 IN PLACE and reports the    *)
(*                       scalar interval [1,3) = 1..2                       *)
(*                                                                         *)
(* Seq 2 was not lost, so ExactGapAccounting fails immediately; if the     *)
(* report and data drain, the wire is A3 then B2, so WireSeqMonotone fails  *)
(* too. The checked cfg pins the bug FALSE. Flip only that constant to TRUE *)
(* to reproduce the counterexample recorded in formal/README.md.           *)
(*                                                                         *)
(* The other seeded bugs retain D5's original non-vacuity coverage:        *)
(* SilentSupersedeBug omits the exact report ledger,                       *)
(* CoalesceReliableBug lets volatile eviction touch reliable,             *)
(* MisdropLatestBug misclassifies a latest as a volatile drop, and         *)
(* ReportOverstateBug publishes a cumulative counter above truth.          *)
(*                                                                         *)
(* Close-reason priority matches E3 production behavior: ordinary Stale /  *)
(* Lifecycle reasons are stable against ordinary requests, but Shutdown is *)
(* allowed to supersede either and is then terminally stable.              *)
(*                                                                         *)
(* `reportQ` is bounded. A lossy send that cannot atomically admit its exact *)
(* report NEVER parks: it leaves queued predecessors untouched, abandons    *)
(* only the new frame with a loud close, and admits no later observable     *)
(* data. This is the full-control-lane edge D2 alone cannot compose.         *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Keys,                 \* finite latest-key set (checked model: {"A", "B"})
    RelBudget,            \* reliable frames offered (keep tiny)
    LatBudget,            \* latest frames offered (at least 3 for A1,B2,A3)
    BeBudget,             \* volatile frames offered (keep tiny)
    QCAP,                 \* bounded outbound data capacity
    ReportCap,            \* bounded exact DeliveryReport capacity
    ScalarInPlaceBug,     \* old scalar range + in-place replacement
    SilentSupersedeBug,   \* supersede without exact gap accounting
    CoalesceReliableBug,  \* volatile eviction may remove reliable
    MisdropLatestBug,     \* volatile eviction may remove latest
    ReportOverstateBug    \* report snapshot exceeds true cumulative count

ASSUME /\ IsFiniteSet(Keys)
       /\ Cardinality(Keys) >= 2
       /\ "nokey" \notin Keys
       /\ RelBudget \in Nat \ {0}
       /\ LatBudget \in Nat \ {0, 1, 2}
       /\ BeBudget  \in Nat \ {0}
       /\ QCAP      \in Nat \ {0, 1}
       /\ ReportCap \in Nat \ {0}
       /\ ScalarInPlaceBug    \in BOOLEAN
       /\ SilentSupersedeBug  \in BOOLEAN
       /\ CoalesceReliableBug \in BOOLEAN
       /\ MisdropLatestBug    \in BOOLEAN
       /\ ReportOverstateBug  \in BOOLEAN

Class == {"reliable", "latest", "volatile"}
GapReason == {"superseded", "volatile_drop", "latest_full"}
MaxSeq == RelBudget + LatBudget + BeBudget
SeqNo == 1..MaxSeq
KeyDom == Keys \cup {"nokey"}
Entry == [class : Class, key : KeyDom, seq : SeqNo]
Report == [lo : SeqNo, hi : SeqNo, reason : GapReason,
           sup : 0..MaxSeq, vol : 0..MaxSeq, lat : 0..MaxSeq]

VARIABLES
    queue,                  \* Seq(Entry): recipient's bounded data queue
    written,                \* SUBSET Entry: successfully written data
    wireData,               \* Seq(Entry): exact data write order
    superseded,             \* SUBSET Entry: same-key latest predecessors
    volDropped,             \* SUBSET Entry: volatile-policy drops
    latDropped,             \* SUBSET Entry: latest arrivals dropped at full
    droppedWithClose,       \* SUBSET Entry: abandoned with loud connection close
    accountedGaps,          \* SUBSET SeqNo: exact durable server gap ledger
    reportQ,                \* Seq(Report): priority DeliveryReport control lane
    reportedOnWire,         \* SUBSET SeqNo: exact gap ranges client received
    relSent,                \* reliable frames offered
    latSent,                \* latest frames offered
    volSent,                \* volatile frames offered
    connState,              \* "Open" | "CloseRequested" | "Closed"
    closeReason,            \* "None" | "Stale" | "Lifecycle" | "Shutdown"
    shutdownRequested,      \* E3 process drain has requested priority close
    lastReport,             \* last cumulative report snapshot written
    dataPassedUnreportedGap \* ghost: data exposed a gap before its report

vars == <<queue, written, wireData, superseded, volDropped, latDropped,
          droppedWithClose, accountedGaps, reportQ, reportedOnWire,
          relSent, latSent, volSent, connState, closeReason,
          shutdownRequested, lastReport, dataPassedUnreportedGap>>

QueueSet == {queue[i] : i \in DOMAIN queue}
WireSet == {wireData[i] : i \in DOMAIN wireData}
AllEntries ==
    QueueSet \cup written \cup superseded \cup volDropped \cup latDropped
             \cup droppedWithClose

RelIn(S) == {e \in S : e.class = "reliable"}
LatIn(S) == {e \in S : e.class = "latest"}
VolIn(S) == {e \in S : e.class = "volatile"}
Seqs(S) == {e.seq : e \in S}

NextSeq == relSent + latSent + volSent + 1
CurSup == Cardinality(superseded)
CurVol == Cardinality(volDropped)
CurLat == Cardinality(latDropped)
CurClosed == Cardinality(droppedWithClose)
LostSeqs == Seqs(superseded \cup volDropped \cup latDropped)
PriorLost(seq) == {s \in LostSeqs : s < seq}

RangeSeqs(report) == report.lo..report.hi
QueuedReportSeqs ==
    UNION {RangeSeqs(reportQ[i]) : i \in DOMAIN reportQ}

GapReport(lo, hi, reason, sup, vol, lat) ==
    [lo |-> lo, hi |-> hi, reason |-> reason,
     sup |-> sup, vol |-> vol, lat |-> lat]

HasLatest(k) ==
    \E i \in DOMAIN queue : queue[i].class = "latest" /\ queue[i].key = k
LatestIdx(k) ==
    CHOOSE i \in DOMAIN queue : queue[i].class = "latest" /\ queue[i].key = k

HasVol == \E i \in DOMAIN queue : queue[i].class = "volatile"
OldestVolIdx ==
    CHOOSE i \in DOMAIN queue :
        /\ queue[i].class = "volatile"
        /\ \A j \in DOMAIN queue : queue[j].class = "volatile" => i <= j

HasLat == \E i \in DOMAIN queue : queue[i].class = "latest"
OldestLatIdx ==
    CHOOSE i \in DOMAIN queue :
        /\ queue[i].class = "latest"
        /\ \A j \in DOMAIN queue : queue[j].class = "latest" => i <= j

Remove(seq, i) == SubSeq(seq, 1, i - 1) \o SubSeq(seq, i + 1, Len(seq))

RequestClose(reason) ==
    closeReason' =
        IF reason = "Shutdown"
          THEN "Shutdown"
          ELSE IF closeReason = "None" THEN reason ELSE closeReason

KeepCloseState == UNCHANGED <<droppedWithClose, connState, closeReason>>

(* Exact-report capacity exhaustion is resolved without parking a lossy     *)
(* sender or creating an unreported gap on a still-open connection.         *)
CloseForReportOverflow(new) ==
    /\ queue' = queue
    /\ droppedWithClose' = droppedWithClose \cup {new}
    /\ connState' = "CloseRequested"
    /\ RequestClose("Stale")
    /\ UNCHANGED <<superseded, volDropped, latDropped,
                   accountedGaps, reportQ>>

Init ==
    /\ queue = <<>>
    /\ written = {}
    /\ wireData = <<>>
    /\ superseded = {}
    /\ volDropped = {}
    /\ latDropped = {}
    /\ droppedWithClose = {}
    /\ accountedGaps = {}
    /\ reportQ = <<>>
    /\ reportedOnWire = {}
    /\ relSent = 0
    /\ latSent = 0
    /\ volSent = 0
    /\ connState = "Open"
    /\ closeReason = "None"
    /\ shutdownRequested = FALSE
    /\ lastReport = [sup |-> 0, vol |-> 0, lat |-> 0, closed |-> 0]
    /\ dataPassedUnreportedGap = FALSE

SendReliable ==
    /\ relSent < RelBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ Len(queue) < QCAP
    /\ queue' = Append(queue,
                       [class |-> "reliable", key |-> "nokey", seq |-> NextSeq])
    /\ relSent' = relSent + 1
    /\ UNCHANGED <<written, wireData, superseded, volDropped, latDropped,
                   droppedWithClose, accountedGaps, reportQ, reportedOnWire,
                   latSent, volSent, connState, closeReason, shutdownRequested, lastReport,
                   dataPassedUnreportedGap>>

SendLatest(k) ==
    /\ latSent < LatBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ latSent' = latSent + 1
    /\ UNCHANGED <<written, wireData, relSent, volSent,
                   lastReport, reportedOnWire,
                   shutdownRequested, dataPassedUnreportedGap>>
    /\ LET new == [class |-> "latest", key |-> k, seq |-> NextSeq] IN
         IF HasLatest(k) /\ Len(reportQ) = ReportCap
           THEN CloseForReportOverflow(new)
           ELSE IF HasLatest(k)
           THEN LET i == LatestIdx(k)
                    old == queue[i]
                    lo == old.seq
                    hi == IF ScalarInPlaceBug THEN new.seq - 1 ELSE old.seq
                    nextSup == CurSup + 1
                    reportSup == IF ReportOverstateBug THEN nextSup + 1 ELSE nextSup
                IN /\ queue' = IF ScalarInPlaceBug
                                  THEN [queue EXCEPT ![i] = new]
                                  ELSE Append(Remove(queue, i), new)
                   /\ superseded' = superseded \cup {old}
                   /\ accountedGaps' =
                          IF SilentSupersedeBug
                            THEN accountedGaps
                            ELSE accountedGaps \cup (lo..hi)
                   /\ reportQ' =
                          IF SilentSupersedeBug
                            THEN reportQ
                            ELSE Append(reportQ,
                                    GapReport(lo, hi, "superseded",
                                              reportSup, CurVol, CurLat))
                   /\ UNCHANGED <<volDropped, latDropped>>
                   /\ KeepCloseState
           ELSE IF Len(queue) < QCAP
                  THEN /\ queue' = Append(queue, new)
                       /\ UNCHANGED <<superseded, volDropped, latDropped,
                                      accountedGaps, reportQ>>
                       /\ KeepCloseState
                  ELSE IF HasVol
                         THEN IF Len(reportQ) = ReportCap
                                THEN CloseForReportOverflow(new)
                                ELSE LET j == OldestVolIdx
                                  old == queue[j]
                              IN /\ queue' = Append(Remove(queue, j), new)
                                 /\ volDropped' = volDropped \cup {old}
                                 /\ accountedGaps' = accountedGaps \cup {old.seq}
                                 /\ reportQ' = Append(reportQ,
                                        GapReport(old.seq, old.seq, "volatile_drop",
                                                  CurSup, CurVol + 1, CurLat))
                                 /\ UNCHANGED <<superseded, latDropped>>
                                 /\ KeepCloseState
                         ELSE IF Len(reportQ) = ReportCap
                                THEN CloseForReportOverflow(new)
                                ELSE /\ queue' = queue
                                     /\ latDropped' = latDropped \cup {new}
                                     /\ accountedGaps' = accountedGaps \cup {new.seq}
                                     /\ reportQ' = Append(reportQ,
                                            GapReport(new.seq, new.seq, "latest_full",
                                                      CurSup, CurVol, CurLat + 1))
                                     /\ UNCHANGED <<superseded, volDropped>>
                                     /\ KeepCloseState

SendBestEffort ==
    /\ volSent < BeBudget
    /\ connState \in {"Open", "CloseRequested"}
    /\ volSent' = volSent + 1
    /\ UNCHANGED <<written, wireData, superseded, latDropped,
                   relSent, latSent,
                   shutdownRequested, lastReport, reportedOnWire,
                   dataPassedUnreportedGap>>
    /\ LET new == [class |-> "volatile", key |-> "nokey", seq |-> NextSeq] IN
         IF Len(queue) < QCAP
           THEN /\ queue' = Append(queue, new)
                /\ UNCHANGED <<volDropped, accountedGaps, reportQ>>
                /\ KeepCloseState
           ELSE IF Len(reportQ) = ReportCap
                  THEN CloseForReportOverflow(new)
                  ELSE LET old ==
                      IF CoalesceReliableBug
                        THEN Head(queue)
                        ELSE IF MisdropLatestBug /\ HasLat
                               THEN queue[OldestLatIdx]
                               ELSE IF HasVol THEN queue[OldestVolIdx] ELSE new
                    removeIdx ==
                      IF CoalesceReliableBug
                        THEN 1
                        ELSE IF MisdropLatestBug /\ HasLat
                               THEN OldestLatIdx
                               ELSE IF HasVol THEN OldestVolIdx ELSE 0
                IN /\ queue' = IF removeIdx = 0
                                  THEN queue
                                  ELSE Append(Remove(queue, removeIdx), new)
                   /\ volDropped' = volDropped \cup {old}
                   /\ accountedGaps' = accountedGaps \cup {old.seq}
                   /\ reportQ' = Append(reportQ,
                          GapReport(old.seq, old.seq, "volatile_drop",
                                    CurSup, CurVol + 1, CurLat))
                   /\ KeepCloseState

(* DeliveryReport is control-plane data and always drains before GameData. *)
WriterDrainReport ==
    /\ connState = "Open"
    /\ reportQ # <<>>
    /\ LET report == Head(reportQ) IN
         /\ reportQ' = Tail(reportQ)
         /\ reportedOnWire' = reportedOnWire \cup RangeSeqs(report)
         /\ lastReport' = [sup |-> report.sup, vol |-> report.vol,
                            lat |-> report.lat, closed |-> CurClosed]
    /\ UNCHANGED <<queue, written, wireData, superseded, volDropped, latDropped,
                   droppedWithClose, accountedGaps, relSent, latSent, volSent,
                   connState, closeReason, shutdownRequested,
                   dataPassedUnreportedGap>>

WriterDrainData ==
    /\ connState = "Open"
    /\ reportQ = <<>>
    /\ queue # <<>>
    /\ LET head == Head(queue) IN
         /\ queue' = Tail(queue)
         /\ written' = written \cup {head}
         /\ wireData' = Append(wireData, head)
         /\ dataPassedUnreportedGap' =
                (dataPassedUnreportedGap \/
                 ~(PriorLost(head.seq) \subseteq reportedOnWire))
    /\ UNCHANGED <<superseded, volDropped, latDropped, droppedWithClose,
                   accountedGaps, reportQ, reportedOnWire, relSent, latSent,
                   volSent, connState, closeReason, shutdownRequested, lastReport>>

CloseStale ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Stale")
    /\ UNCHANGED <<queue, written, wireData, superseded, volDropped, latDropped,
                   droppedWithClose, accountedGaps, reportQ, reportedOnWire,
                   relSent, latSent, volSent, shutdownRequested, lastReport,
                   dataPassedUnreportedGap>>

CloseLifecycle ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<queue, written, wireData, superseded, volDropped, latDropped,
                   droppedWithClose, accountedGaps, reportQ, reportedOnWire,
                   relSent, latSent, volSent, shutdownRequested, lastReport,
                   dataPassedUnreportedGap>>

(* E3 priority exception: shutdown upgrades an already-requested close. *)
CloseShutdown ==
    /\ connState \in {"Open", "CloseRequested"}
    /\ closeReason # "Shutdown"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Shutdown")
    /\ shutdownRequested' = TRUE
    /\ UNCHANGED <<queue, written, wireData, superseded, volDropped, latDropped,
                   droppedWithClose, accountedGaps, reportQ, reportedOnWire,
                   relSent, latSent, volSent, lastReport, dataPassedUnreportedGap>>

CloseFinish ==
    /\ connState = "CloseRequested"
    /\ connState' = "Closed"
    /\ droppedWithClose' = droppedWithClose \cup QueueSet
    /\ queue' = <<>>
    /\ reportQ' = <<>>
    /\ UNCHANGED <<written, wireData, superseded, volDropped, latDropped,
                   accountedGaps, reportedOnWire, relSent, latSent, volSent,
                   closeReason, shutdownRequested, lastReport,
                   dataPassedUnreportedGap>>

AllResolved ==
    \/ connState = "Closed"
    \/ (relSent = RelBudget /\ latSent = LatBudget /\ volSent = BeBudget /\
        queue = <<>> /\ reportQ = <<>>)

Done == /\ AllResolved
        /\ UNCHANGED vars

Next ==
    \/ SendReliable
    \/ \E k \in Keys : SendLatest(k)
    \/ SendBestEffort
    \/ WriterDrainReport
    \/ WriterDrainData
    \/ CloseStale
    \/ CloseLifecycle
    \/ CloseShutdown
    \/ CloseFinish
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ queue \in Seq(Entry)
    /\ Len(queue) <= QCAP
    /\ written \subseteq Entry
    /\ wireData \in Seq(Entry)
    /\ superseded \subseteq Entry
    /\ volDropped \subseteq Entry
    /\ latDropped \subseteq Entry
    /\ droppedWithClose \subseteq Entry
    /\ accountedGaps \subseteq SeqNo
    /\ reportQ \in Seq(Report)
    /\ Len(reportQ) <= ReportCap
    /\ \A i \in DOMAIN reportQ : reportQ[i].lo <= reportQ[i].hi
    /\ reportedOnWire \subseteq SeqNo
    /\ relSent \in 0..RelBudget
    /\ latSent \in 0..LatBudget
    /\ volSent \in 0..BeBudget
    /\ connState \in {"Open", "CloseRequested", "Closed"}
    /\ closeReason \in {"None", "Stale", "Lifecycle", "Shutdown"}
    /\ shutdownRequested \in BOOLEAN
    /\ lastReport \in [sup : 0..MaxSeq, vol : 0..MaxSeq,
                        lat : 0..MaxSeq, closed : 0..MaxSeq]
    /\ dataPassedUnreportedGap \in BOOLEAN
    /\ \A e \in AllEntries : (e.class = "latest") <=> (e.key \in Keys)

UniqueSeqs ==
    \A e1 \in AllEntries : \A e2 \in AllEntries :
        (e1.seq = e2.seq) => (e1 = e2)

WrittenMatchesWire == written = WireSet

ReliableConservation ==
    /\ relSent = Cardinality(RelIn(QueueSet))
                 + Cardinality(RelIn(written))
                 + Cardinality(RelIn(droppedWithClose))
    /\ RelIn(superseded) = {}
    /\ RelIn(volDropped) = {}
    /\ RelIn(latDropped) = {}

CoalesceNeverTouchesReliable ==
    /\ RelIn(superseded) = {}
    /\ RelIn(volDropped) = {}
    /\ RelIn(latDropped) = {}

LatestConservation ==
    /\ latSent = Cardinality(LatIn(QueueSet))
                 + Cardinality(LatIn(written))
                 + Cardinality(LatIn(superseded))
                 + Cardinality(LatIn(latDropped))
                 + Cardinality(LatIn(droppedWithClose))
    /\ LatIn(volDropped) = {}

VolatileConservation ==
    /\ volSent = Cardinality(VolIn(QueueSet))
                 + Cardinality(VolIn(written))
                 + Cardinality(VolIn(volDropped))
                 + Cardinality(VolIn(droppedWithClose))
    /\ VolIn(superseded) = {}
    /\ VolIn(latDropped) = {}

LatestValueLastWrite ==
    /\ \A k \in Keys :
         Cardinality({i \in DOMAIN queue :
                        queue[i].class = "latest" /\ queue[i].key = k}) <= 1
    /\ \A qi \in DOMAIN queue :
         queue[qi].class = "latest" =>
           \A e \in (superseded \cup written) :
             (e.class = "latest" /\ e.key = queue[qi].key) =>
               e.seq < queue[qi].seq

(* The report ledger names exactly, not approximately, every non-close loss. *)
ExactGapAccounting == accountedGaps = LostSeqs

(* While data can still be written, every accounted range is either queued  *)
(* on the priority control lane or already visible on the wire.             *)
ReportsRemainCausal ==
    connState = "Open" =>
      accountedGaps = (QueuedReportSeqs \cup reportedOnWire)

ReportsAreCausallyPrioritized == ~dataPassedUnreportedGap

(* WebSocket order for this one sender must remain strictly increasing. *)
QueueSeqMonotone ==
    \A i, j \in DOMAIN queue : i < j => queue[i].seq < queue[j].seq

WireSeqMonotone ==
    \A i, j \in DOMAIN wireData : i < j => wireData[i].seq < wireData[j].seq

ReportHonest ==
    /\ lastReport.sup <= CurSup
    /\ lastReport.vol <= CurVol
    /\ lastReport.lat <= CurLat
    /\ lastReport.closed <= CurClosed
    /\ \A i \in DOMAIN reportQ :
         /\ reportQ[i].sup <= CurSup
         /\ reportQ[i].vol <= CurVol
         /\ reportQ[i].lat <= CurLat

(* A report-capacity overflow abandons only with a semantic close; it never  *)
(* becomes a best-effort gap on a connection that can keep delivering data. *)
DropsWithCloseAreLoud ==
    droppedWithClose # {} =>
      (connState \in {"CloseRequested", "Closed"} /\ closeReason # "None")

(* Latest and volatile remain enabled at a full data/report queue: overflow *)
(* takes the immediate loud-close branch rather than parking either sender. *)
LossyClassesNeverPark ==
    /\ (~ScalarInPlaceBug /\ latSent < LatBudget /\
        connState \in {"Open", "CloseRequested"}) =>
         ENABLED \E k \in Keys : SendLatest(k)
    /\ (~ScalarInPlaceBug /\ volSent < BeBudget /\
        connState \in {"Open", "CloseRequested"}) =>
         ENABLED SendBestEffort

(* The explicit E3 request latch makes the priority override a state         *)
(* invariant: a shutdown request and a non-Shutdown reason cannot coexist.   *)
ShutdownWins == shutdownRequested => closeReason = "Shutdown"

----------------------------------------------------------------------------
(* Temporal close-reason priority: Shutdown may upgrade an ordinary reason; *)
(* once Shutdown is selected no later request can replace it.               *)
ShutdownPriorityStable ==
    [][ /\ closeReason = "Shutdown" => closeReason' = "Shutdown"
        /\ closeReason \in {"Stale", "Lifecycle"} =>
             closeReason' \in {closeReason, "Shutdown"} ]_vars

=============================================================================

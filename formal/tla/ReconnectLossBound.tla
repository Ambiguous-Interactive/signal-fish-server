------------------------ MODULE ReconnectLossBound ------------------------
(***************************************************************************)
(* Additional disconnect/outage exposure for one recipient and one room.    *)
(*                                                                          *)
(* This is deliberately NOT a bound on every GameData omission visible at   *)
(* reconnect. Delivery-class supersession/drop and any gap already observed  *)
(* before the cut are accounted elsewhere. The modeled online candidates are *)
(* only frames committed to this recipient's old delivery pipeline.          *)
(*                                                                          *)
(* A disconnect can add two disjoint sets of unobservable frames:            *)
(*                                                                          *)
(*   1. the old connection tail: frames in its outbound queue or already     *)
(*      dequeued but not yet observed by the client application; and          *)
(*   2. outage traffic accepted while the recipient has no live route.        *)
(*                                                                          *)
(* The post-queue stage includes the server batcher, an active/partial write, *)
(* kernel and network buffers, the client receive stack, and any frame not    *)
(* yet applied by the client. PCAP is therefore an independently established *)
(* end-to-end frame-count bound, not the configured socket-buffer byte hint.  *)
(*                                                                          *)
(* Outage arrivals use a burst-plus-rate assumption. BURST frames may be      *)
(* accepted immediately after the cut. Every elapsed time quantum releases   *)
(* RATE more admissions, so at elapsedQuanta = t:                             *)
(*                                                                          *)
(*   absentAccepted <= BURST + RATE * t                                       *)
(*                                                                          *)
(* This is the discrete counterpart of an enforced arrival curve             *)
(* A(T) <= B + ceil(R*T). Production supplies no such room-wide admission     *)
(* limit; the constants are operator/workload assumptions.                    *)
(*                                                                          *)
(* NON-VACUITY: IgnorePostQueuePipelineBug = TRUE omits PCAP from the claimed *)
(* bound. The CI-pinned _ExpectedFailure configuration requires TLC to report *)
(* the resulting ReconnectExposureBounded violation.                          *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    ONLINE_BUDGET,                \* old-pipeline commits available pre-cut
    QCAP,                         \* recipient outbound data-queue capacity
    PCAP,                         \* dequeued but client-unobserved pipeline cap
    BURST,                        \* immediate post-cut admission allowance
    RATE,                         \* admission allowance per elapsed quantum
    WINDOW,                       \* maximum elapsed quanta before expiry
    IgnorePostQueuePipelineBug    \* TRUE omits PCAP (must violate the bound)

ASSUME /\ ONLINE_BUDGET \in Nat \ {0}
       /\ QCAP \in Nat \ {0}
       /\ PCAP \in Nat \ {0}
       /\ BURST \in Nat
       /\ RATE \in Nat
       /\ WINDOW \in Nat
       /\ IgnorePostQueuePipelineBug \in BOOLEAN

VARIABLES
    phase,                   \* {"Online", "Dropped", "Reconnected"}
    queue,                   \* committed frames in the outbound data queue
    postQueuePipeline,       \* dequeued but not client-application-observed
    onlineCommitted,         \* commits to this old connection's pipeline
    clientObserved,          \* those commits applied before the cut
    elapsedQuanta,           \* elapsed outage time quanta
    burstAccepted,           \* accepted against the burst allowance
    steadyAccepted,          \* accepted against elapsed rate allowance
    absentAccepted,          \* all frames accepted while recipient is absent
    pipelineAtDrop,          \* queue + postQueuePipeline at the cut
    reconnectExposure        \* additional unobservable frames from this cut

vars == <<phase, queue, postQueuePipeline, onlineCommitted, clientObserved,
          elapsedQuanta, burstAccepted, steadyAccepted, absentAccepted,
          pipelineAtDrop, reconnectExposure>>

Init ==
    /\ phase = "Online"
    /\ queue = 0
    /\ postQueuePipeline = 0
    /\ onlineCommitted = 0
    /\ clientObserved = 0
    /\ elapsedQuanta = 0
    /\ burstAccepted = 0
    /\ steadyAccepted = 0
    /\ absentAccepted = 0
    /\ pipelineAtDrop = 0
    /\ reconnectExposure = 0

(* Commit one frame to the old recipient's outbound delivery queue. *)
EnqueueOnline ==
    /\ phase = "Online"
    /\ onlineCommitted < ONLINE_BUDGET
    /\ queue < QCAP
    /\ queue' = queue + 1
    /\ onlineCommitted' = onlineCommitted + 1
    /\ UNCHANGED <<phase, postQueuePipeline, clientObserved, elapsedQuanta,
                   burstAccepted, steadyAccepted, absentAccepted,
                   pipelineAtDrop, reconnectExposure>>

(* Dequeue into any later stage before client application observation. *)
DequeueToPostQueuePipeline ==
    /\ phase = "Online"
    /\ queue > 0
    /\ postQueuePipeline < PCAP
    /\ queue' = queue - 1
    /\ postQueuePipeline' = postQueuePipeline + 1
    /\ UNCHANGED <<phase, onlineCommitted, clientObserved, elapsedQuanta,
                   burstAccepted, steadyAccepted, absentAccepted,
                   pipelineAtDrop, reconnectExposure>>

(* Application observation retires one frame from every post-queue stage. *)
ClientObserve ==
    /\ phase = "Online"
    /\ postQueuePipeline > 0
    /\ postQueuePipeline' = postQueuePipeline - 1
    /\ clientObserved' = clientObserved + 1
    /\ UNCHANGED <<phase, queue, onlineCommitted, elapsedQuanta,
                   burstAccepted, steadyAccepted, absentAccepted,
                   pipelineAtDrop, reconnectExposure>>

(* Abrupt connection loss abandons the complete unobserved old-pipeline tail. *)
Drop ==
    /\ phase = "Online"
    /\ phase' = "Dropped"
    /\ pipelineAtDrop' = queue + postQueuePipeline
    /\ queue' = 0
    /\ postQueuePipeline' = 0
    /\ UNCHANGED <<onlineCommitted, clientObserved, elapsedQuanta,
                   burstAccepted, steadyAccepted, absentAccepted,
                   reconnectExposure>>

(* A token-bucket-style burst is admissible even before time advances. *)
AcceptAbsentBurst ==
    /\ phase = "Dropped"
    /\ burstAccepted < BURST
    /\ burstAccepted' = burstAccepted + 1
    /\ absentAccepted' = absentAccepted + 1
    /\ UNCHANGED <<phase, queue, postQueuePipeline, onlineCommitted,
                   clientObserved, elapsedQuanta, steadyAccepted,
                   pipelineAtDrop, reconnectExposure>>

(* One elapsed quantum releases RATE additional admissions. *)
AdvanceQuantum ==
    /\ phase = "Dropped"
    /\ elapsedQuanta < WINDOW
    /\ elapsedQuanta' = elapsedQuanta + 1
    /\ UNCHANGED <<phase, queue, postQueuePipeline, onlineCommitted,
                   clientObserved, burstAccepted, steadyAccepted,
                   absentAccepted, pipelineAtDrop, reconnectExposure>>

(* Spend only rate credit released by elapsed time; no average-rate shortcut. *)
AcceptAbsentSteady ==
    /\ phase = "Dropped"
    /\ steadyAccepted < RATE * elapsedQuanta
    /\ steadyAccepted' = steadyAccepted + 1
    /\ absentAccepted' = absentAccepted + 1
    /\ UNCHANGED <<phase, queue, postQueuePipeline, onlineCommitted,
                   clientObserved, elapsedQuanta, burstAccepted,
                   pipelineAtDrop, reconnectExposure>>

(* Fresh baseline: GameData from these two sets is not replayed. *)
Reconnect ==
    /\ phase = "Dropped"
    /\ phase' = "Reconnected"
    /\ reconnectExposure' = pipelineAtDrop + absentAccepted
    /\ UNCHANGED <<queue, postQueuePipeline, onlineCommitted, clientObserved,
                   elapsedQuanta, burstAccepted, steadyAccepted,
                   absentAccepted, pipelineAtDrop>>

(* Explicit terminal stutter keeps TLC deadlock checking meaningful. *)
Done ==
    /\ phase = "Reconnected"
    /\ UNCHANGED vars

Next ==
    \/ EnqueueOnline
    \/ DequeueToPostQueuePipeline
    \/ ClientObserve
    \/ Drop
    \/ AcceptAbsentBurst
    \/ AdvanceQuantum
    \/ AcceptAbsentSteady
    \/ Reconnect
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ phase \in {"Online", "Dropped", "Reconnected"}
    /\ queue \in 0..QCAP
    /\ postQueuePipeline \in 0..PCAP
    /\ onlineCommitted \in 0..ONLINE_BUDGET
    /\ clientObserved \in 0..ONLINE_BUDGET
    /\ elapsedQuanta \in 0..WINDOW
    /\ burstAccepted \in 0..BURST
    /\ steadyAccepted \in 0..(RATE * WINDOW)
    /\ absentAccepted \in 0..(BURST + RATE * WINDOW)
    /\ pipelineAtDrop \in 0..(QCAP + PCAP)
    /\ reconnectExposure \in 0..(ONLINE_BUDGET + BURST + RATE * WINDOW)

(* Pre-cut commits are observed or in one pipeline stage. After the cut the   *)
(* only unobserved committed frames are exactly the captured pipeline tail.   *)
OldPipelineConservation ==
    IF phase = "Online"
        THEN onlineCommitted = clientObserved + queue + postQueuePipeline
        ELSE onlineCommitted = clientObserved + pipelineAtDrop

AbsentAccounting ==
    absentAccepted = burstAccepted + steadyAccepted

(* This invariant is the executable burst-plus-elapsed-rate assumption. *)
AbsentArrivalCurve ==
    /\ steadyAccepted <= RATE * elapsedQuanta
    /\ absentAccepted <= BURST + RATE * elapsedQuanta

(* This is additional exposure caused by this cut, not total historical loss. *)
ReconnectExposureExact ==
    phase = "Reconnected" =>
        reconnectExposure = pipelineAtDrop + absentAccepted

(* The bug arm proves the complete post-queue pipeline term is necessary. *)
ReconnectExposureBounded ==
    phase = "Reconnected" =>
        reconnectExposure <=
            QCAP
            + (IF IgnorePostQueuePipelineBug THEN 0 ELSE PCAP)
            + BURST
            + RATE * WINDOW

=============================================================================

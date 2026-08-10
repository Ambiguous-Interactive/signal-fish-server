---------------------- MODULE RoomMessageTransaction ----------------------
(***************************************************************************)
(* Exact-membership, two-phase room-message transaction (P75).             *)
(*                                                                         *)
(* The model bounds production at one or two recipients and two phases.    *)
(* framePlan is chosen nondeterministically from every per-recipient subset *)
(* of the phases, excluding only the all-empty transaction. Thus the       *)
(* two-recipient run checks all 15 labeled nonempty plans (nine up to       *)
(* recipient symmetry), including identity-only members.                   *)
(*                                                                         *)
(* Each reservation attempt snapshots the current connection generation.   *)
(* A generation change can cancel an outstanding scoped reservation; only  *)
(* that explicit canceled result enables the stale-attempt retry. Retry     *)
(* releases every held sibling permit, records all canceled waiters,        *)
(* refreshes all generation snapshots, and retains both FnOnce callbacks.   *)
(* ChannelClosed / SlowConsumer is limited to members with physical frames  *)
(* and atomically abstracts conditional sender removal plus the next outer  *)
(* collection: either no replacement exists (RoutingChanged) or a fresh    *)
(* replacement generation begins another reservation attempt.              *)
(*                                                                         *)
(* Mapping to commit_room_messages_if_members_with_hook:                   *)
(*   ReserveFrame                 reserve_room_batch / DeliveryPermit      *)
(*   ChangeGeneration            a matching queue generation is replaced  *)
(*   CancelStaleReservation      RoomBatchReservation::Canceled result     *)
(*   RetryCanceledReservation    release partial attempt / recollect       *)
(*   ReservationUnavailable      closed/slow removal and recollection      *)
(*   LeaveRoom / AbortRoutingChanged                                      *)
(*                                exact membership/sender revalidation     *)
(*   CallHook                     before_send accept/reject/Err             *)
(*   SendFrame                    phase-ordered permit.send                *)
(*   CallPhaseZeroCallback        after_first_phase(failed_phase_zero)     *)
(*   Finish                       Committed / CommittedDegraded            *)
(*                                                                         *)
(* HookError is an abstraction of the outer anyhow::Result::Err channel; it *)
(* is not a RoomMessageTransactionOutcome enum variant.                    *)
(***************************************************************************)
EXTENDS FiniteSets, Integers, Naturals

CONSTANTS
    Recipients,
    Phases,
    Actor,
    PartialReservationBug,
    SkipRoutingValidationBug,
    SkipEmptyRoutingValidationBug,
    PhaseBoundaryBug,
    DuplicateCallbackBug,
    MisreportCallbackBug,
    MiscountFailedFramesBug,
    OmitPermitReleaseBug,
    OmitRetryReleaseBug,
    ConsumeHookOnRetryBug,
    ConsumePhaseCallbackOnRetryBug,
    SkipRetryRefreshBug

ASSUME /\ Cardinality(Recipients) \in 1..2
       /\ Phases = {0, 1}
       /\ Actor \in Recipients
       /\ PartialReservationBug \in BOOLEAN
       /\ SkipRoutingValidationBug \in BOOLEAN
       /\ SkipEmptyRoutingValidationBug \in BOOLEAN
       /\ PhaseBoundaryBug \in BOOLEAN
       /\ DuplicateCallbackBug \in BOOLEAN
       /\ MisreportCallbackBug \in BOOLEAN
       /\ MiscountFailedFramesBug \in BOOLEAN
       /\ OmitPermitReleaseBug \in BOOLEAN
       /\ OmitRetryReleaseBug \in BOOLEAN
       /\ ConsumeHookOnRetryBug \in BOOLEAN
       /\ ConsumePhaseCallbackOnRetryBug \in BOOLEAN
       /\ SkipRetryRefreshBug \in BOOLEAN

NoCount == -1
Generation == 0..2
FrameStates ==
    {"Absent", "Unreserved", "Reserved", "Enqueued", "Stale", "Closed",
     "Aborted", "Canceled"}
FailureStates == {"Stale", "Closed", "Canceled"}
TerminalOutcomes ==
    {"Committed", "CommittedDegraded", "RoutingChanged",
     "HookRejected", "HookError"}

VARIABLES
    framePlan,                \* every nonempty Recipients -> SUBSET Phases plan
    stage,                    \* Reserving / Accepted / CallbackDone / Complete
    routePresent,             \* current exact-member map
    currentGeneration,        \* current connection generation
    attemptGeneration,        \* generation captured by this reservation attempt
    canceledWaiters,          \* recipients whose scoped batch returned Canceled
    unavailableResults,       \* closed/slow sibling results joined with Canceled
    staleCancellationObserved,
    queueOpen,                \* receiver can close during the async hook
    permitGeneration,         \* synthetic import of P73 stale-permit behavior
    frameState,               \* lifecycle of each recipient/phase frame
    reservedSlots,            \* queue capacity claims held by permits
    permitProducers,          \* permit-backed producer capabilities
    hookCalls,
    hookAvailable,            \* before_send FnOnce is still owned
    hookResult,               \* None / Accepted / Rejected / Error
    reservedAtHook,
    routingValidAtHook,
    callbackCalls,
    callbackAvailable,        \* after_first_phase FnOnce is still owned
    callbackDecision,         \* None / Continue / Stop
    callbackObserved,         \* phase-zero failed-frame argument
    failedFrames,             \* returned CommittedDegraded count
    retryCount,
    retryHeldPermits,
    retryCanceledWaiterCount,
    retryUnavailableHeldPermits,
    retryCanceledPermits,
    outcome

vars ==
    <<framePlan, stage, routePresent, currentGeneration, attemptGeneration,
      canceledWaiters, unavailableResults, staleCancellationObserved, queueOpen, permitGeneration,
      frameState, reservedSlots, permitProducers, hookCalls, hookAvailable,
      hookResult, reservedAtHook, routingValidAtHook, callbackCalls,
      callbackAvailable, callbackDecision, callbackObserved, failedFrames,
      retryCount, retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

Frames == {f \in Recipients \X Phases : f[2] \in framePlan[f[1]]}

StaleWaiterRecipients ==
    {r \in Recipients :
        r \notin unavailableResults
        /\ currentGeneration[r] # attemptGeneration[r]
        /\ \E p \in framePlan[r] : frameState[r][p] = "Unreserved"}

HealthyOutstandingFrames ==
    {f \in Frames :
        frameState[f[1]][f[2]] = "Unreserved"
        /\ f[1] \notin StaleWaiterRecipients
        /\ f[1] \notin unavailableResults}

ReservedFrames ==
    {f \in Frames : frameState[f[1]][f[2]] = "Reserved"}

FailedFrames ==
    {f \in Frames : frameState[f[1]][f[2]] \in FailureStates}

PhaseFailures(p) ==
    Cardinality({r \in Recipients :
        p \in framePlan[r] /\ frameState[r][p] \in FailureStates})

PhaseResolved(p) ==
    \A r \in Recipients :
        p \notin framePlan[r]
        \/ frameState[r][p] \notin {"Unreserved", "Reserved"}

ExactRouting ==
    \A r \in Recipients :
        routePresent[r] /\ currentGeneration[r] = attemptGeneration[r]

NoStalePhysicalWaiter ==
    StaleWaiterRecipients = {}

RoutingAccepted ==
    SkipRoutingValidationBug
    \/ \A r \in Recipients :
           IF SkipEmptyRoutingValidationBug /\ framePlan[r] = {}
           THEN TRUE
           ELSE routePresent[r]
                /\ currentGeneration[r] = attemptGeneration[r]

AllReserved == Cardinality(ReservedFrames) = Cardinality(Frames)

ReleasedCounter(counter, count) ==
    IF OmitPermitReleaseBug THEN counter ELSE counter - count

CountFailures(count) ==
    IF MiscountFailedFramesBug THEN 0 ELSE count

AbortReserved ==
    [r \in Recipients |->
        [p \in Phases |->
            IF frameState[r][p] = "Reserved"
                THEN "Aborted"
                ELSE frameState[r][p]]]

ResetReserved ==
    [r \in Recipients |->
        [p \in Phases |->
            IF frameState[r][p] = "Reserved"
                THEN "Unreserved"
                ELSE frameState[r][p]]]

ResetRecipientReserved(recipient) ==
    [r \in Recipients |->
        [p \in Phases |->
            IF r = recipient /\ frameState[r][p] = "Reserved"
                THEN "Unreserved"
                ELSE frameState[r][p]]]

CancelPhaseOne ==
    [r \in Recipients |->
        [p \in Phases |->
            IF p = 1 /\ frameState[r][p] = "Reserved"
                THEN "Canceled"
                ELSE frameState[r][p]]]

Init ==
    /\ framePlan \in [Recipients -> SUBSET Phases]
    /\ Cardinality(Frames) > 0
    /\ stage = "Reserving"
    /\ routePresent = [r \in Recipients |-> TRUE]
    /\ currentGeneration = [r \in Recipients |-> 0]
    /\ attemptGeneration = [r \in Recipients |-> 0]
    /\ canceledWaiters = {}
    /\ unavailableResults = {}
    /\ staleCancellationObserved = FALSE
    /\ queueOpen = [r \in Recipients |-> TRUE]
    /\ permitGeneration = [r \in Recipients |-> 0]
    /\ frameState = [r \in Recipients |-> [p \in Phases |->
           IF p \in framePlan[r] THEN "Unreserved" ELSE "Absent"]]
    /\ reservedSlots = 0
    /\ permitProducers = 0
    /\ hookCalls = 0
    /\ hookAvailable = TRUE
    /\ hookResult = "None"
    /\ reservedAtHook = NoCount
    /\ routingValidAtHook = FALSE
    /\ callbackCalls = 0
    /\ callbackAvailable = TRUE
    /\ callbackDecision = "None"
    /\ callbackObserved = NoCount
    /\ failedFrames = 0
    /\ retryCount = 0
    /\ retryHeldPermits = NoCount
    /\ retryCanceledWaiterCount = NoCount
    /\ retryUnavailableHeldPermits = 0
    /\ retryCanceledPermits = 0
    /\ outcome = "None"

ReserveFrame(r, p) ==
    /\ stage = "Reserving"
    /\ canceledWaiters = {}
    /\ r \notin unavailableResults
    /\ <<r, p>> \in Frames
    /\ frameState[r][p] = "Unreserved"
    /\ currentGeneration[r] = attemptGeneration[r]
    /\ frameState' = [frameState EXCEPT ![r][p] = "Reserved"]
    /\ reservedSlots' = reservedSlots + 1
    /\ permitProducers' = permitProducers + 1
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   hookCalls, hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackAvailable,
                   callbackDecision, callbackObserved, failedFrames,
                   retryCount, retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

LeaveRoom(r) ==
    /\ stage = "Reserving"
    /\ routePresent[r]
    /\ routePresent' = [routePresent EXCEPT ![r] = FALSE]
    /\ UNCHANGED <<framePlan, stage, currentGeneration, attemptGeneration,
                   canceledWaiters, unavailableResults, staleCancellationObserved, queueOpen,
                   permitGeneration, frameState, reservedSlots,
                   permitProducers, hookCalls, hookAvailable, hookResult,
                   reservedAtHook, routingValidAtHook, callbackCalls,
                   callbackAvailable, callbackDecision, callbackObserved,
                   failedFrames, retryCount, retryHeldPermits,
                   retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

ChangeGeneration(r) ==
    /\ stage = "Reserving"
    /\ retryCount = 0
    /\ canceledWaiters = {}
    /\ currentGeneration[r] < 2
    /\ currentGeneration' = [currentGeneration EXCEPT ![r] = @ + 1]
    /\ UNCHANGED <<framePlan, stage, routePresent, attemptGeneration,
                   canceledWaiters, unavailableResults, staleCancellationObserved, queueOpen,
                   permitGeneration, frameState, reservedSlots,
                   permitProducers, hookCalls, hookAvailable, hookResult,
                   reservedAtHook, routingValidAtHook, callbackCalls,
                   callbackAvailable, callbackDecision, callbackObserved,
                   failedFrames, retryCount, retryHeldPermits,
                   retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

(* join_all can resolve a healthy-generation sibling as closed/slow while  *)
(* another recipient is stale. Record that result and release any permits  *)
(* its batch already held, but defer route cleanup because Canceled wins.   *)
ResolveUnavailableSibling(r) ==
    /\ stage = "Reserving"
    /\ retryCount = 0
    /\ StaleWaiterRecipients # {}
    /\ r \notin StaleWaiterRecipients
    /\ r \notin unavailableResults
    /\ framePlan[r] # {}
    /\ routePresent[r]
    /\ currentGeneration[r] = attemptGeneration[r]
    /\ \E p \in framePlan[r] : frameState[r][p] = "Unreserved"
    /\ LET held == Cardinality({f \in ReservedFrames : f[1] = r}) IN
       /\ frameState' = ResetRecipientReserved(r)
       /\ reservedSlots' = ReleasedCounter(reservedSlots, held)
       /\ permitProducers' = ReleasedCounter(permitProducers, held)
       /\ retryUnavailableHeldPermits' = retryUnavailableHeldPermits + held
    /\ unavailableResults' = unavailableResults \cup {r}
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   hookCalls, hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackAvailable,
                   callbackDecision, callbackObserved, failedFrames,
                   retryCount, retryHeldPermits, retryCanceledWaiterCount,
                   retryCanceledPermits, outcome>>

(* A stale generation must first be observed by an outstanding physical    *)
(* reservation. Identity-only members cannot manufacture Canceled.         *)
CancelStaleReservation(r, p) ==
    /\ stage = "Reserving"
    /\ retryCount = 0
    /\ r \notin canceledWaiters
    /\ r \in StaleWaiterRecipients
    /\ <<r, p>> \in Frames
    /\ frameState[r][p] = "Unreserved"
    /\ HealthyOutstandingFrames = {}
    /\ canceledWaiters' = canceledWaiters \cup {r}
    /\ staleCancellationObserved' = TRUE
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, unavailableResults, queueOpen,
                   permitGeneration,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackAvailable,
                   callbackDecision, callbackObserved, failedFrames,
                   retryCount, retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

(* Every canceled waiter plus every held sibling is canceled exactly once. *)
(* The refreshed attempt snapshots all current generations and keeps both  *)
(* one-shot closures. Zero held siblings is a valid retry.                  *)
RetryCanceledReservation ==
    /\ stage = "Reserving"
    /\ retryCount = 0
    /\ canceledWaiters # {}
    /\ canceledWaiters = StaleWaiterRecipients
    /\ HealthyOutstandingFrames = {}
    /\ LET held == Cardinality(ReservedFrames)
           waiters == Cardinality(canceledWaiters)
       IN
       /\ frameState' = ResetReserved
       /\ IF OmitRetryReleaseBug
             THEN /\ UNCHANGED reservedSlots
                  /\ UNCHANGED permitProducers
             ELSE /\ reservedSlots' = reservedSlots - held
                  /\ permitProducers' = permitProducers - held
       /\ retryHeldPermits' = held
       /\ retryCanceledWaiterCount' = waiters
       /\ retryCanceledPermits' =
              held + waiters + retryUnavailableHeldPermits
    /\ attemptGeneration' =
           IF SkipRetryRefreshBug THEN attemptGeneration ELSE currentGeneration
    /\ canceledWaiters' = {}
    /\ unavailableResults' = {}
    /\ retryCount' = 1
    /\ hookAvailable' = IF ConsumeHookOnRetryBug THEN FALSE ELSE hookAvailable
    /\ callbackAvailable' =
           IF ConsumePhaseCallbackOnRetryBug THEN FALSE ELSE callbackAvailable
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   hookCalls, hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackDecision, callbackObserved,
                   failedFrames, retryUnavailableHeldPermits, outcome>>

(* Atomic abstraction of conditional failed-sender removal plus the next   *)
(* outer-loop collection. An installed replacement starts from a fresh     *)
(* snapshot; otherwise the expected member is missing and routing changes. *)
ReservationUnavailable(r, replacement) ==
    /\ stage = "Reserving"
    /\ canceledWaiters = {}
    /\ NoStalePhysicalWaiter
    /\ r \in Recipients
    /\ replacement \in BOOLEAN
    /\ framePlan[r] # {}
    /\ routePresent[r]
    /\ currentGeneration[r] = attemptGeneration[r]
    /\ \E p \in framePlan[r] : frameState[r][p] = "Unreserved"
    /\ ~replacement \/ currentGeneration[r] < 2
    /\ LET held == Cardinality(ReservedFrames)
           refreshed == [currentGeneration EXCEPT ![r] = @ + 1]
       IN
       /\ reservedSlots' = ReleasedCounter(reservedSlots, held)
       /\ permitProducers' = ReleasedCounter(permitProducers, held)
       /\ IF replacement
             THEN /\ frameState' = ResetReserved
                  /\ currentGeneration' = refreshed
                  /\ attemptGeneration' = refreshed
                  /\ UNCHANGED <<stage, routePresent, outcome>>
             ELSE /\ frameState' = AbortReserved
                  /\ routePresent' = [routePresent EXCEPT ![r] = FALSE]
                  /\ stage' = "Complete"
                  /\ outcome' = "RoutingChanged"
                  /\ UNCHANGED <<currentGeneration, attemptGeneration>>
    /\ UNCHANGED <<framePlan, canceledWaiters, unavailableResults, staleCancellationObserved,
                   queueOpen, permitGeneration, hookCalls, hookAvailable,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackAvailable, callbackDecision,
                   callbackObserved, failedFrames, retryCount,
                   retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits>>

AbortRoutingChanged ==
    /\ stage = "Reserving"
    /\ AllReserved
    /\ ~ExactRouting
    /\ stage' = "Complete"
    /\ frameState' = AbortReserved
    /\ reservedSlots' = ReleasedCounter(reservedSlots, Cardinality(ReservedFrames))
    /\ permitProducers' =
           ReleasedCounter(permitProducers, Cardinality(ReservedFrames))
    /\ outcome' = "RoutingChanged"
    /\ UNCHANGED <<framePlan, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   hookCalls, hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackAvailable,
                   callbackDecision, callbackObserved, failedFrames,
                   retryCount, retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits>>

CallHook(result) ==
    /\ stage = "Reserving"
    /\ canceledWaiters = {}
    /\ unavailableResults = {}
    /\ result \in {"Accepted", "Rejected", "Error"}
    /\ hookCalls = 0
    /\ hookAvailable
    /\ Cardinality(ReservedFrames) > 0
    /\ (AllReserved \/ PartialReservationBug)
    /\ RoutingAccepted
    /\ hookCalls' = 1
    /\ hookAvailable' = FALSE
    /\ hookResult' = result
    /\ reservedAtHook' = Cardinality(ReservedFrames)
    /\ routingValidAtHook' = ExactRouting
    /\ IF result = "Accepted"
          THEN /\ stage' = "Accepted"
               /\ UNCHANGED <<frameState, reservedSlots, permitProducers,
                              outcome>>
          ELSE /\ stage' = "Complete"
               /\ frameState' = AbortReserved
               /\ reservedSlots' =
                      ReleasedCounter(reservedSlots, Cardinality(ReservedFrames))
               /\ permitProducers' =
                      ReleasedCounter(permitProducers, Cardinality(ReservedFrames))
               /\ outcome' = IF result = "Rejected"
                                  THEN "HookRejected" ELSE "HookError"
    /\ UNCHANGED <<framePlan, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   callbackCalls, callbackAvailable, callbackDecision,
                   callbackObserved, failedFrames, retryCount,
                   retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits>>

CloseAfterHook(r) ==
    /\ stage \in {"Accepted", "CallbackDone"}
    /\ queueOpen[r]
    /\ queueOpen' = [queueOpen EXCEPT ![r] = FALSE]
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, permitGeneration, frameState,
                   reservedSlots, permitProducers, hookCalls, hookAvailable,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackAvailable, callbackDecision,
                   callbackObserved, failedFrames, retryCount,
                   retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

StaleAfterHook(r) ==
    /\ stage \in {"Accepted", "CallbackDone"}
    /\ permitGeneration[r] = 0
    /\ permitGeneration' = [permitGeneration EXCEPT ![r] = 1]
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, frameState,
                   reservedSlots, permitProducers, hookCalls, hookAvailable,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackAvailable, callbackDecision,
                   callbackObserved, failedFrames, retryCount,
                   retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

SendResult(r) ==
    IF ~queueOpen[r] THEN "Closed"
    ELSE IF permitGeneration[r] # 0 THEN "Stale"
    ELSE "Enqueued"

SendFrame(r, p) ==
    /\ frameState[r][p] = "Reserved"
    /\ \/ /\ p = 0
           /\ stage = "Accepted"
       \/ /\ p = 1
           /\ \/ /\ stage = "CallbackDone"
                    /\ callbackDecision = "Continue"
              \/ /\ PhaseBoundaryBug
                    /\ stage = "Accepted"
    /\ LET sendResult == SendResult(r) IN
       /\ frameState' = [frameState EXCEPT ![r][p] = sendResult]
       /\ failedFrames' = failedFrames
              + IF sendResult \in {"Stale", "Closed"}
                    THEN CountFailures(1) ELSE 0
    /\ reservedSlots' = ReleasedCounter(reservedSlots, 1)
    /\ permitProducers' = ReleasedCounter(permitProducers, 1)
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   hookCalls, hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackAvailable,
                   callbackDecision, callbackObserved, retryCount,
                   retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

CallPhaseZeroCallback(decision) ==
    /\ stage = "Accepted"
    /\ PhaseResolved(0)
    /\ callbackCalls = 0
    /\ callbackAvailable
    /\ decision \in {"Continue", "Stop"}
    /\ callbackCalls' = 1
    /\ callbackAvailable' = FALSE
    /\ callbackDecision' = decision
    /\ callbackObserved' =
           IF MisreportCallbackBug THEN 0 ELSE PhaseFailures(0)
    /\ IF decision = "Continue"
          THEN /\ stage' = "CallbackDone"
               /\ UNCHANGED <<frameState, reservedSlots, permitProducers,
                              failedFrames, outcome>>
          ELSE LET canceled ==
                       Cardinality({r \in Recipients :
                           frameState[r][1] = "Reserved"})
                   finalFailures == failedFrames + CountFailures(canceled)
               IN /\ stage' = "Complete"
                  /\ frameState' = CancelPhaseOne
                  /\ reservedSlots' = ReleasedCounter(reservedSlots, canceled)
                  /\ permitProducers' =
                         ReleasedCounter(permitProducers, canceled)
                  /\ failedFrames' = finalFailures
                  /\ outcome' = IF finalFailures = 0
                                    THEN "Committed" ELSE "CommittedDegraded"
    /\ UNCHANGED <<framePlan, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   hookCalls, hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, retryCount, retryHeldPermits,
                   retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits>>

CallPhaseZeroCallbackAgain ==
    /\ DuplicateCallbackBug
    /\ stage = "CallbackDone"
    /\ callbackCalls = 1
    /\ callbackCalls' = 2
    /\ UNCHANGED <<framePlan, stage, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackAvailable, callbackDecision,
                   callbackObserved, failedFrames, retryCount,
                   retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits, outcome>>

Finish ==
    /\ stage = "CallbackDone"
    /\ callbackDecision = "Continue"
    /\ PhaseResolved(1)
    /\ stage' = "Complete"
    /\ outcome' = IF failedFrames = 0
                      THEN "Committed" ELSE "CommittedDegraded"
    /\ UNCHANGED <<framePlan, routePresent, currentGeneration,
                   attemptGeneration, canceledWaiters, unavailableResults,
                   staleCancellationObserved, queueOpen, permitGeneration,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookAvailable, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackAvailable,
                   callbackDecision, callbackObserved, failedFrames,
                   retryCount, retryHeldPermits, retryCanceledWaiterCount, retryUnavailableHeldPermits, retryCanceledPermits>>

Done ==
    /\ stage = "Complete"
    /\ UNCHANGED vars

Next ==
    \/ \E r \in Recipients, p \in Phases : ReserveFrame(r, p)
    \/ \E r \in Recipients : LeaveRoom(r)
    \/ \E r \in Recipients : ChangeGeneration(r)
    \/ \E r \in Recipients : ResolveUnavailableSibling(r)
    \/ \E r \in Recipients, p \in Phases : CancelStaleReservation(r, p)
    \/ RetryCanceledReservation
    \/ \E r \in Recipients, replacement \in BOOLEAN :
           ReservationUnavailable(r, replacement)
    \/ AbortRoutingChanged
    \/ \E result \in {"Accepted", "Rejected", "Error"} : CallHook(result)
    \/ \E r \in Recipients : CloseAfterHook(r)
    \/ \E r \in Recipients : StaleAfterHook(r)
    \/ \E r \in Recipients, p \in Phases : SendFrame(r, p)
    \/ \E decision \in {"Continue", "Stop"} :
           CallPhaseZeroCallback(decision)
    \/ CallPhaseZeroCallbackAgain
    \/ Finish
    \/ Done

Spec == Init /\ [][Next]_vars

(* A deterministic witness proves the complete production retry chain, not *)
(* merely the existence of an isolated reset action. Weak fairness removes *)
(* specification stuttering from the finite scenario.                      *)
RetryScenarioInit ==
    /\ Init
    /\ framePlan = [r \in Recipients |-> Phases]

RetryScenarioSiblingsReserved ==
    \A f \in Frames :
        f = <<Actor, 1>> \/ frameState[f[1]][f[2]] = "Reserved"

RetryScenarioNext ==
    \/ /\ stage = "Reserving"
       /\ retryCount = 0
       /\ currentGeneration[Actor] = 0
       /\ ~RetryScenarioSiblingsReserved
       /\ \E r \in Recipients, p \in Phases :
              /\ <<r, p>> # <<Actor, 1>>
              /\ ReserveFrame(r, p)
    \/ /\ stage = "Reserving"
       /\ retryCount = 0
       /\ currentGeneration[Actor] = 0
       /\ RetryScenarioSiblingsReserved
       /\ ChangeGeneration(Actor)
    \/ /\ stage = "Reserving"
       /\ retryCount = 0
       /\ currentGeneration[Actor] = 1
       /\ canceledWaiters = {}
       /\ CancelStaleReservation(Actor, 1)
    \/ /\ stage = "Reserving"
       /\ retryCount = 0
       /\ canceledWaiters # {}
       /\ RetryCanceledReservation
    \/ /\ stage = "Reserving"
       /\ retryCount = 1
       /\ ~AllReserved
       /\ \E r \in Recipients, p \in Phases : ReserveFrame(r, p)
    \/ /\ stage = "Reserving"
       /\ retryCount = 1
       /\ AllReserved
       /\ CallHook("Accepted")
    \/ /\ stage = "Accepted"
       /\ ~PhaseResolved(0)
       /\ \E r \in Recipients : SendFrame(r, 0)
    \/ /\ stage = "Accepted"
       /\ PhaseResolved(0)
       /\ CallPhaseZeroCallback("Continue")
    \/ /\ stage = "CallbackDone"
       /\ ~PhaseResolved(1)
       /\ \E r \in Recipients : SendFrame(r, 1)
    \/ /\ stage = "CallbackDone"
       /\ PhaseResolved(1)
       /\ Finish
    \/ Done

RetryScenarioSpec ==
    RetryScenarioInit
    /\ [][RetryScenarioNext]_vars
    /\ WF_vars(RetryScenarioNext)

RetryScenarioCompletes ==
    <> (retryCount = 1 /\ outcome = "Committed")

---------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ framePlan \in [Recipients -> SUBSET Phases]
    /\ Cardinality(Frames) > 0
    /\ stage \in {"Reserving", "Accepted", "CallbackDone", "Complete"}
    /\ routePresent \in [Recipients -> BOOLEAN]
    /\ currentGeneration \in [Recipients -> Generation]
    /\ attemptGeneration \in [Recipients -> Generation]
    /\ canceledWaiters \in SUBSET Recipients
    /\ unavailableResults \in SUBSET Recipients
    /\ staleCancellationObserved \in BOOLEAN
    /\ queueOpen \in [Recipients -> BOOLEAN]
    /\ permitGeneration \in [Recipients -> 0..1]
    /\ frameState \in [Recipients -> [Phases -> FrameStates]]
    /\ reservedSlots \in 0..Cardinality(Frames)
    /\ permitProducers \in 0..Cardinality(Frames)
    /\ hookCalls \in 0..1
    /\ hookAvailable \in BOOLEAN
    /\ hookResult \in {"None", "Accepted", "Rejected", "Error"}
    /\ reservedAtHook \in {NoCount} \cup 0..Cardinality(Frames)
    /\ routingValidAtHook \in BOOLEAN
    /\ callbackCalls \in 0..2
    /\ callbackAvailable \in BOOLEAN
    /\ callbackDecision \in {"None", "Continue", "Stop"}
    /\ callbackObserved \in {NoCount} \cup 0..Cardinality(Recipients)
    /\ failedFrames \in 0..Cardinality(Frames)
    /\ retryCount \in 0..1
    /\ retryHeldPermits \in {NoCount} \cup 0..Cardinality(Frames)
    /\ retryCanceledWaiterCount \in {NoCount} \cup 0..Cardinality(Recipients)
    /\ retryUnavailableHeldPermits \in 0..Cardinality(Frames)
    /\ retryCanceledPermits \in
           0..(Cardinality(Frames) + Cardinality(Recipients))
    /\ outcome \in {"None"} \cup TerminalOutcomes

HookAfterExactReservation ==
    hookCalls = 1 => reservedAtHook = Cardinality(Frames)

HookAfterExactRoutingValidation ==
    hookCalls = 1 => routingValidAtHook

HookConsumptionExact ==
    hookAvailable <=> hookCalls = 0

CallbackRetentionExact ==
    /\ (stage # "Complete" /\ callbackCalls = 0 => callbackAvailable)
    /\ (callbackCalls > 0 => ~callbackAvailable)

RetrySnapshotFresh ==
    retryCount = 1 =>
        /\ staleCancellationObserved
        /\ canceledWaiters = {}
        /\ unavailableResults = {}
        /\ attemptGeneration = currentGeneration

RetryCancellationExact ==
    /\ (retryCount = 0 =>
           /\ retryHeldPermits = NoCount
           /\ retryCanceledWaiterCount = NoCount
           /\ retryCanceledPermits = 0)
    /\ (retryCount = 1 =>
           /\ retryHeldPermits \in 0..Cardinality(Frames)
           /\ retryCanceledWaiterCount \in 1..Cardinality(Recipients)
           /\ retryCanceledPermits =
                  retryHeldPermits
                  + retryCanceledWaiterCount
                  + retryUnavailableHeldPermits)

PhaseBoundaryExact ==
    \A r \in Recipients :
        1 \in framePlan[r]
        /\ frameState[r][1] \notin {"Unreserved", "Reserved", "Aborted"} =>
            /\ callbackCalls = 1
            /\ PhaseResolved(0)

CallbackExactlyOnce ==
    /\ callbackCalls <= 1
    /\ (outcome \in {"Committed", "CommittedDegraded"} => callbackCalls = 1)

CallbackPhaseZeroFailuresExact ==
    callbackCalls > 0 => callbackObserved = PhaseFailures(0)

FailedFrameAccountingExact ==
    failedFrames = Cardinality(FailedFrames)

PermitAccountingExact ==
    /\ reservedSlots = Cardinality(ReservedFrames)
    /\ permitProducers = Cardinality(ReservedFrames)

NoEnqueueBeforeHookAccepts ==
    (\E f \in Frames : frameState[f[1]][f[2]] = "Enqueued")
        => hookResult = "Accepted"

OutcomeExact ==
    /\ (outcome = "RoutingChanged" => hookCalls = 0)
    /\ (outcome = "HookRejected" => hookResult = "Rejected")
    /\ (outcome = "HookError" => hookResult = "Error")
    /\ (outcome = "Committed" =>
           /\ hookResult = "Accepted"
           /\ failedFrames = 0)
    /\ (outcome = "CommittedDegraded" =>
           /\ hookResult = "Accepted"
           /\ failedFrames > 0)
    /\ (stage = "Complete" =>
           /\ outcome \in TerminalOutcomes
           /\ reservedSlots = 0
           /\ permitProducers = 0)

=============================================================================

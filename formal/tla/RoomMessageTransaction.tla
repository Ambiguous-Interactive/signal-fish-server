---------------------- MODULE RoomMessageTransaction ----------------------
(***************************************************************************)
(* Exact-membership, two-phase room-message transaction (P74).             *)
(*                                                                         *)
(* The model fixes the production-critical shape at two recipients and two *)
(* frames per recipient. Reservations are acquired independently, but the  *)
(* durable hook may run only after all four permits exist and a final exact *)
(* member/connection-generation revalidation succeeds. Hook rejection and  *)
(* error are pre-durability aborts; neither may publish a frame.            *)
(*                                                                         *)
(* After hook acceptance, phase-zero permits resolve before the one-shot   *)
(* callback. The callback observes exactly the phase-zero failures. TRUE   *)
(* admits phase one; FALSE cancels every remaining phase-one permit.        *)
(* A receiver close can race the async hook. Synthetic scope invalidation   *)
(* additionally composes P73's stale-permit lemma at this transaction seam. *)
(* Every resulting failed frame remains exactly accounted.                 *)
(*                                                                         *)
(* Mapping to commit_room_messages_if_members_with_hook:                   *)
(*   ReserveFrame                 reserve_room_batch / DeliveryPermit      *)
(*   LeaveRoom / ChangeGeneration final routing + same-channel validation  *)
(*   AbortRoutingChanged          RoomMessageTransactionOutcome::          *)
(*                                  RoutingChanged                         *)
(*   CallHook                     before_send accept / reject / error      *)
(*   CloseAfterHook / StaleAfterHook                                      *)
(*                                post-hook permit.send outcomes            *)
(*   SendFrame                    phase-ordered permit.send                *)
(*   CallPhaseZeroCallback        after_first_phase(failed_phase_zero)     *)
(*   Finish                       Committed / CommittedDegraded            *)
(*                                                                         *)
(* Every Bug constant is FALSE in the healthy configuration. Independent   *)
(* expected-failure configurations set exactly one and pin the invariant   *)
(* that detects it.                                                        *)
(***************************************************************************)
EXTENDS FiniteSets, Integers, Naturals

CONSTANTS
    Recipients,
    Phases,
    PartialReservationBug,
    SkipRoutingValidationBug,
    PhaseBoundaryBug,
    DuplicateCallbackBug,
    MisreportCallbackBug,
    MiscountFailedFramesBug,
    OmitPermitReleaseBug

ASSUME /\ Cardinality(Recipients) = 2
       /\ Phases = {0, 1}
       /\ PartialReservationBug \in BOOLEAN
       /\ SkipRoutingValidationBug \in BOOLEAN
       /\ PhaseBoundaryBug \in BOOLEAN
       /\ DuplicateCallbackBug \in BOOLEAN
       /\ MisreportCallbackBug \in BOOLEAN
       /\ MiscountFailedFramesBug \in BOOLEAN
       /\ OmitPermitReleaseBug \in BOOLEAN

NoCount == -1
Frames == Recipients \X Phases
FrameStates ==
    {"Unreserved", "Reserved", "Enqueued", "Stale", "Closed",
     "Aborted", "Canceled"}
FailureStates == {"Stale", "Closed", "Canceled"}
TerminalOutcomes ==
    {"Committed", "CommittedDegraded", "RoutingChanged",
     "HookRejected", "HookError"}

VARIABLES
    stage,                    \* Reserving / Accepted / CallbackDone / Complete
    routePresent,             \* exact expected-member snapshot
    routeGeneration,          \* 0 is the captured connection generation
    queueOpen,                \* receiver can close during the async hook
    permitGeneration,         \* synthetic import of P73 stale-permit behavior
    frameState,               \* lifecycle of each recipient/phase frame
    reservedSlots,            \* queue capacity claims held by permits
    permitProducers,          \* permit-backed producer capabilities
    hookCalls,
    hookResult,               \* None / Accepted / Rejected / Error
    reservedAtHook,           \* exact pre-hook reservation snapshot
    routingValidAtHook,       \* exact final routing validation snapshot
    callbackCalls,
    callbackDecision,         \* None / Continue / Stop
    callbackObserved,         \* callback's phase-zero failed-frame argument
    failedFrames,             \* returned CommittedDegraded count
    outcome

vars ==
    <<stage, routePresent, routeGeneration, queueOpen, permitGeneration,
      frameState, reservedSlots, permitProducers, hookCalls, hookResult,
      reservedAtHook, routingValidAtHook, callbackCalls, callbackDecision,
      callbackObserved, failedFrames, outcome>>

ReservedFrames ==
    {f \in Frames : frameState[f[1]][f[2]] = "Reserved"}

FailedFrames ==
    {f \in Frames : frameState[f[1]][f[2]] \in FailureStates}

PhaseFailures(p) ==
    Cardinality({r \in Recipients : frameState[r][p] \in FailureStates})

PhaseResolved(p) ==
    \A r \in Recipients :
        frameState[r][p] \notin {"Unreserved", "Reserved"}

ExactRouting ==
    \A r \in Recipients : routePresent[r] /\ routeGeneration[r] = 0

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

CancelPhaseOne ==
    [r \in Recipients |->
        [p \in Phases |->
            IF p = 1 /\ frameState[r][p] = "Reserved"
                THEN "Canceled"
                ELSE frameState[r][p]]]

Init ==
    /\ stage = "Reserving"
    /\ routePresent = [r \in Recipients |-> TRUE]
    /\ routeGeneration = [r \in Recipients |-> 0]
    /\ queueOpen = [r \in Recipients |-> TRUE]
    /\ permitGeneration = [r \in Recipients |-> 0]
    /\ frameState = [r \in Recipients |-> [p \in Phases |-> "Unreserved"]]
    /\ reservedSlots = 0
    /\ permitProducers = 0
    /\ hookCalls = 0
    /\ hookResult = "None"
    /\ reservedAtHook = NoCount
    /\ routingValidAtHook = FALSE
    /\ callbackCalls = 0
    /\ callbackDecision = "None"
    /\ callbackObserved = NoCount
    /\ failedFrames = 0
    /\ outcome = "None"

(* Reservations may complete in any recipient/frame order. No hook or send  *)
(* is folded into this awaited acquisition action.                         *)
ReserveFrame(r, p) ==
    /\ stage = "Reserving"
    /\ frameState[r][p] = "Unreserved"
    /\ frameState' = [frameState EXCEPT ![r][p] = "Reserved"]
    /\ reservedSlots' = reservedSlots + 1
    /\ permitProducers' = permitProducers + 1
    /\ UNCHANGED <<stage, routePresent, routeGeneration, queueOpen,
                   permitGeneration, hookCalls, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackDecision,
                   callbackObserved, failedFrames, outcome>>

(* Membership and sender identity can change while capacity waits complete. *)
LeaveRoom(r) ==
    /\ stage = "Reserving"
    /\ routePresent[r]
    /\ routePresent' = [routePresent EXCEPT ![r] = FALSE]
    /\ UNCHANGED <<stage, routeGeneration, queueOpen, permitGeneration,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackDecision, callbackObserved,
                   failedFrames, outcome>>

ChangeGeneration(r) ==
    /\ stage = "Reserving"
    /\ routeGeneration[r] = 0
    /\ routeGeneration' = [routeGeneration EXCEPT ![r] = 1]
    /\ UNCHANGED <<stage, routePresent, queueOpen, permitGeneration,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackDecision, callbackObserved,
                   failedFrames, outcome>>

(* A failed final revalidation releases every pre-hook reservation and does  *)
(* not consume either callback.                                            *)
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
    /\ UNCHANGED <<routePresent, routeGeneration, queueOpen,
                   permitGeneration, hookCalls, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackDecision,
                   callbackObserved, failedFrames>>

(* The seeded reservation bug permits the hook after a strict nonempty       *)
(* subset. The routing bug permits it after a stale member/generation check. *)
CallHook(result) ==
    /\ stage = "Reserving"
    /\ result \in {"Accepted", "Rejected", "Error"}
    /\ hookCalls = 0
    /\ Cardinality(ReservedFrames) > 0
    /\ (AllReserved \/ PartialReservationBug)
    /\ (ExactRouting \/ SkipRoutingValidationBug)
    /\ hookCalls' = 1
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
    /\ UNCHANGED <<routePresent, routeGeneration, queueOpen,
                   permitGeneration, callbackCalls, callbackDecision,
                   callbackObserved, failedFrames>>

(* Receiver close is a live transaction race. Scope invalidation is a       *)
(* synthetic composition action importing P73's proved queue behavior; the *)
(* transaction's routing read guards block the normal registration path.   *)
CloseAfterHook(r) ==
    /\ stage \in {"Accepted", "CallbackDone"}
    /\ queueOpen[r]
    /\ queueOpen' = [queueOpen EXCEPT ![r] = FALSE]
    /\ UNCHANGED <<stage, routePresent, routeGeneration, permitGeneration,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackDecision, callbackObserved,
                   failedFrames, outcome>>

StaleAfterHook(r) ==
    /\ stage \in {"Accepted", "CallbackDone"}
    /\ permitGeneration[r] = 0
    /\ permitGeneration' = [permitGeneration EXCEPT ![r] = 1]
    /\ UNCHANGED <<stage, routePresent, routeGeneration, queueOpen,
                   frameState, reservedSlots, permitProducers, hookCalls,
                   hookResult, reservedAtHook, routingValidAtHook,
                   callbackCalls, callbackDecision, callbackObserved,
                   failedFrames, outcome>>

SendResult(r) ==
    IF ~queueOpen[r] THEN "Closed"
    ELSE IF permitGeneration[r] # 0 THEN "Stale"
    ELSE "Enqueued"

(* Phase zero is admitted after hook acceptance. Phase one is admitted only  *)
(* after a TRUE callback, except in the independently seeded boundary bug.  *)
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
    /\ UNCHANGED <<stage, routePresent, routeGeneration, queueOpen,
                   permitGeneration, hookCalls, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackDecision,
                   callbackObserved, outcome>>

(* Both callback decisions are reachable. FALSE converts every held phase-1 *)
(* permit into an exactly counted post-durability cancellation.             *)
CallPhaseZeroCallback(decision) ==
    /\ stage = "Accepted"
    /\ PhaseResolved(0)
    /\ callbackCalls = 0
    /\ decision \in {"Continue", "Stop"}
    /\ callbackCalls' = 1
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
               IN /\ stage' = "Complete"
                  /\ frameState' = CancelPhaseOne
                  /\ reservedSlots' = ReleasedCounter(reservedSlots, canceled)
                  /\ permitProducers' =
                         ReleasedCounter(permitProducers, canceled)
                  /\ failedFrames' = failedFrames + CountFailures(canceled)
                  /\ outcome' = "CommittedDegraded"
    /\ UNCHANGED <<routePresent, routeGeneration, queueOpen,
                   permitGeneration, hookCalls, hookResult, reservedAtHook,
                   routingValidAtHook>>

(* FnOnce makes this impossible in Rust; the action is solely a non-vacuity  *)
(* witness for CallbackExactlyOnce.                                        *)
CallPhaseZeroCallbackAgain ==
    /\ DuplicateCallbackBug
    /\ stage = "CallbackDone"
    /\ callbackCalls = 1
    /\ callbackCalls' = 2
    /\ UNCHANGED <<stage, routePresent, routeGeneration, queueOpen,
                   permitGeneration, frameState, reservedSlots,
                   permitProducers, hookCalls, hookResult, reservedAtHook,
                   routingValidAtHook, callbackDecision, callbackObserved,
                   failedFrames, outcome>>

Finish ==
    /\ stage = "CallbackDone"
    /\ callbackDecision = "Continue"
    /\ PhaseResolved(1)
    /\ stage' = "Complete"
    /\ outcome' = IF failedFrames = 0
                      THEN "Committed" ELSE "CommittedDegraded"
    /\ UNCHANGED <<routePresent, routeGeneration, queueOpen,
                   permitGeneration, frameState, reservedSlots,
                   permitProducers, hookCalls, hookResult, reservedAtHook,
                   routingValidAtHook, callbackCalls, callbackDecision,
                   callbackObserved, failedFrames>>

Done ==
    /\ stage = "Complete"
    /\ UNCHANGED vars

Next ==
    \/ \E r \in Recipients, p \in Phases : ReserveFrame(r, p)
    \/ \E r \in Recipients : LeaveRoom(r)
    \/ \E r \in Recipients : ChangeGeneration(r)
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

---------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ stage \in {"Reserving", "Accepted", "CallbackDone", "Complete"}
    /\ routePresent \in [Recipients -> BOOLEAN]
    /\ routeGeneration \in [Recipients -> 0..1]
    /\ queueOpen \in [Recipients -> BOOLEAN]
    /\ permitGeneration \in [Recipients -> 0..1]
    /\ frameState \in [Recipients -> [Phases -> FrameStates]]
    /\ reservedSlots \in 0..Cardinality(Frames)
    /\ permitProducers \in 0..Cardinality(Frames)
    /\ hookCalls \in 0..1
    /\ hookResult \in {"None", "Accepted", "Rejected", "Error"}
    /\ reservedAtHook \in {NoCount} \cup 0..Cardinality(Frames)
    /\ routingValidAtHook \in BOOLEAN
    /\ callbackCalls \in 0..2
    /\ callbackDecision \in {"None", "Continue", "Stop"}
    /\ callbackObserved \in {NoCount} \cup 0..Cardinality(Recipients)
    /\ failedFrames \in 0..Cardinality(Frames)
    /\ outcome \in {"None"} \cup TerminalOutcomes

HookAfterExactReservation ==
    hookCalls = 1 => reservedAtHook = Cardinality(Frames)

HookAfterExactRoutingValidation ==
    hookCalls = 1 => routingValidAtHook

PhaseBoundaryExact ==
    \A r \in Recipients :
        frameState[r][1] \notin {"Unreserved", "Reserved", "Aborted"} =>
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

NoPublicationBeforeHookAccepts ==
    (\E f \in Frames :
        frameState[f[1]][f[2]] \in {"Enqueued", "Stale", "Closed", "Canceled"})
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

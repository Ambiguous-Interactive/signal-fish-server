------------------------ MODULE RoomEventSequencer ------------------------
(***************************************************************************)
(* Process-local per-room mutation/event handoff (P76).                    *)
(*                                                                         *)
(* The production RoomEventSequencer does not admit an ordinary backlog of  *)
(* independently mutated jobs. A producer first owns the room's mutation    *)
(* gate, performs its state mutation, and synchronously transfers that same *)
(* guard into one owned job. The guard remains held while the job is queued *)
(* and running. Only after success, error, or isolated panic drops the guard *)
(* may the next same-room producer mutate.                                  *)
(*                                                                         *)
(* A caller may drop its oneshot completion receiver after enqueue without  *)
(* canceling the owned job. The lane worker isolates the job in a child task *)
(* so a panic cannot leave `running = true` forever. The weak lane registry  *)
(* also uses pointer identity when an old lane's destructor overlaps a new  *)
(* acquisition: stale cleanup must not remove the replacement lane.         *)
(*                                                                         *)
(* Mapping to src/coordination/mod.rs:                                      *)
(*   AcquireGuard       RoomEventSequencer::lock / OwnedMutexGuard          *)
(*   Mutate             room-scoped state mutation under the guard          *)
(*   Enqueue            synchronous guard transfer into RoomEventJob        *)
(*   StartJob           RoomEventLane::drain pop + isolated tokio::spawn     *)
(*   Complete*          child completion / JoinError followed by guard drop  *)
(*   DropCompletion     caller drops the oneshot receiver                    *)
(*   Begin/FinishDrop   weak-registry expiry and pointer-identity cleanup    *)
(*                                                                         *)
(* The four Bug constants are FALSE in the healthy configuration. Each      *)
(* expected-failure configuration enables exactly one independently.        *)
(***************************************************************************)
EXTENDS FiniteSets, Integers, Naturals, Sequences, TLC

CONSTANTS
    Events,
    FirstEvent,
    SecondEvent,
    ReleaseGuardAtEnqueueBug,
    DropCompletionCancelsBug,
    PanicStrandsLaneBug,
    StaleDropRemovesReplacementBug

ASSUME /\ Cardinality(Events) = 2
       /\ FirstEvent \in Events
       /\ SecondEvent \in Events
       /\ FirstEvent /= SecondEvent
       /\ ReleaseGuardAtEnqueueBug \in BOOLEAN
       /\ DropCompletionCancelsBug \in BOOLEAN
       /\ PanicStrandsLaneBug \in BOOLEAN
       /\ StaleDropRemovesReplacementBug \in BOOLEAN

NoEvent == "NoEvent"
NoLane == 0
LaneGenerations == 1..3
TerminalPhases == {"Succeeded", "Failed", "Panicked"}
LiveJobPhases == {"Queued", "Running"}
AllPhases ==
    {"Pending", "Guarded", "Mutated", "Queued", "Running",
     "Succeeded", "Failed", "Panicked", "AbortedBeforeMutation",
     "CanceledByCaller"}

VARIABLES
    phase,                  \* per-event mutation/job lifecycle
    eventLane,              \* lane generation captured by each guard/job
    gateOwner,              \* the one same-room OwnedMutexGuard owner
    gateLane,
    queue,                  \* at most the guard-owning event under this contract
    activeJob,
    workerRunning,
    workerStranded,         \* seeded panic bug: running flag without a drainer
    callerAttached,         \* whether the oneshot receiver is retained
    mutationOrder,
    admissionOrder,
    startOrder,
    terminalOrder,
    registryLane,           \* weak registry entry's lane generation
    registryUpgradeable,    \* FALSE while the old lane destructor is pending
    droppingLane,
    nextLane,
    replacementInstalled

vars ==
    <<phase, eventLane, gateOwner, gateLane, queue, activeJob,
      workerRunning, workerStranded, callerAttached, mutationOrder, admissionOrder,
      startOrder, terminalOrder, registryLane, registryUpgradeable,
      droppingLane, nextLane, replacementInstalled>>

SeqSet(sequence) == {sequence[index] : index \in 1..Len(sequence)}

IsPrefix(prefix, sequence) ==
    /\ Len(prefix) <= Len(sequence)
    /\ \A index \in 1..Len(prefix) : prefix[index] = sequence[index]

Init ==
    /\ phase = [event \in Events |-> "Pending"]
    /\ eventLane = [event \in Events |-> NoLane]
    /\ gateOwner = NoEvent
    /\ gateLane = NoLane
    /\ queue = <<>>
    /\ activeJob = NoEvent
    /\ workerRunning = FALSE
    /\ workerStranded = FALSE
    /\ callerAttached = [event \in Events |-> TRUE]
    /\ mutationOrder = <<>>
    /\ admissionOrder = <<>>
    /\ startOrder = <<>>
    /\ terminalOrder = <<>>
    /\ registryLane = NoLane
    /\ registryUpgradeable = FALSE
    /\ droppingLane = NoLane
    /\ nextLane = 1
    /\ replacementInstalled = FALSE

AcquireGuard(event) ==
    /\ event \in Events
    /\ phase[event] = "Pending"
    /\ gateOwner = NoEvent
    /\ IF registryLane /= NoLane /\ registryUpgradeable
          THEN
            /\ phase' = [phase EXCEPT ![event] = "Guarded"]
            /\ eventLane' = [eventLane EXCEPT ![event] = registryLane]
            /\ gateOwner' = event
            /\ gateLane' = registryLane
            /\ UNCHANGED <<registryLane, registryUpgradeable, nextLane,
                            replacementInstalled>>
          ELSE
            /\ nextLane \in LaneGenerations
            /\ phase' = [phase EXCEPT ![event] = "Guarded"]
            /\ eventLane' = [eventLane EXCEPT ![event] = nextLane]
            /\ gateOwner' = event
            /\ gateLane' = nextLane
            /\ registryLane' = nextLane
            /\ registryUpgradeable' = TRUE
            /\ nextLane' = nextLane + 1
            /\ replacementInstalled' =
                  (replacementInstalled \/ droppingLane /= NoLane)
    /\ UNCHANGED <<queue, activeJob, workerRunning, workerStranded, callerAttached,
                    mutationOrder, admissionOrder, startOrder, terminalOrder,
                    droppingLane>>

Mutate(event) ==
    /\ event \in Events
    /\ phase[event] = "Guarded"
    /\ gateOwner = event
    /\ phase' = [phase EXCEPT ![event] = "Mutated"]
    /\ mutationOrder' = Append(mutationOrder, event)
    /\ UNCHANGED <<eventLane, gateOwner, gateLane, queue, activeJob,
                    workerRunning, workerStranded, callerAttached, admissionOrder, startOrder,
                    terminalOrder, registryLane, registryUpgradeable,
                    droppingLane, nextLane, replacementInstalled>>

AbortBeforeMutation(event) ==
    /\ event \in Events
    /\ phase[event] = "Guarded"
    /\ gateOwner = event
    /\ phase' = [phase EXCEPT ![event] = "AbortedBeforeMutation"]
    /\ gateOwner' = NoEvent
    /\ gateLane' = NoLane
    /\ UNCHANGED <<eventLane, queue, activeJob, workerRunning, workerStranded,
                    callerAttached, mutationOrder, admissionOrder, startOrder,
                    terminalOrder, registryLane, registryUpgradeable,
                    droppingLane, nextLane, replacementInstalled>>

Enqueue(event) ==
    /\ event \in Events
    /\ phase[event] = "Mutated"
    /\ gateOwner = event
    /\ phase' = [phase EXCEPT ![event] = "Queued"]
    /\ queue' = Append(queue, event)
    /\ workerRunning' = TRUE
    /\ admissionOrder' = Append(admissionOrder, event)
    /\ IF ReleaseGuardAtEnqueueBug
          THEN /\ gateOwner' = NoEvent
               /\ gateLane' = NoLane
          ELSE /\ UNCHANGED <<gateOwner, gateLane>>
    /\ UNCHANGED <<eventLane, activeJob, workerStranded, callerAttached, mutationOrder,
                    startOrder, terminalOrder, registryLane,
                    registryUpgradeable, droppingLane, nextLane,
                    replacementInstalled>>

StartJob ==
    /\ workerRunning
    /\ ~workerStranded
    /\ activeJob = NoEvent
    /\ Len(queue) > 0
    /\ LET event == Head(queue) IN
       /\ phase[event] = "Queued"
       /\ phase' = [phase EXCEPT ![event] = "Running"]
       /\ queue' = Tail(queue)
       /\ activeJob' = event
       /\ startOrder' = Append(startOrder, event)
    /\ UNCHANGED <<eventLane, gateOwner, gateLane, workerRunning, workerStranded,
                    callerAttached, mutationOrder, admissionOrder,
                    terminalOrder, registryLane, registryUpgradeable,
                    droppingLane, nextLane, replacementInstalled>>

CompleteJob(event, result) ==
    /\ event \in Events
    /\ result \in TerminalPhases
    /\ activeJob = event
    /\ phase[event] = "Running"
    /\ phase' = [phase EXCEPT ![event] = result]
    /\ activeJob' = NoEvent
    /\ terminalOrder' = Append(terminalOrder, event)
    /\ gateOwner' = NoEvent
    /\ gateLane' = NoLane
    /\ workerRunning' = TRUE
    /\ workerStranded' = (result = "Panicked" /\ PanicStrandsLaneBug)
    /\ UNCHANGED <<eventLane, queue, callerAttached, mutationOrder,
                    admissionOrder, startOrder, registryLane,
                    registryUpgradeable, droppingLane, nextLane,
                    replacementInstalled>>

CompleteSuccess(event) == CompleteJob(event, "Succeeded")
CompleteError(event) == CompleteJob(event, "Failed")
CompletePanic(event) == CompleteJob(event, "Panicked")

StopIdleWorker ==
    /\ workerRunning
    /\ ~workerStranded
    /\ activeJob = NoEvent
    /\ Len(queue) = 0
    /\ workerRunning' = FALSE
    /\ UNCHANGED <<phase, eventLane, gateOwner, gateLane, queue,
                    activeJob, workerStranded, callerAttached, mutationOrder,
                    admissionOrder, startOrder, terminalOrder, registryLane,
                    registryUpgradeable, droppingLane, nextLane,
                    replacementInstalled>>

DropCompletion(event) ==
    /\ event \in Events
    /\ callerAttached[event]
    \* A post-terminal receiver drop is behaviorless. Model the cancellation-
    \* relevant case while the lane still owns queued/running work.
    /\ phase[event] \in LiveJobPhases
    /\ callerAttached' = [callerAttached EXCEPT ![event] = FALSE]
    /\ IF DropCompletionCancelsBug /\ phase[event] \in LiveJobPhases
          THEN
            /\ phase' = [phase EXCEPT ![event] = "CanceledByCaller"]
            /\ queue' = IF phase[event] = "Queued" THEN <<>> ELSE queue
            /\ activeJob' = NoEvent
            /\ workerRunning' = FALSE
            /\ workerStranded' = FALSE
            /\ gateOwner' = NoEvent
            /\ gateLane' = NoLane
          ELSE
            /\ UNCHANGED <<phase, queue, activeJob, workerRunning, workerStranded,
                            gateOwner, gateLane>>
    /\ UNCHANGED <<eventLane, mutationOrder, admissionOrder, startOrder,
                    terminalOrder, registryLane, registryUpgradeable,
                    droppingLane, nextLane, replacementInstalled>>

BeginLaneDrop ==
    /\ registryLane /= NoLane
    /\ registryUpgradeable
    /\ droppingLane = NoLane
    /\ gateOwner = NoEvent
    /\ activeJob = NoEvent
    /\ Len(queue) = 0
    /\ ~workerRunning
    /\ droppingLane' = registryLane
    /\ registryUpgradeable' = FALSE
    /\ UNCHANGED <<phase, eventLane, gateOwner, gateLane, queue,
                    activeJob, workerRunning, workerStranded, callerAttached, mutationOrder,
                    admissionOrder, startOrder, terminalOrder, registryLane,
                    nextLane, replacementInstalled>>

FinishLaneDrop ==
    /\ droppingLane /= NoLane
    /\ IF StaleDropRemovesReplacementBug
          THEN /\ registryLane' = NoLane
               /\ registryUpgradeable' = FALSE
          ELSE IF registryLane = droppingLane /\ ~registryUpgradeable
                  THEN /\ registryLane' = NoLane
                       /\ registryUpgradeable' = FALSE
                  ELSE /\ UNCHANGED <<registryLane, registryUpgradeable>>
    /\ droppingLane' = NoLane
    /\ UNCHANGED <<phase, eventLane, gateOwner, gateLane, queue,
                    activeJob, workerRunning, workerStranded, callerAttached, mutationOrder,
                    admissionOrder, startOrder, terminalOrder, nextLane,
                    replacementInstalled>>

Done ==
    /\ \A event \in Events : phase[event] \in
           (TerminalPhases \cup {"AbortedBeforeMutation", "CanceledByCaller"})
    /\ activeJob = NoEvent
    /\ Len(queue) = 0
    /\ ~workerRunning
    /\ UNCHANGED vars

Next ==
    \/ \E event \in Events : AcquireGuard(event)
    \/ \E event \in Events : Mutate(event)
    \/ \E event \in Events : AbortBeforeMutation(event)
    \/ \E event \in Events : Enqueue(event)
    \/ StartJob
    \/ \E event \in Events : CompleteSuccess(event)
    \/ \E event \in Events : CompleteError(event)
    \/ \E event \in Events : CompletePanic(event)
    \/ StopIdleWorker
    \/ \E event \in Events : DropCompletion(event)
    \/ BeginLaneDrop
    \/ FinishLaneDrop
    \/ Done

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ phase \in [Events -> AllPhases]
    /\ eventLane \in [Events -> (LaneGenerations \cup {NoLane})]
    /\ gateOwner \in Events \cup {NoEvent}
    /\ gateLane \in LaneGenerations \cup {NoLane}
    /\ queue \in Seq(Events)
    /\ activeJob \in Events \cup {NoEvent}
    /\ workerRunning \in BOOLEAN
    /\ workerStranded \in BOOLEAN
    /\ callerAttached \in [Events -> BOOLEAN]
    /\ mutationOrder \in Seq(Events)
    /\ admissionOrder \in Seq(Events)
    /\ startOrder \in Seq(Events)
    /\ terminalOrder \in Seq(Events)
    /\ registryLane \in LaneGenerations \cup {NoLane}
    /\ registryUpgradeable \in BOOLEAN
    /\ droppingLane \in LaneGenerations \cup {NoLane}
    /\ nextLane \in 1..4
    /\ replacementInstalled \in BOOLEAN

SingleOccurrence ==
    /\ Len(mutationOrder) = Cardinality(SeqSet(mutationOrder))
    /\ Len(admissionOrder) = Cardinality(SeqSet(admissionOrder))
    /\ Len(startOrder) = Cardinality(SeqSet(startOrder))
    /\ Len(terminalOrder) = Cardinality(SeqSet(terminalOrder))

HandoffOrderExact ==
    /\ IsPrefix(admissionOrder, mutationOrder)
    /\ IsPrefix(startOrder, admissionOrder)
    /\ IsPrefix(terminalOrder, startOrder)

GuardHeldThroughOwnedJob ==
    \A event \in Events : phase[event] \in LiveJobPhases =>
        gateOwner = event /\ gateLane = eventLane[event]

NoMutatedEventLost ==
    \A event \in SeqSet(mutationOrder) :
        phase[event] \in ({"Mutated"} \cup LiveJobPhases \cup TerminalPhases)

SameRoomMutationBarrier ==
    \A later \in 2..Len(mutationOrder) :
        phase[mutationOrder[later - 1]] \in TerminalPhases

QueueAndActiveExact ==
    /\ Len(queue) <= 1
    /\ SeqSet(queue) = {event \in Events : phase[event] = "Queued"}
    /\ (activeJob = NoEvent) =
          ({event \in Events : phase[event] = "Running"} = {})
    /\ activeJob /= NoEvent => phase[activeJob] = "Running"

WorkerStateSound ==
    /\ ~workerRunning => activeJob = NoEvent /\ Len(queue) = 0
    /\ activeJob /= NoEvent => workerRunning /\ ~workerStranded
    /\ workerStranded => workerRunning /\ activeJob = NoEvent

NoStrandedWorker == ~workerStranded

RegistryProtectsActiveGuard ==
    gateOwner /= NoEvent =>
        registryLane = gateLane /\ registryUpgradeable

Healthy ==
    /\ TypeOK
    /\ SingleOccurrence
    /\ HandoffOrderExact
    /\ GuardHeldThroughOwnedJob
    /\ NoMutatedEventLost
    /\ SameRoomMutationBarrier
    /\ QueueAndActiveExact
    /\ WorkerStateSound
    /\ NoStrandedWorker
    /\ RegistryProtectsActiveGuard

RecoveryScenarioNotReached ==
    ~(phase[FirstEvent] = "Panicked"
      /\ ~callerAttached[FirstEvent]
      /\ phase[SecondEvent] = "Succeeded"
      /\ terminalOrder = <<FirstEvent, SecondEvent>>
      /\ replacementInstalled
      /\ droppingLane = NoLane)

=============================================================================

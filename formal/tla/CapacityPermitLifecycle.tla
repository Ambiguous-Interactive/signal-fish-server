-------------------- MODULE CapacityPermitLifecycle --------------------
(***************************************************************************)
(* Classified control-permit lifecycle conservation (#220 / P73).          *)
(*                                                                         *)
(* CapacityDeadlineArbitration.tla stops when a waiter atomically claims a  *)
(* queue slot. This model follows two independently held permits through    *)
(* ordinary-control and transition commit, explicit Drop, receiver close,  *)
(* accountability failure, last-sender drop, generation change, and an     *)
(* already-waiting receiver's notification boundary.                       *)
(*                                                                         *)
(* Mapping to src/coordination/outbound_queue.rs and coordination/mod.rs:   *)
(*   ReserveCurrent / ReserveNext       DeliverySender reservation          *)
(*   SenderDrop / ProducersOpen         sender_count / permit_count         *)
(*   ReceiverClose / FailAccountability QueueState::accepting               *)
(*   PermitCommit*                     DeliveryPermit::send and             *)
(*                                      OutboundPermit::send_control_inner   *)
(*   PermitDrop                        OutboundPermit::drop                  *)
(*   ReceiverPoll / ReceiverResume     OutboundReceiver::recv / Notify      *)
(*                                                                         *)
(* The two-permit bound matches the production room transaction, which can  *)
(* hold two phase permits per recipient. A transition permit can commit     *)
(* while an ordinary permit retains the old scope. Every consuming path     *)
(* must release both counters exactly once; a final permit must notify an   *)
(* already-waiting receiver when it commits or ceases to be a producer.     *)
(***************************************************************************)
EXTENDS FiniteSets, Integers, Naturals, Sequences

CONSTANTS
    PermitIds,
    QueueCapacity,
    OmitPermitProducerBug,
    OmitDropReleaseBug,
    OmitFailedReleaseBug,
    OmitStaleReleaseBug,
    SkipScopeValidationBug,
    OmitPermitWakeBug

ASSUME /\ Cardinality(PermitIds) = 2
       /\ QueueCapacity = 2
       /\ OmitPermitProducerBug \in BOOLEAN
       /\ OmitDropReleaseBug \in BOOLEAN
       /\ OmitFailedReleaseBug \in BOOLEAN
       /\ OmitStaleReleaseBug \in BOOLEAN
       /\ SkipScopeValidationBug \in BOOLEAN
       /\ OmitPermitWakeBug \in BOOLEAN

NoGeneration == -1
NoRoom == "NoRoom"
AnyRoom == "AnyRoom"

VARIABLES
    queue,                    \* sequence of committed permit identities
    receiverOpen,             \* QueueState::receiver_open
    accountabilityFailed,    \* QueueState::accountability_failed
    senderCount,              \* one original sender, then zero
    reserved,                 \* QueueState::reserved_control
    permitCount,              \* permit-backed producer capabilities
    lifecycle,                \* per-permit Ready/Held/terminal state
    permitScope,              \* current or next-generation reservation path
    expectedGeneration,       \* scope captured by DeliveryPermit
    expectedRoom,             \* exact room or wildcard captured at reserve
    committedGeneration,      \* scope observed by successful commit
    committedRoom,
    enqueueGeneration,
    activeRoom,
    receiverWaiting,          \* recv observed Empty and parked on Notify
    wakePending,              \* a relevant producer action notified it
    eof,
    lastPoll

vars == <<queue, receiverOpen, accountabilityFailed, senderCount, reserved,
          permitCount, lifecycle, permitScope, expectedGeneration, expectedRoom,
          committedGeneration, committedRoom, enqueueGeneration, activeRoom,
          receiverWaiting, wakePending, eof, lastPoll>>

Held == {p \in PermitIds : lifecycle[p] = "Held"}

Accepting == receiverOpen /\ ~accountabilityFailed

ProducersOpen ==
    senderCount > 0
    \/ /\ ~OmitPermitProducerBug
       /\ permitCount > 0

HasCapacity == Len(queue) + reserved < QueueCapacity

ScopeMatches(p) ==
    expectedGeneration[p] = enqueueGeneration
    /\ (expectedRoom[p] = AnyRoom \/ expectedRoom[p] = activeRoom)

QueueOccurrences(p) ==
    Cardinality({i \in 1..Len(queue) : queue[i] = p})

Init ==
    /\ queue = <<>>
    /\ receiverOpen = TRUE
    /\ accountabilityFailed = FALSE
    /\ senderCount = 1
    /\ reserved = 0
    /\ permitCount = 0
    /\ lifecycle = [p \in PermitIds |-> "Ready"]
    /\ permitScope = [p \in PermitIds |-> "None"]
    /\ expectedGeneration = [p \in PermitIds |-> NoGeneration]
    /\ expectedRoom = [p \in PermitIds |-> NoRoom]
    /\ committedGeneration = [p \in PermitIds |-> NoGeneration]
    /\ committedRoom = [p \in PermitIds |-> NoRoom]
    /\ enqueueGeneration = 0
    /\ activeRoom = "A"
    /\ receiverWaiting = FALSE
    /\ wakePending = FALSE
    /\ eof = FALSE
    /\ lastPoll = "None"

ReserveCurrent(p, roomScope) ==
    /\ p \in PermitIds
    /\ roomScope \in {activeRoom, AnyRoom}
    /\ lifecycle[p] = "Ready"
    /\ Accepting
    /\ senderCount > 0
    /\ HasCapacity
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Held"]
    /\ permitScope' = [permitScope EXCEPT ![p] = "Current"]
    /\ expectedGeneration' = [expectedGeneration EXCEPT ![p] = enqueueGeneration]
    /\ expectedRoom' = [expectedRoom EXCEPT ![p] = roomScope]
    /\ reserved' = reserved + 1
    /\ permitCount' = permitCount + 1
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, senderCount,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, wakePending, eof, lastPoll>>

(* The initial lifecycle transition reserves next-generation wildcard scope. *)
ReserveNext(p) ==
    /\ p \in PermitIds
    /\ lifecycle[p] = "Ready"
    /\ Accepting
    /\ senderCount > 0
    /\ enqueueGeneration = 0
    /\ HasCapacity
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Held"]
    /\ permitScope' = [permitScope EXCEPT ![p] = "Next"]
    /\ expectedGeneration' = [expectedGeneration EXCEPT ![p] = 1]
    /\ expectedRoom' = [expectedRoom EXCEPT ![p] = AnyRoom]
    /\ reserved' = reserved + 1
    /\ permitCount' = permitCount + 1
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, senderCount,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, wakePending, eof, lastPoll>>

SenderDrop ==
    /\ senderCount = 1
    /\ senderCount' = 0
    /\ wakePending' = IF receiverWaiting /\ permitCount = 0
                          THEN TRUE ELSE wakePending
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, reserved,
                   permitCount, lifecycle, permitScope, expectedGeneration,
                   expectedRoom, committedGeneration, committedRoom,
                   enqueueGeneration, activeRoom, receiverWaiting, eof, lastPoll>>

ReceiverClose ==
    /\ receiverOpen
    /\ receiverOpen' = FALSE
    /\ wakePending' = IF receiverWaiting THEN TRUE ELSE wakePending
    /\ UNCHANGED <<queue, accountabilityFailed, senderCount, reserved,
                   permitCount, lifecycle, permitScope, expectedGeneration,
                   expectedRoom, committedGeneration, committedRoom,
                   enqueueGeneration, activeRoom, receiverWaiting, eof, lastPoll>>

(* Another live producer can make accountability terminal while permits are *)
(* held. This is the exact composition exercised by the Rust regression.    *)
FailAccountability ==
    /\ Accepting
    /\ senderCount > 0
    /\ Held # {}
    /\ accountabilityFailed' = TRUE
    /\ wakePending' = IF receiverWaiting THEN TRUE ELSE wakePending
    /\ UNCHANGED <<queue, receiverOpen, senderCount, reserved, permitCount,
                   lifecycle, permitScope, expectedGeneration, expectedRoom,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, eof, lastPoll>>

PermitDrop(p) ==
    /\ p \in Held
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Canceled"]
    /\ IF OmitDropReleaseBug
          THEN /\ UNCHANGED reserved
               /\ UNCHANGED permitCount
          ELSE /\ reserved' = reserved - 1
               /\ permitCount' = permitCount - 1
    /\ wakePending' =
          IF receiverWaiting
             /\ senderCount = 0
             /\ Cardinality(Held \ {p}) = 0
             /\ ~OmitPermitWakeBug
          THEN TRUE
          ELSE wakePending
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, senderCount,
                   permitScope, expectedGeneration, expectedRoom,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, eof, lastPoll>>

PermitCommitFailed(p) ==
    /\ p \in Held
    /\ ~Accepting
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Failed"]
    /\ IF OmitFailedReleaseBug
          THEN /\ UNCHANGED reserved
               /\ UNCHANGED permitCount
          ELSE /\ reserved' = reserved - 1
               /\ permitCount' = permitCount - 1
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, senderCount,
                   permitScope, expectedGeneration, expectedRoom,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, wakePending, eof, lastPoll>>

CancelStale(p) ==
    /\ p \in Held
    /\ Accepting
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Canceled"]
    /\ IF OmitStaleReleaseBug
          THEN /\ UNCHANGED reserved
               /\ UNCHANGED permitCount
          ELSE /\ reserved' = reserved - 1
               /\ permitCount' = permitCount - 1
    /\ wakePending' =
          IF receiverWaiting
             /\ senderCount = 0
             /\ Cardinality(Held \ {p}) = 0
             /\ ~OmitPermitWakeBug
          THEN TRUE
          ELSE wakePending
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, senderCount,
                   permitScope, expectedGeneration, expectedRoom,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, eof, lastPoll>>

(* Message kind is chosen at commit, not reservation. Both mismatch          *)
(* directions are therefore reachable: current scope used for a transition  *)
(* and next-generation scope used for an ordinary control.                   *)
PermitCommitStaleControl(p) ==
    /\ ~ScopeMatches(p)
    /\ ~SkipScopeValidationBug
    /\ CancelStale(p)

PermitCommitStaleTransition(p) ==
    /\ expectedGeneration[p] # enqueueGeneration + 1
    /\ CancelStale(p)

PermitCommitControl(p) ==
    /\ p \in Held
    /\ Accepting
    /\ (ScopeMatches(p) \/ SkipScopeValidationBug)
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Queued"]
    /\ queue' = Append(queue, p)
    /\ reserved' = reserved - 1
    /\ permitCount' = permitCount - 1
    /\ committedGeneration' =
          [committedGeneration EXCEPT ![p] = enqueueGeneration]
    /\ committedRoom' = [committedRoom EXCEPT ![p] = activeRoom]
    /\ wakePending' = IF receiverWaiting /\ ~OmitPermitWakeBug
                          THEN TRUE ELSE wakePending
    /\ UNCHANGED <<receiverOpen, accountabilityFailed, senderCount, permitScope,
                   expectedGeneration, expectedRoom, enqueueGeneration,
                   activeRoom, receiverWaiting, eof, lastPoll>>

PermitCommitTransition(p) ==
    /\ p \in Held
    /\ Accepting
    /\ expectedGeneration[p] = enqueueGeneration + 1
    /\ lifecycle' = [lifecycle EXCEPT ![p] = "Queued"]
    /\ queue' = Append(queue, p)
    /\ reserved' = reserved - 1
    /\ permitCount' = permitCount - 1
    /\ committedGeneration' =
          [committedGeneration EXCEPT ![p] = expectedGeneration[p]]
    /\ committedRoom' = [committedRoom EXCEPT ![p] = "B"]
    /\ enqueueGeneration' = expectedGeneration[p]
    /\ activeRoom' = "B"
    /\ wakePending' = IF receiverWaiting /\ ~OmitPermitWakeBug
                          THEN TRUE ELSE wakePending
    /\ UNCHANGED <<receiverOpen, accountabilityFailed, senderCount, permitScope,
                   expectedGeneration, expectedRoom, receiverWaiting, eof, lastPoll>>

ReceiverResume ==
    /\ receiverWaiting
    /\ wakePending
    /\ receiverWaiting' = FALSE
    /\ wakePending' = FALSE
    /\ UNCHANGED <<queue, receiverOpen, accountabilityFailed, senderCount,
                   reserved, permitCount, lifecycle, permitScope,
                   expectedGeneration, expectedRoom, committedGeneration,
                   committedRoom, enqueueGeneration, activeRoom, eof, lastPoll>>

ReceiverPoll ==
    /\ ~receiverWaiting
    /\ ~eof
    /\ IF accountabilityFailed
          THEN /\ eof' = TRUE
               /\ lastPoll' = "Failed"
               /\ UNCHANGED <<queue, lifecycle>>
               /\ receiverWaiting' = FALSE
          ELSE IF queue # <<>>
          THEN /\ lifecycle' = [lifecycle EXCEPT ![Head(queue)] = "Delivered"]
               /\ queue' = Tail(queue)
               /\ eof' = FALSE
               /\ lastPoll' = "Item"
               /\ receiverWaiting' = FALSE
          ELSE IF receiverOpen /\ ProducersOpen
          THEN /\ UNCHANGED <<queue, lifecycle>>
               /\ eof' = FALSE
               /\ lastPoll' = "Empty"
               /\ receiverWaiting' = TRUE
          ELSE /\ UNCHANGED <<queue, lifecycle>>
               /\ eof' = TRUE
               /\ lastPoll' = "Eof"
               /\ receiverWaiting' = FALSE
    /\ UNCHANGED <<receiverOpen, accountabilityFailed, senderCount, reserved,
                   permitCount, permitScope, expectedGeneration, expectedRoom,
                   committedGeneration, committedRoom, enqueueGeneration,
                   activeRoom, wakePending>>

Done ==
    /\ eof
    /\ UNCHANGED vars

Next ==
    \/ \E p \in PermitIds, roomScope \in {"A", "B", AnyRoom} :
         ReserveCurrent(p, roomScope)
    \/ \E p \in PermitIds : ReserveNext(p)
    \/ SenderDrop
    \/ ReceiverClose
    \/ FailAccountability
    \/ \E p \in PermitIds : PermitDrop(p)
    \/ \E p \in PermitIds : PermitCommitFailed(p)
    \/ \E p \in PermitIds : PermitCommitStaleControl(p)
    \/ \E p \in PermitIds : PermitCommitStaleTransition(p)
    \/ \E p \in PermitIds : PermitCommitControl(p)
    \/ \E p \in PermitIds : PermitCommitTransition(p)
    \/ ReceiverResume
    \/ ReceiverPoll
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ queue \in Seq(PermitIds)
    /\ receiverOpen \in BOOLEAN
    /\ accountabilityFailed \in BOOLEAN
    /\ senderCount \in 0..1
    /\ reserved \in 0..2
    /\ permitCount \in 0..2
    /\ lifecycle \in [PermitIds ->
          {"Ready", "Held", "Queued", "Canceled", "Failed", "Delivered"}]
    /\ permitScope \in [PermitIds -> {"None", "Current", "Next"}]
    /\ expectedGeneration \in [PermitIds -> {NoGeneration, 0, 1}]
    /\ expectedRoom \in [PermitIds -> {NoRoom, AnyRoom, "A", "B"}]
    /\ committedGeneration \in [PermitIds -> {NoGeneration, 0, 1}]
    /\ committedRoom \in [PermitIds -> {NoRoom, "A", "B"}]
    /\ enqueueGeneration \in 0..1
    /\ activeRoom \in {"A", "B"}
    /\ receiverWaiting \in BOOLEAN
    /\ wakePending \in BOOLEAN
    /\ eof \in BOOLEAN
    /\ lastPoll \in {"None", "Empty", "Item", "Eof", "Failed"}

QueueBounded == Len(queue) + reserved <= QueueCapacity

PermitAccountingExact ==
    /\ reserved = Cardinality(Held)
    /\ permitCount = Cardinality(Held)

PermitLifecycleConservation ==
    \A p \in PermitIds :
        /\ (lifecycle[p] = "Queued" => QueueOccurrences(p) = 1)
        /\ (lifecycle[p] # "Queued" => QueueOccurrences(p) = 0)
        /\ (lifecycle[p] = "Ready" => permitScope[p] = "None")
        /\ (lifecycle[p] # "Ready" => permitScope[p] # "None")

NoPrematureEof ==
    eof => ~(Held # {} /\ Accepting)

NoStaleScopeCommit ==
    \A p \in PermitIds :
        lifecycle[p] \in {"Queued", "Delivered"} =>
            /\ expectedGeneration[p] = committedGeneration[p]
            /\ (expectedRoom[p] = AnyRoom
                  \/ expectedRoom[p] = committedRoom[p])

WaitingReceiverNotified ==
    receiverWaiting
    /\ (queue # <<>> \/ ~Accepting \/ ~ProducersOpen)
        => wakePending

=============================================================================

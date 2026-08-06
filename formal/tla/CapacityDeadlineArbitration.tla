------------------- MODULE CapacityDeadlineArbitration -------------------
(***************************************************************************)
(* Classified outbound-queue capacity-deadline arbitration (#290 / P56).   *)
(*                                                                         *)
(* A producer observes a full lane at FULL_OBSERVED_AT and waits until the *)
(* exclusive DEADLINE. The socket writer can return capacity before, at,   *)
(* or after that deadline while the producer is not scheduled. Another    *)
(* producer can refill the lane before the waiter runs.                    *)
(*                                                                         *)
(* The queue records the first full-to-non-full transition and retains it  *)
(* only while capacity remains continuously available. On a late producer *)
(* poll, witness validation and admission are one atomic queue-lock action: *)
(*                                                                         *)
(*   availableSince >= FULL_OBSERVED_AT                                    *)
(*   availableSince < DEADLINE                                             *)
(*                                                                         *)
(* A refill to capacity clears availableSince. A later drain starts new    *)
(* evidence, so stale progress cannot revive an expired wait.              *)
(*                                                                         *)
(* Mapping to src/coordination/outbound_queue.rs:                           *)
(*   WriterDrain       QueueState::refresh_capacity_availability after pop *)
(*   CompetingRefill   enqueue/reserve plus the same refresh helper         *)
(*   ProducerAdmit     CapacityReleaseWitness::permits_locked and the       *)
(*                     try_enqueue_*_released_before /                      *)
(*                     try_reserve_control_*_released_before methods       *)
(*                                                                         *)
(* NON-VACUITY: TimerFirstBug = TRUE models the old deadline branch, which  *)
(* declared SlowConsumer whenever the producer was polled after expiry even *)
(* if capacity had become and stayed available beforehand. The CI-pinned   *)
(* _ExpectedFailure configuration requires TLC to report exactly           *)
(* `NoFalseSlowConsumer` for that seeded defect.                            *)
(***************************************************************************)
EXTENDS FiniteSets, Integers, Naturals, Sequences

CONSTANTS
    QUEUE_CAPACITY,       \* one lane's bounded capacity
    DEADLINE,             \* exclusive full-queue deadline
    MAX_TIME,             \* scheduler-delay exploration ceiling
    TimerFirstBug         \* TRUE reintroduces the pre-#291 arbitration bug

ASSUME /\ QUEUE_CAPACITY \in Nat \ {0}
       /\ DEADLINE \in Nat \ {0}
       /\ MAX_TIME \in Nat
       /\ MAX_TIME > DEADLINE
       /\ TimerFirstBug \in BOOLEAN

NoTime == -1
FULL_OBSERVED_AT == 0

VARIABLES
    now,                    \* discrete monotonic time
    queue,                  \* bounded capacity claims, not post-reservation sends
    availableSince,         \* NoTime while full; first continuous free instant
    waiter,                 \* "Waiting" | "Admitted" | "SlowConsumer"
    resolvedAt,             \* NoTime until the waiter resolves
    admittedRelease,        \* witness instant used by a late admission
    abandoned               \* expired waiter attempts, exactly zero or one

vars == <<now, queue, availableSince, waiter, resolvedAt,
          admittedRelease, abandoned>>

QueueFull(q) == Len(q) = QUEUE_CAPACITY
HasCapacity(q) == Len(q) < QUEUE_CAPACITY

(* The late-poll witness is valid only while the same lane remains non-full. *)
TimelyContinuousCapacity ==
    /\ HasCapacity(queue)
    /\ availableSince # NoTime
    /\ availableSince >= FULL_OBSERVED_AT
    /\ availableSince < DEADLINE

Init ==
    /\ now = FULL_OBSERVED_AT
    /\ queue = [i \in 1..QUEUE_CAPACITY |-> "Existing"]
    /\ availableSince = NoTime
    /\ waiter = "Waiting"
    /\ resolvedAt = NoTime
    /\ admittedRelease = NoTime
    /\ abandoned = 0

(* Time can advance without polling the waiting producer. This is the hosted *)
(* scheduler-delay shape that exposed #290.                                 *)
AdvanceTime ==
    /\ waiter = "Waiting"
    /\ now < MAX_TIME
    /\ now' = now + 1
    /\ UNCHANGED <<queue, availableSince, waiter, resolvedAt,
                   admittedRelease, abandoned>>

(* A socket write frees one slot. Only a full-to-non-full transition starts *)
(* the witness clock; later drains preserve that first instant.             *)
WriterDrain ==
    /\ waiter = "Waiting"
    /\ queue # <<>>
    /\ queue' = Tail(queue)
    /\ availableSince' = IF QueueFull(queue) THEN now ELSE availableSince
    /\ UNCHANGED <<now, waiter, resolvedAt, admittedRelease, abandoned>>

(* Any enqueue or reservation can win the queue lock first. Filling the lane *)
(* invalidates the release witness; validation may never use stale progress. *)
CompetingRefill ==
    /\ waiter = "Waiting"
    /\ HasCapacity(queue)
    /\ queue' = Append(queue, "Competing")
    /\ availableSince' = IF QueueFull(queue') THEN NoTime ELSE availableSince
    /\ UNCHANGED <<now, waiter, resolvedAt, admittedRelease, abandoned>>

(* Before expiry, an enqueue or reservation may claim any available slot. At *)
(* or after expiry, admission requires the retained pre-deadline witness. The *)
(* check and capacity claim are one action, matching one queue mutex hold.    *)
(* Post-reservation send/drop/cancel lifecycle is deliberately out of scope.  *)
ProducerAdmit ==
    /\ waiter = "Waiting"
    /\ HasCapacity(queue)
    /\ \/ now < DEADLINE
       \/ /\ ~TimerFirstBug
          /\ TimelyContinuousCapacity
    /\ queue' = Append(queue, "Waiter")
    /\ availableSince' = IF QueueFull(queue') THEN NoTime ELSE availableSince
    /\ waiter' = "Admitted"
    /\ resolvedAt' = now
    /\ admittedRelease' =
           IF now >= DEADLINE THEN availableSince ELSE NoTime
    /\ UNCHANGED <<now, abandoned>>

(* The exclusive deadline wins at equality. A corrected late poll expires   *)
(* only when no continuously available pre-deadline slot can be claimed.    *)
ProducerExpire ==
    /\ waiter = "Waiting"
    /\ now >= DEADLINE
    /\ \/ TimerFirstBug
       \/ ~TimelyContinuousCapacity
    /\ waiter' = "SlowConsumer"
    /\ resolvedAt' = now
    /\ abandoned' = 1
    /\ UNCHANGED <<now, queue, availableSince, admittedRelease>>

(* Terminal stutter keeps TLC deadlock checking meaningful. *)
Done ==
    /\ waiter \in {"Admitted", "SlowConsumer"}
    /\ UNCHANGED vars

Next ==
    \/ AdvanceTime
    \/ WriterDrain
    \/ CompetingRefill
    \/ ProducerAdmit
    \/ ProducerExpire
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ now \in 0..MAX_TIME
    /\ queue \in Seq({"Existing", "Competing", "Waiter"})
    /\ availableSince \in {NoTime} \cup 0..MAX_TIME
    /\ waiter \in {"Waiting", "Admitted", "SlowConsumer"}
    /\ resolvedAt \in {NoTime} \cup 0..MAX_TIME
    /\ admittedRelease \in {NoTime} \cup 0..MAX_TIME
    /\ abandoned \in 0..1

QueueBounded == Len(queue) <= QUEUE_CAPACITY

AvailabilityExact ==
    /\ (QueueFull(queue) => availableSince = NoTime)
    /\ (HasCapacity(queue) => availableSince # NoTime)

(* The one logical waiter is waiting, capacity-admitted, or loudly abandoned. *)
(* This stops at reservation admission; it does not model permit commit/drop. *)
AdmissionConservation ==
    1 =
        (IF waiter = "Waiting" THEN 1 ELSE 0)
        + Cardinality({i \in 1..Len(queue) : queue[i] = "Waiter"})
        + abandoned

ResolutionConsistent ==
    /\ ((waiter = "Waiting") = (resolvedAt = NoTime))
    /\ ((waiter = "SlowConsumer") = (abandoned = 1))
    /\ ((waiter = "Admitted") =>
           (admittedRelease # NoTime \/ resolvedAt < DEADLINE))

(* This is the #290 property. Scheduler delay alone cannot evict a waiter if *)
(* a slot became and remained available strictly before the deadline.       *)
NoFalseSlowConsumer ==
    waiter = "SlowConsumer" => ~TimelyContinuousCapacity

(* An admission at/after the exclusive deadline must cite current, retained, *)
(* strictly pre-deadline release evidence.                                  *)
NoLateRevival ==
    (waiter = "Admitted" /\ resolvedAt >= DEADLINE) =>
        /\ admittedRelease # NoTime
        /\ admittedRelease >= FULL_OBSERVED_AT
        /\ admittedRelease < DEADLINE

=============================================================================

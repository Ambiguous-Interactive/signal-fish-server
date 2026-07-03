------------------------------ MODULE DeliveryContract ------------------------------
(***************************************************************************)
(* The per-connection delivery contract introduced by the #131 fix         *)
(* (`coordination::deliver_or_disconnect` + `ConnectionCloseSignal`):      *)
(*                                                                         *)
(*   A message accepted for delivery is NEVER silently dropped. It is      *)
(*   either enqueued on the recipient's bounded outbound queue (applying   *)
(*   backpressure to the sender while the queue is full), or — if the      *)
(*   recipient cannot absorb one message for the whole slow-consumer       *)
(*   grace window — it is abandoned only TOGETHER WITH the recipient's     *)
(*   connection: the close is requested (first reason wins), the writer    *)
(*   flushes what it can, and every unflushed message is counted as        *)
(*   dropped against that closing connection.                              *)
(*                                                                         *)
(* One recipient connection, a small set of senders with finite message    *)
(* budgets. The consumer (the socket writer draining the queue) is         *)
(* deliberately UNFAIR: TLC may simply never schedule `WriterDrain`,       *)
(* modeling a peer that stops reading. The slow-consumer timeout is        *)
(* abstracted as the nondeterministic `GraceExpired` action.               *)
(*                                                                         *)
(* Mapping to the implementation (src/coordination/mod.rs,                 *)
(* src/websocket/connection.rs):                                           *)
(*   SendFast          try_send fast path into the bounded mpsc            *)
(*   SendFull          try_send returns Full; sender parks in send().await *)
(*   ParkedEnqueue     the backpressured send().await completes            *)
(*   GraceExpired      tokio::time::timeout elapses: message abandoned,    *)
(*                     request_close(SlowConsumer), drop counted           *)
(*   LifecycleClose    any other eviction path requesting the close        *)
(*                     (reaper, idle timeout) — exercises first-reason-wins*)
(*   WriterDrain       the send task writing a queued message to the socket*)
(*   CloseFlushDrain   the bounded already-queued flush during teardown    *)
(*   CloseFinish       teardown completes even with a wedged socket write  *)
(*                     (the `select!` close preemption); the undrained     *)
(*                     queue remainder is counted dropped, exactly like    *)
(*                     `finalize_closed_connection`                        *)
(*   SendChannelClosed try_send/send against a dropped receiver resolves   *)
(*                     as the ChannelClosed outcome (a disconnect race,    *)
(*                     not a delivery fault)                               *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible): setting   *)
(* the `SilentDropBug` constant to TRUE turns `SendFull` into the pre-#131 *)
(* behavior — the message vanishes with no park, no drop count, no close.  *)
(* TLC then reports a `Conservation` violation within a handful of steps   *)
(* (a sent message accounted nowhere, on a live connection). The checked   *)
(* configuration pins `SilentDropBug = FALSE`; flip it locally to watch    *)
(* the invariant catch the original bug.                                   *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Senders,            \* nonempty set of sender identities
    MessagesPerSender,  \* per-sender delivery budget (keep tiny: 2)
    QueueCapacity,      \* bounded outbound queue slots (keep tiny: 2)
    SilentDropBug       \* TRUE reintroduces the pre-#131 silent drop (must
                        \* violate Conservation; FALSE in checked configs)

ASSUME /\ Senders # {}
       /\ MessagesPerSender \in Nat \ {0}
       /\ QueueCapacity \in Nat \ {0}
       /\ SilentDropBug \in BOOLEAN

VARIABLES
    queue,        \* Seq(Senders): the recipient's bounded outbound queue
    senderState,  \* [Senders -> {"Idle", "Parked"}]: Parked = in send().await
    sent,         \* [Senders -> Nat]: delivery attempts started
    written,      \* [Senders -> Nat]: messages drained to the socket
    dropped,      \* [Senders -> Nat]: messages abandoned with the connection
    chClosed,     \* [Senders -> Nat]: attempts resolved ChannelClosed
    connState,    \* "Open" | "CloseRequested" | "Closed"
    closeReason   \* "None" | "SlowConsumer" | "Lifecycle" (first wins)

vars == <<queue, senderState, sent, written, dropped, chClosed, connState, closeReason>>

(* Number of sender s's messages currently sitting in the queue. *)
Occ(s) == Len(SelectSeq(queue, LAMBDA e: e = s))

(* A sender with budget left and no in-flight (parked) message. *)
CanSend(s) == senderState[s] = "Idle" /\ sent[s] < MessagesPerSender

(* First close reason wins — later requests must not overwrite it. *)
RequestClose(reason) ==
    closeReason' = IF closeReason = "None" THEN reason ELSE closeReason

Init ==
    /\ queue = <<>>
    /\ senderState = [s \in Senders |-> "Idle"]
    /\ sent = [s \in Senders |-> 0]
    /\ written = [s \in Senders |-> 0]
    /\ dropped = [s \in Senders |-> 0]
    /\ chClosed = [s \in Senders |-> 0]
    /\ connState = "Open"
    /\ closeReason = "None"

(* try_send succeeds: the queue has a free slot. Deliveries keep landing    *)
(* after a close was REQUESTED (the receiver still exists until the tasks  *)
(* exit); CloseFinish accounts whatever never reaches the socket.          *)
SendFast(s) ==
    /\ CanSend(s)
    /\ connState \in {"Open", "CloseRequested"}
    /\ Len(queue) < QueueCapacity
    /\ queue' = Append(queue, s)
    /\ sent' = [sent EXCEPT ![s] = @ + 1]
    /\ UNCHANGED <<senderState, written, dropped, chClosed, connState, closeReason>>

(* try_send returns Full. Contract behavior: the sender parks inside the   *)
(* backpressured send().await. Pre-#131 bug behavior (SilentDropBug): the  *)
(* message simply vanishes — sent is incremented but nothing else changes, *)
(* which is exactly the accounting hole Conservation must catch.           *)
SendFull(s) ==
    /\ CanSend(s)
    /\ connState \in {"Open", "CloseRequested"}
    /\ Len(queue) = QueueCapacity
    /\ sent' = [sent EXCEPT ![s] = @ + 1]
    /\ IF SilentDropBug
           THEN UNCHANGED <<queue, senderState, written, dropped, chClosed,
                            connState, closeReason>>
           ELSE /\ senderState' = [senderState EXCEPT ![s] = "Parked"]
                /\ UNCHANGED <<queue, written, dropped, chClosed, connState,
                               closeReason>>

(* A delivery attempt against the fully torn-down connection resolves as   *)
(* ChannelClosed — a normal disconnect race, never a silent loss.          *)
SendChannelClosed(s) ==
    /\ CanSend(s)
    /\ connState = "Closed"
    /\ sent' = [sent EXCEPT ![s] = @ + 1]
    /\ chClosed' = [chClosed EXCEPT ![s] = @ + 1]
    /\ UNCHANGED <<queue, senderState, written, dropped, connState, closeReason>>

(* The backpressured send().await completes: a slot freed before the grace *)
(* window elapsed.                                                         *)
ParkedEnqueue(s) ==
    /\ senderState[s] = "Parked"
    /\ connState \in {"Open", "CloseRequested"}
    /\ Len(queue) < QueueCapacity
    /\ queue' = Append(queue, s)
    /\ senderState' = [senderState EXCEPT ![s] = "Idle"]
    /\ UNCHANGED <<sent, written, dropped, chClosed, connState, closeReason>>

(* The receiver was dropped while the sender was parked: send().await      *)
(* errors, the attempt resolves ChannelClosed.                             *)
ParkedChannelClosed(s) ==
    /\ senderState[s] = "Parked"
    /\ connState = "Closed"
    /\ chClosed' = [chClosed EXCEPT ![s] = @ + 1]
    /\ senderState' = [senderState EXCEPT ![s] = "Idle"]
    /\ UNCHANGED <<queue, sent, written, dropped, connState, closeReason>>

(* The slow-consumer grace window elapses for a parked sender: the message *)
(* is abandoned (counted dropped) and the recipient's close is requested   *)
(* with reason SlowConsumer — loud by construction. Nondeterministic       *)
(* enablement abstracts the timeout duration.                              *)
GraceExpired(s) ==
    /\ senderState[s] = "Parked"
    /\ connState \in {"Open", "CloseRequested"}
    /\ dropped' = [dropped EXCEPT ![s] = @ + 1]
    /\ senderState' = [senderState EXCEPT ![s] = "Idle"]
    /\ connState' = "CloseRequested"
    /\ RequestClose("SlowConsumer")
    /\ UNCHANGED <<queue, sent, written, chClosed>>

(* Any non-delivery eviction (reaper, idle timeout) requesting the close.  *)
(* Exercises first-reason-wins against GraceExpired.                       *)
LifecycleClose ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<queue, senderState, sent, written, dropped, chClosed>>

(* The send task writes one queued message to the socket. Deliberately     *)
(* unfair: a stalled peer means TLC never schedules this.                  *)
WriterDrain ==
    /\ connState = "Open"
    /\ queue # <<>>
    /\ written' = [written EXCEPT ![Head(queue)] = @ + 1]
    /\ queue' = Tail(queue)
    /\ UNCHANGED <<senderState, sent, dropped, chClosed, connState, closeReason>>

(* Bounded flush of already-queued messages during teardown.               *)
CloseFlushDrain ==
    /\ connState = "CloseRequested"
    /\ queue # <<>>
    /\ written' = [written EXCEPT ![Head(queue)] = @ + 1]
    /\ queue' = Tail(queue)
    /\ UNCHANGED <<senderState, sent, dropped, chClosed, connState, closeReason>>

(* Teardown completes — even mid-flush (the close preempts a wedged socket *)
(* write). Everything still queued is counted dropped against the closing  *)
(* connection, mirroring `finalize_closed_connection`.                     *)
CloseFinish ==
    /\ connState = "CloseRequested"
    /\ connState' = "Closed"
    /\ dropped' = [s \in Senders |-> dropped[s] + Occ(s)]
    /\ queue' = <<>>
    /\ UNCHANGED <<senderState, sent, written, chClosed, closeReason>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful      *)
(* (matches the SignalFishSession convention): once every budget is spent  *)
(* and every attempt has resolved, the system idles.                       *)
AllResolved ==
    /\ \A s \in Senders : sent[s] = MessagesPerSender /\ senderState[s] = "Idle"
    /\ queue = <<>> \/ connState = "Closed"

Done ==
    /\ AllResolved
    /\ UNCHANGED vars

Next ==
    \/ \E s \in Senders :
        SendFast(s) \/ SendFull(s) \/ SendChannelClosed(s)
        \/ ParkedEnqueue(s) \/ ParkedChannelClosed(s) \/ GraceExpired(s)
    \/ LifecycleClose
    \/ WriterDrain
    \/ CloseFlushDrain
    \/ CloseFinish
    \/ Done

(* Fairness: ONLY the contract's own guarantees. The grace window always   *)
(* fires eventually for a permanently parked sender (a tokio timeout),     *)
(* teardown always completes once requested (the close preemption), and a  *)
(* parked send always observes a dropped receiver. The CONSUMER gets no    *)
(* fairness at all — the contract must hold against a peer that never      *)
(* reads.                                                                  *)
Fairness ==
    /\ \A s \in Senders : WF_vars(GraceExpired(s))
    /\ \A s \in Senders : WF_vars(ParkedChannelClosed(s))
    /\ WF_vars(CloseFinish)

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ queue \in Seq(Senders)
    /\ Len(queue) <= QueueCapacity
    /\ senderState \in [Senders -> {"Idle", "Parked"}]
    /\ sent \in [Senders -> 0..MessagesPerSender]
    /\ connState \in {"Open", "CloseRequested", "Closed"}
    /\ closeReason \in {"None", "SlowConsumer", "Lifecycle"}

(* THE #131 invariant: every delivery attempt is accounted for at every    *)
(* reachable state — parked in-flight, queued, written to the socket,      *)
(* dropped together with a closing connection, or resolved as a disconnect *)
(* race. A message that vanishes any other way violates this immediately.  *)
Conservation ==
    \A s \in Senders :
        sent[s] = Occ(s) + written[s] + dropped[s] + chClosed[s]
                  + (IF senderState[s] = "Parked" THEN 1 ELSE 0)

(* Drops are LOUD: a nonzero drop count can only coexist with a requested/ *)
(* completed close and a recorded reason — never with a healthy connection.*)
NoSilentLoss ==
    (\E s \in Senders : dropped[s] > 0) =>
        /\ connState \in {"CloseRequested", "Closed"}
        /\ closeReason # "None"

(* Teardown accounted for the whole queue.                                 *)
ClosedQueueEmpty == (connState = "Closed") => queue = <<>>

----------------------------------------------------------------------------
(* Temporal properties *)

(* First reason wins: once recorded, the close reason never changes.       *)
ReasonStable == [][closeReason # "None" => closeReason' = closeReason]_vars

(* Close preemption: a requested close always completes, even against a    *)
(* consumer that never drains (no WriterDrain fairness anywhere).          *)
CloseCompletes == (connState = "CloseRequested") ~> (connState = "Closed")

(* Bounded backpressure: no sender blocks forever — the grace window or a  *)
(* freed slot or the teardown always unparks it.                           *)
SenderUnblocks ==
    \A s \in Senders : (senderState[s] = "Parked") ~> (senderState[s] = "Idle")

=============================================================================

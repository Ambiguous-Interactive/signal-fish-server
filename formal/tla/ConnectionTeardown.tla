------------------------------ MODULE ConnectionTeardown ------------------------------
(***************************************************************************)
(* The per-connection TASK-lifecycle contract of the WebSocket layer       *)
(* (src/websocket/connection.rs `handle_socket` + `finalize_closed_        *)
(* connection`, driven by `coordination::ConnectionCloseSignal`):          *)
(*                                                                         *)
(*   Once a server-side close is requested, BOTH socket tasks (send and    *)
(*   receive) exit — even against a peer that never drains, i.e. a socket  *)
(*   write wedged forever — because the send task's write loop runs INSIDE *)
(*   a select! against the close listener, so the close preempts a wedged  *)
(*   write at any await point. Every message still queued at finalize is   *)
(*   counted dropped exactly once, pre-close farewells are bounded         *)
(*   try_sends that never wait, and the connection's resources (the fd)    *)
(*   are released exactly when both tasks have exited. No zombie sockets.  *)
(*                                                                         *)
(* DeliveryContract.tla models the QUEUE side of the same connection       *)
(* (backpressure, grace expiry, sender-observed outcomes); this module     *)
(* models the TASK side: the send task's write loop with an explicit       *)
(* in-flight (possibly wedged) socket write, the receive task, the close   *)
(* watch, the bounded farewell, the drain-on-close flush, and resource     *)
(* release. The peer draining a write is deliberately UNFAIR — the         *)
(* contract must hold against a peer that stops reading forever.           *)
(*                                                                         *)
(* Mapping to the implementation (src/websocket/connection.rs,             *)
(* src/coordination/mod.rs):                                               *)
(*   Enqueue            a delivery landing on the connection's bounded     *)
(*                      outbound mpsc (deliver_or_disconnect's enqueue     *)
(*                      outcome; its full-queue dynamics live in           *)
(*                      DeliveryContract.tla)                              *)
(*   WriteStart         the send task popping a message and starting the   *)
(*                      socket write (send_single_message / send_batch)    *)
(*   WriteFinish        the peer draining the write — UNFAIR: never        *)
(*                      scheduled models the wedged write                  *)
(*   RequestClose*      ConnectionCloseSignal::request_close (slow         *)
(*                      consumer / lifecycle eviction; first reason wins)  *)
(*   RecvPeerClose      the peer closing (or erroring) the socket: the     *)
(*                      receive task exits on its own and its cleanup      *)
(*                      unregisters the connection, which requests the     *)
(*                      close for the sibling task                         *)
(*   FarewellTry        enqueue_farewell_message: a try_send that either   *)
(*                      enqueues or fails Full IN THE SAME STEP — there is *)
(*                      no waiting state for it, by construction           *)
(*   CloseFlush         finalize's bounded already-queued flush (the       *)
(*                      CLOSE_WRITE_TIMEOUT'd drain; optional — bounded    *)
(*                      means exit never waits on it)                      *)
(*   SendTaskExit       the select! close arm firing: the write loop is    *)
(*                      cancelled WHEREVER it was (including mid-write),   *)
(*                      finalize counts rx.len() + batcher.len() [+ the    *)
(*                      cancelled in-flight write — the model counts it    *)
(*                      dropped too, strengthening the code's "misses at   *)
(*                      most the one frame actively on the wire" to exact  *)
(*                      conservation] as dropped, then the task exits      *)
(*   RecvTaskExit       the receive task's close-listener select arm       *)
(*   ReleaseResource    handle_socket's final join + unregister: the       *)
(*                      socket fd drops when both halves are done          *)
(*                                                                         *)
(* Deliberately NOT modeled (documented): the send task's natural exit on  *)
(* a closed channel (requires every delivery handle dropped, which in the  *)
(* server always travels through unregistration — i.e. a close request);   *)
(* post-exit deliveries resolving ChannelClosed (DeliveryContract's        *)
(* SendChannelClosed).                                                     *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible): setting   *)
(* the `NoPreemptionBug` constant to TRUE makes the send task observe the  *)
(* close only BETWEEN writes (the write not wrapped in the select! — the   *)
(* exact zombie-socket hazard the real code's comment warns about). TLC    *)
(* then reports a `NoZombie` liveness violation: Enqueue -> WriteStart     *)
(* (the write wedges; WriteFinish is unfair and never scheduled) ->        *)
(* RequestCloseSlow -> the receive task exits, but the send task can       *)
(* never observe the close — the behavior stutters forever with            *)
(* sendTask = "Running". The checked configuration pins                    *)
(* `NoPreemptionBug = FALSE`; flip it locally to watch the property catch  *)
(* the zombie.                                                             *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    MessageBudget, \* deliveries enqueued onto the connection (keep tiny: 3)
    QueueCapacity, \* bounded outbound queue slots (keep tiny: 2)
    NoPreemptionBug \* TRUE: the socket write is NOT select!'d against the
                    \* close, so a wedged write blocks exit forever (must
                    \* violate NoZombie; FALSE in checked configs)

ASSUME /\ MessageBudget \in Nat \ {0}
       /\ QueueCapacity \in Nat \ {0}
       /\ NoPreemptionBug \in BOOLEAN

VARIABLES
    sendTask,    \* "Running" | "Exited"
    recvTask,    \* "Running" | "Exited"
    closeReason, \* "None" | "SlowConsumer" | "Lifecycle" (first wins)
    qlen,        \* messages sitting in the bounded outbound queue
    inFlight,    \* 0 | 1: a socket write in progress (possibly wedged)
    sent,        \* messages ever enqueued (deliveries + farewell)
    written,     \* messages fully written to the socket
    dropped,     \* messages abandoned with the closing connection
    farewell,    \* "NotAttempted" | "Enqueued" | "QueueFull" — the bounded
                 \* farewell try_send resolves in ONE atomic step: this
                 \* domain deliberately has NO waiting state
    released     \* the connection's resources (fd) have been reclaimed

vars == <<sendTask, recvTask, closeReason, qlen, inFlight, sent, written,
          dropped, farewell, released>>

(* First close reason wins — later requests must not overwrite it. *)
RequestClose(reason) ==
    closeReason' = IF closeReason = "None" THEN reason ELSE closeReason

Init ==
    /\ sendTask = "Running"
    /\ recvTask = "Running"
    /\ closeReason = "None"
    /\ qlen = 0
    /\ inFlight = 0
    /\ sent = 0
    /\ written = 0
    /\ dropped = 0
    /\ farewell = "NotAttempted"
    /\ released = FALSE

(* A delivery lands on the outbound queue (the enqueue outcome of           *)
(* deliver_or_disconnect; full-queue backpressure/eviction dynamics are     *)
(* DeliveryContract.tla's subject). Deliveries keep landing after a close   *)
(* was requested — the receiver exists until the send task exits.           *)
Enqueue ==
    /\ sendTask = "Running"
    /\ sent < MessageBudget
    /\ qlen < QueueCapacity
    /\ qlen' = qlen + 1
    /\ sent' = sent + 1
    /\ UNCHANGED <<sendTask, recvTask, closeReason, inFlight, written,
                   dropped, farewell, released>>

(* The send task pops a message and starts the socket write.                *)
WriteStart ==
    /\ sendTask = "Running"
    /\ closeReason = "None"
    /\ inFlight = 0
    /\ qlen > 0
    /\ qlen' = qlen - 1
    /\ inFlight' = 1
    /\ UNCHANGED <<sendTask, recvTask, closeReason, sent, written, dropped,
                   farewell, released>>

(* The peer drains the write. Deliberately UNFAIR: a peer that stopped      *)
(* reading means TLC never schedules this — the wedged-write state the      *)
(* close preemption must overcome.                                          *)
WriteFinish ==
    /\ sendTask = "Running"
    /\ inFlight = 1
    /\ inFlight' = 0
    /\ written' = written + 1
    /\ UNCHANGED <<sendTask, recvTask, closeReason, qlen, sent, dropped,
                   farewell, released>>

(* Slow-consumer eviction requests the close (deliver_or_disconnect's grace *)
(* expiry — modeled nondeterministically; the queue-state trigger lives in  *)
(* DeliveryContract.tla).                                                   *)
RequestCloseSlow ==
    /\ closeReason = "None"
    /\ RequestClose("SlowConsumer")
    /\ UNCHANGED <<sendTask, recvTask, qlen, inFlight, sent, written,
                   dropped, farewell, released>>

(* Any lifecycle eviction (activity reaper, explicit unregistration)        *)
(* requesting the close. Exercises first-reason-wins ordering against       *)
(* RequestCloseSlow (ReasonStable).                                         *)
RequestCloseLifecycle ==
    /\ closeReason = "None"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<sendTask, recvTask, qlen, inFlight, sent, written,
                   dropped, farewell, released>>

(* The peer closes (or errors) the socket: the receive task exits on its    *)
(* own, and its cleanup unregisters the connection — which requests the     *)
(* close so the sibling send task tears down too (never a half-alive        *)
(* socket).                                                                 *)
RecvPeerClose ==
    /\ recvTask = "Running"
    /\ recvTask' = "Exited"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<sendTask, qlen, inFlight, sent, written, dropped,
                   farewell, released>>

(* enqueue_farewell_message: once a close is pending, a farewell error      *)
(* frame is try_sent onto the connection's own queue. It either enqueues    *)
(* or fails Full IN THIS SAME STEP — the action has no waiting state, so a  *)
(* full queue can never delay the teardown (FarewellNeverBlocks).           *)
FarewellTry ==
    /\ closeReason # "None"
    /\ farewell = "NotAttempted"
    /\ sendTask = "Running"
    /\ IF qlen < QueueCapacity
           THEN /\ farewell' = "Enqueued"
                /\ qlen' = qlen + 1
                /\ sent' = sent + 1
           ELSE /\ farewell' = "QueueFull"
                /\ UNCHANGED <<qlen, sent>>
    /\ UNCHANGED <<sendTask, recvTask, closeReason, inFlight, written,
                   dropped, released>>

(* finalize's bounded drain-on-close: already-queued messages are flushed   *)
(* while the peer still reads. Optional and unfair — the CLOSE_WRITE_       *)
(* TIMEOUT bound means SendTaskExit never waits for it.                     *)
CloseFlush ==
    /\ sendTask = "Running"
    /\ closeReason # "None"
    /\ inFlight = 0
    /\ qlen > 0
    /\ qlen' = qlen - 1
    /\ written' = written + 1
    /\ UNCHANGED <<sendTask, recvTask, closeReason, inFlight, sent, dropped,
                   farewell, released>>

(* The send task's select! close arm fires: the write loop is cancelled     *)
(* wherever it was — INCLUDING a wedged in-flight write — and finalize      *)
(* counts everything still buffered as dropped, exactly once, before the    *)
(* task exits. Under NoPreemptionBug the write is not select!'d against     *)
(* the close, so the close is only observable between writes (inFlight = 0) *)
(* — and a wedged write pins the task forever (the zombie).                 *)
SendTaskExit ==
    /\ sendTask = "Running"
    /\ closeReason # "None"
    /\ (NoPreemptionBug => inFlight = 0)
    /\ sendTask' = "Exited"
    /\ dropped' = dropped + qlen + inFlight
    /\ qlen' = 0
    /\ inFlight' = 0
    /\ UNCHANGED <<recvTask, closeReason, sent, written, farewell, released>>

(* The receive task's close-listener select arm fires: it stops reading and *)
(* exits (the send task owns the farewell + sink close).                    *)
RecvTaskExit ==
    /\ recvTask = "Running"
    /\ closeReason # "None"
    /\ recvTask' = "Exited"
    /\ UNCHANGED <<sendTask, closeReason, qlen, inFlight, sent, written,
                   dropped, farewell, released>>

(* handle_socket's final join + unregister: the connection's resources (the *)
(* socket fd, the registration) are reclaimed once BOTH tasks are done.     *)
ReleaseResource ==
    /\ sendTask = "Exited"
    /\ recvTask = "Exited"
    /\ ~released
    /\ released' = TRUE
    /\ UNCHANGED <<sendTask, recvTask, closeReason, qlen, inFlight, sent,
                   written, dropped, farewell>>

(* Explicit terminal stutter so TLC's deadlock check stays meaningful       *)
(* (matches the SignalFishSession convention).                              *)
Done ==
    /\ sendTask = "Exited"
    /\ recvTask = "Exited"
    /\ released
    /\ UNCHANGED vars

Next ==
    \/ Enqueue
    \/ WriteStart
    \/ WriteFinish
    \/ RequestCloseSlow
    \/ RequestCloseLifecycle
    \/ RecvPeerClose
    \/ FarewellTry
    \/ CloseFlush
    \/ SendTaskExit
    \/ RecvTaskExit
    \/ ReleaseResource
    \/ Done

(* Fairness: ONLY the code's own guarantees. The close-preemption arms      *)
(* always fire eventually (tokio wakes a select! on a watch-channel write), *)
(* and the parent task always reaps + unregisters. The PEER gets no         *)
(* fairness at all — WriteFinish (and the optional CloseFlush) may never be *)
(* scheduled, which is exactly the wedged-socket adversary the contract     *)
(* must beat.                                                               *)
Fairness ==
    /\ WF_vars(SendTaskExit)
    /\ WF_vars(RecvTaskExit)
    /\ WF_vars(ReleaseResource)

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ sendTask \in {"Running", "Exited"}
    /\ recvTask \in {"Running", "Exited"}
    /\ closeReason \in {"None", "SlowConsumer", "Lifecycle"}
    /\ qlen \in 0..QueueCapacity
    /\ inFlight \in 0..1
    /\ sent \in 0..(MessageBudget + 1)
    /\ written \in 0..(MessageBudget + 1)
    /\ dropped \in 0..(MessageBudget + 1)
    /\ farewell \in {"NotAttempted", "Enqueued", "QueueFull"}
    /\ released \in BOOLEAN

(* DropAccounting: every message ever enqueued is, at every reachable       *)
(* state, in exactly one place — still queued, in the (possibly wedged)     *)
(* in-flight write, written to the socket, or dropped with the closing      *)
(* connection. Monotone counters + this conservation law give "counted      *)
(* dropped exactly once" (a double count would overshoot, a missed count    *)
(* would undershoot).                                                       *)
DropAccounting ==
    sent = qlen + inFlight + written + dropped

(* Finalize accounted for the whole queue: an exited send task holds        *)
(* nothing (its queue remainder and any cancelled in-flight write were      *)
(* counted dropped by SendTaskExit).                                        *)
ExitedHoldsNothing ==
    sendTask = "Exited" => (qlen = 0 /\ inFlight = 0)

(* Drops happen only on the close path — never against a healthy            *)
(* connection.                                                              *)
DropsAreLoud ==
    dropped > 0 => closeReason # "None"

(* FarewellNeverBlocks (structural): the farewell outcome domain has no     *)
(* waiting state — FarewellTry resolves to Enqueued or QueueFull in the     *)
(* same atomic step it is attempted (see the action), so a full queue can   *)
(* never park the teardown behind the farewell.                             *)
FarewellNeverBlocks ==
    farewell \in {"NotAttempted", "Enqueued", "QueueFull"}

(* Resources are released only after BOTH tasks exited — never while a task *)
(* could still touch the socket.                                            *)
ReleaseRequiresBothExited ==
    released => (sendTask = "Exited" /\ recvTask = "Exited")

----------------------------------------------------------------------------
(* Temporal properties *)

(* First reason wins: once recorded, the close reason never changes.        *)
ReasonStable == [][closeReason # "None" => closeReason' = closeReason]_vars

(* THE no-zombie property: once a close is requested, both tasks eventually *)
(* exit — with NO fairness on the peer draining anything (a wedged write    *)
(* cannot pin the connection).                                              *)
NoZombie ==
    (closeReason # "None") ~> (sendTask = "Exited" /\ recvTask = "Exited")

(* And the fd is eventually reclaimed.                                      *)
ResourceReclaimed == (closeReason # "None") ~> released

=============================================================================

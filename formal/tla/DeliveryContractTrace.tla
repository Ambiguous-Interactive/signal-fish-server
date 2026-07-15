------------------------- MODULE DeliveryContractTrace -------------------------
(***************************************************************************)
(* P10.D7 executable trace refinement for the reliable, single-FIFO       *)
(* delivery contract. GeneratedDeliveryContractTrace.tla supplies one or  *)
(* more JSONL-derived traces. TLC chooses a trace in Init and replays only *)
(* its next named action. If that action's guard is false, TNext is false  *)
(* and TLC reports a deadlock with traceId and i in the state: the exact   *)
(* implementation <-> specification divergence.                           *)
(*                                                                         *)
(* The WriterStart/CloseFlushStart projection is deliberately more        *)
(* concrete than DeliveryContract.tla: the real socket task removes an    *)
(* item from queue capacity before its write future resolves. The item    *)
(* remains accountable in inFlight until a matching drain completes or   *)
(* CloseFinish abandons it with the connection.                            *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, GeneratedDeliveryContractTrace

CONSTANT TraceActionBug
ASSUME TraceActionBug \in BOOLEAN

VARIABLES traceId, i, queue, inFlight, senderState, sent, written, dropped,
          chClosed, queueOpen, connState, closeReason

vars == <<traceId, i, queue, inFlight, senderState, sent, written, dropped,
          chClosed, queueOpen, connState, closeReason>>

Senders == TraceSenders[traceId]
QueueCapacity == TraceCapacity[traceId]
CurrentTrace == Traces[traceId]
Occ(items, s) == Len(SelectSeq(items, LAMBDA e: e = s))
CanSend(s) == senderState[s] = "Idle" /\ sent[s] = 0

Init ==
    /\ traceId \in TraceIds
    /\ i = 1
    /\ queue = <<>>
    /\ inFlight = <<>>
    /\ senderState = [s \in Senders |-> "Idle"]
    /\ sent = [s \in Senders |-> 0]
    /\ written = [s \in Senders |-> 0]
    /\ dropped = [s \in Senders |-> 0]
    /\ chClosed = [s \in Senders |-> 0]
    /\ queueOpen = TRUE
    /\ connState = "Open"
    /\ closeReason = "None"

RequestClose(reason) ==
    closeReason' = IF closeReason = "None" THEN reason ELSE closeReason

SendFast(s) ==
    /\ s \in Senders /\ CanSend(s)
    /\ queueOpen
    /\ Len(queue) < QueueCapacity
    /\ queue' = Append(queue, s)
    /\ sent' = [sent EXCEPT ![s] = 1]
    /\ UNCHANGED <<inFlight, senderState, written, dropped, chClosed,
                    queueOpen, connState, closeReason>>

SendFull(s) ==
    /\ s \in Senders /\ CanSend(s)
    /\ queueOpen
    /\ Len(queue) = QueueCapacity
    /\ sent' = [sent EXCEPT ![s] = 1]
    /\ senderState' = [senderState EXCEPT ![s] = "Parked"]
    /\ UNCHANGED <<queue, inFlight, written, dropped, chClosed,
                    queueOpen, connState, closeReason>>

SendChannelClosed(s) ==
    /\ s \in Senders /\ CanSend(s)
    /\ ~queueOpen
    /\ sent' = [sent EXCEPT ![s] = 1]
    /\ chClosed' = [chClosed EXCEPT ![s] = 1]
    /\ UNCHANGED <<queue, inFlight, senderState, written, dropped,
                    queueOpen, connState, closeReason>>

ParkedEnqueue(s) ==
    /\ s \in Senders /\ senderState[s] = "Parked"
    /\ queueOpen
    /\ Len(queue) < QueueCapacity
    /\ queue' = Append(queue, s)
    /\ senderState' = [senderState EXCEPT ![s] = "Idle"]
    /\ UNCHANGED <<inFlight, sent, written, dropped, chClosed,
                    queueOpen, connState, closeReason>>

ParkedChannelClosed(s) ==
    /\ s \in Senders /\ senderState[s] = "Parked"
    /\ ~queueOpen
    /\ chClosed' = [chClosed EXCEPT ![s] = @ + 1]
    /\ senderState' = [senderState EXCEPT ![s] = "Idle"]
    /\ UNCHANGED <<queue, inFlight, sent, written, dropped,
                    queueOpen, connState, closeReason>>

GraceExpired(s) ==
    /\ s \in Senders /\ senderState[s] = "Parked"
    /\ queueOpen
    /\ dropped' = [dropped EXCEPT ![s] = @ + 1]
    /\ senderState' = [senderState EXCEPT ![s] = "Idle"]
    /\ connState' = "CloseRequested"
    /\ RequestClose("SlowConsumer")
    /\ UNCHANGED <<queue, inFlight, sent, written, chClosed, queueOpen>>

LifecycleClose ==
    /\ connState = "Open"
    /\ connState' = "CloseRequested"
    /\ RequestClose("Lifecycle")
    /\ UNCHANGED <<queue, inFlight, senderState, sent, written, dropped,
                    chClosed, queueOpen>>

QueueClose ==
    /\ connState = "CloseRequested" /\ queueOpen
    /\ queueOpen' = FALSE
    /\ UNCHANGED <<queue, inFlight, senderState, sent, written, dropped,
                    chClosed, connState, closeReason>>

WriterStart(s) ==
    /\ connState = "Open" /\ inFlight = <<>> /\ queue # <<>>
    /\ s = Head(queue)
    /\ inFlight' = <<Head(queue)>>
    /\ queue' = Tail(queue)
    /\ UNCHANGED <<senderState, sent, written, dropped, chClosed,
                    queueOpen, connState, closeReason>>

WriterDrain(s) ==
    /\ connState \in {"Open", "CloseRequested"} /\ queueOpen
    /\ inFlight # <<>>
    /\ s = Head(inFlight)
    /\ written' = [written EXCEPT ![Head(inFlight)] = @ + 1]
    /\ inFlight' = <<>>
    /\ UNCHANGED <<queue, senderState, sent, dropped, chClosed,
                    queueOpen, connState, closeReason>>

CloseFlushStart(s) ==
    /\ connState = "CloseRequested" /\ closeReason = "Lifecycle"
    /\ ~queueOpen /\ inFlight = <<>> /\ queue # <<>>
    /\ s = Head(queue)
    /\ inFlight' = <<Head(queue)>>
    /\ queue' = Tail(queue)
    /\ UNCHANGED <<senderState, sent, written, dropped, chClosed,
                    queueOpen, connState, closeReason>>

CloseFlushDrain(s) ==
    /\ connState = "CloseRequested" /\ closeReason = "Lifecycle"
    /\ ~queueOpen /\ inFlight # <<>> /\ s = Head(inFlight)
    /\ written' = [written EXCEPT ![Head(inFlight)] = @ + 1]
    /\ inFlight' = <<>>
    /\ UNCHANGED <<queue, senderState, sent, dropped, chClosed,
                    queueOpen, connState, closeReason>>

CloseFinish ==
    /\ connState = "CloseRequested" /\ ~queueOpen
    /\ connState' = "Closed"
    /\ dropped' = [s \in Senders |->
                      dropped[s] + Occ(queue, s) + Occ(inFlight, s)]
    /\ queue' = <<>> /\ inFlight' = <<>>
    /\ UNCHANGED <<senderState, sent, written, chClosed, queueOpen, closeReason>>

EventAction(event) ==
    IF TraceActionBug /\ i = 1 THEN "WriterDrain" ELSE event.action

Replay(event) ==
    CASE EventAction(event) = "SendFast" -> SendFast(event.sender)
      [] EventAction(event) = "SendFull" -> SendFull(event.sender)
      [] EventAction(event) = "ParkedEnqueue" -> ParkedEnqueue(event.sender)
      [] EventAction(event) = "GraceExpired" -> GraceExpired(event.sender)
      [] EventAction(event) = "SendChannelClosed" -> SendChannelClosed(event.sender)
      [] EventAction(event) = "ParkedChannelClosed" -> ParkedChannelClosed(event.sender)
      [] EventAction(event) = "LifecycleClose" -> LifecycleClose
      [] EventAction(event) = "QueueClose" -> QueueClose
      [] EventAction(event) = "WriterStart" -> WriterStart(event.sender)
      [] EventAction(event) = "WriterDrain" -> WriterDrain(event.sender)
      [] EventAction(event) = "CloseFlushStart" -> CloseFlushStart(event.sender)
      [] EventAction(event) = "CloseFlushDrain" -> CloseFlushDrain(event.sender)
      [] EventAction(event) = "CloseFinish" -> CloseFinish

TNext ==
    IF i <= Len(CurrentTrace)
    THEN /\ Replay(CurrentTrace[i])
         /\ i' = i + 1
         /\ UNCHANGED traceId
    ELSE UNCHANGED vars

TraceSpec == Init /\ [][TNext]_vars

TypeOK ==
    /\ traceId \in TraceIds /\ i \in 1..(Len(CurrentTrace) + 1)
    /\ queue \in Seq(Senders) /\ Len(queue) <= QueueCapacity
    /\ inFlight \in Seq(Senders) /\ Len(inFlight) <= 1
    /\ senderState \in [Senders -> {"Idle", "Parked"}]
    /\ sent \in [Senders -> 0..1]
    /\ written \in [Senders -> 0..1]
    /\ dropped \in [Senders -> 0..1]
    /\ chClosed \in [Senders -> 0..1]
    /\ queueOpen \in BOOLEAN
    /\ connState \in {"Open", "CloseRequested", "Closed"}
    /\ closeReason \in {"None", "SlowConsumer", "Lifecycle"}

Conservation ==
    \A s \in Senders :
        sent[s] = Occ(queue, s) + Occ(inFlight, s) + written[s] +
                  dropped[s] + chClosed[s] +
                  (IF senderState[s] = "Parked" THEN 1 ELSE 0)

NoSilentLoss ==
    (\E s \in Senders : dropped[s] > 0) =>
        /\ connState \in {"CloseRequested", "Closed"}
        /\ closeReason # "None"

ClosedQueueEmpty ==
    (connState = "Closed") => queue = <<>> /\ inFlight = <<>>

=============================================================================

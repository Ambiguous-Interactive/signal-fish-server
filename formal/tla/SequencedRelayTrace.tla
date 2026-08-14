-------------------------- MODULE SequencedRelayTrace --------------------------
(***************************************************************************)
(* Executable observation-trace refinement for SequencedRelay.tla.         *)
(*                                                                         *)
(* GeneratedSequencedRelayTrace supplies production-shaped receiver        *)
(* observations. Replay admits exactly those observations whose            *)
(* receiver-local projection is a behavior of SequencedRelay's             *)
(* lastObs/justified/accountable contract, strengthened for protocol v3:    *)
(* a skipped same-epoch sequence must be covered by causally prior exact    *)
(* DeliveryGap ranges. Counted ReceiverSnapshot/ReceiverReconnect blocks    *)
(* rebuild complete authoritative views, while reconnect retains pre-cut   *)
(* high-water history for senders that remain members. A false action guard *)
(* deadlocks at the first implementation/specification divergence.         *)
(*                                                                         *)
(* Observable refinement map to SequencedRelay:                            *)
(*   Data                    == Deliver(data)                               *)
(*   PlayerLeft             == Deliver(left)                               *)
(*   PlayerJoined/          == Deliver(rejoin), with the exact new epoch   *)
(*     PlayerReconnected       baseline retained by this stronger model    *)
(*   ReceiverSnapshot +     == establish a complete initial/local view     *)
(*     ReceiverBaseline                                                     *)
(*   ReceiverReconnect +    == Reconnect, with the authoritative v3        *)
(*     ReceiverBaseline        sender-watermark replacement                *)
(*   ReceiverReset          == leave the current room/spectator view       *)
(*   DeliveryGap            == a stronger exact witness for justified      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, GeneratedSequencedRelayTrace

VARIABLES traceId, i, viewOpen, snapshotRemaining, known, present, announced,
          activeEpoch, lastObs, highEpoch, highSeq, pendingGaps,
          terminalKnown, terminalSeq

vars == <<traceId, i, viewOpen, snapshotRemaining, known, present, announced,
          activeEpoch, lastObs, highEpoch, highSeq, pendingGaps,
          terminalKnown, terminalSeq>>

Pairs == TracePairs[traceId]
Receivers == TraceReceivers[traceId]
Epochs == 1..MaxEpoch
SequenceNumbers == 0..MaxSequence
CurrentTrace == Traces[traceId]

Pair(receiver, sender) == <<receiver, sender>>
PairFor(event) == Pair(event.receiver, event.sender)
Range(from, to) == IF from <= to THEN from..to ELSE {}

Init ==
    /\ traceId \in TraceIds
    /\ i = 1
    /\ viewOpen = [r \in Receivers |-> FALSE]
    /\ snapshotRemaining = [r \in Receivers |-> 0]
    /\ known = [p \in Pairs |-> FALSE]
    /\ present = [p \in Pairs |-> FALSE]
    /\ announced = [p \in Pairs |-> {}]
    /\ activeEpoch = [p \in Pairs |-> 0]
    /\ lastObs = [p \in Pairs |-> [e \in Epochs |-> 0]]
    /\ highEpoch = [p \in Pairs |-> 0]
    /\ highSeq = [p \in Pairs |-> 0]
    /\ pendingGaps = [p \in Pairs |-> [e \in Epochs |-> {}]]
    /\ terminalKnown = [p \in Pairs |-> {}]
    /\ terminalSeq = [p \in Pairs |-> [e \in Epochs |-> 0]]

ReceiverReady(event) ==
    /\ event.receiver \in Receivers
    /\ viewOpen[event.receiver]
    /\ snapshotRemaining[event.receiver] = 0

ReceiverBaseline(event) ==
    LET p == PairFor(event)
    IN /\ p \in Pairs
       /\ viewOpen[event.receiver]
       /\ snapshotRemaining[event.receiver] > 0
       /\ ~known[p]
       /\ event.epoch \in Epochs /\ event.value1 \in SequenceNumbers
       /\ \/ highEpoch[p] = 0
          \/ event.epoch > highEpoch[p]
          \/ /\ event.epoch = highEpoch[p]
             /\ event.value1 >= highSeq[p]
       /\ snapshotRemaining' = [snapshotRemaining EXCEPT ![event.receiver] = @ - 1]
       /\ known' = [known EXCEPT ![p] = TRUE]
       /\ present' = [present EXCEPT ![p] = TRUE]
       /\ announced' = [announced EXCEPT ![p] = {event.epoch}]
       /\ activeEpoch' = [activeEpoch EXCEPT ![p] = event.epoch]
       /\ lastObs' = [lastObs EXCEPT ![p][event.epoch] = event.value1]
       /\ highEpoch' = [highEpoch EXCEPT
                            ![p] = IF event.epoch > @ THEN event.epoch ELSE @]
       /\ highSeq' = [highSeq EXCEPT ![p] =
                          IF event.epoch > highEpoch[p]
                              THEN event.value1
                          ELSE IF event.epoch = highEpoch[p] /\ event.value1 > @
                              THEN event.value1
                          ELSE @]
       /\ UNCHANGED <<viewOpen, pendingGaps, terminalKnown, terminalSeq>>

LifecycleBaseline(event, requireKnown) ==
    LET p == PairFor(event)
    IN /\ p \in Pairs /\ ReceiverReady(event)
       /\ IF requireKnown THEN known[p] ELSE TRUE
       /\ ~present[p]
       /\ event.epoch \in Epochs /\ event.value1 \in SequenceNumbers
       /\ (~known[p] \/ \A e \in announced[p] : e < event.epoch)
       /\ (highEpoch[p] = 0 \/ event.epoch > highEpoch[p])
       /\ known' = [known EXCEPT ![p] = TRUE]
       /\ present' = [present EXCEPT ![p] = TRUE]
       /\ announced' = [announced EXCEPT ![p] = @ \cup {event.epoch}]
       /\ activeEpoch' = [activeEpoch EXCEPT
                              ![p] = IF @ = 0 THEN event.epoch ELSE @]
       /\ lastObs' = [lastObs EXCEPT ![p][event.epoch] = event.value1]
       /\ highEpoch' = [highEpoch EXCEPT
                            ![p] = IF event.epoch > @ THEN event.epoch ELSE @]
       /\ highSeq' = [highSeq EXCEPT ![p] =
                          IF event.epoch > highEpoch[p]
                              THEN event.value1
                          ELSE IF event.epoch = highEpoch[p] /\ event.value1 > @
                              THEN event.value1
                          ELSE @]
       /\ UNCHANGED <<viewOpen, snapshotRemaining, pendingGaps,
                       terminalKnown, terminalSeq>>

PlayerReconnected(event) ==
    LET p == PairFor(event)
    IN /\ p \in Pairs /\ ReceiverReady(event) /\ known[p] /\ ~present[p]
       /\ event.epoch \in Epochs /\ event.value1 \in SequenceNumbers
       /\ \A e \in announced[p] : e < event.epoch
       /\ present' = [present EXCEPT ![p] = TRUE]
       /\ announced' = [announced EXCEPT ![p] = @ \cup {event.epoch}]
       /\ lastObs' = [lastObs EXCEPT ![p][event.epoch] = event.value1]
       /\ highEpoch' = [highEpoch EXCEPT ![p] = event.epoch]
       /\ highSeq' = [highSeq EXCEPT ![p] = event.value1]
       /\ UNCHANGED <<viewOpen, snapshotRemaining, known, activeEpoch,
                       pendingGaps, terminalKnown, terminalSeq>>

DeliveryGap(event) ==
    LET p == PairFor(event)
        reported == Range(event.value1, event.value2)
    IN /\ p \in Pairs /\ ReceiverReady(event) /\ known[p]
       /\ event.epoch \in announced[p]
       /\ event.value1 \in 1..MaxSequence
       /\ event.value2 \in 1..MaxSequence
       /\ event.value1 <= event.value2
       /\ event.value1 > lastObs[p][event.epoch]
       /\ reported \cap pendingGaps[p][event.epoch] = {}
       /\ event.epoch >= activeEpoch[p]
       /\ (event.epoch \in terminalKnown[p]
             => event.value2 <= terminalSeq[p][event.epoch])
       /\ pendingGaps' = [pendingGaps EXCEPT
                              ![p][event.epoch] = @ \cup reported]
       /\ UNCHANGED <<viewOpen, snapshotRemaining, known, present, announced,
                       activeEpoch, lastObs, highEpoch, highSeq,
                       terminalKnown, terminalSeq>>

OlderTerminalsResolved(p, epoch) ==
    \A old \in terminalKnown[p] :
        old < epoch => Range(lastObs[p][old] + 1, terminalSeq[p][old])
                          \subseteq pendingGaps[p][old]

Data(event) ==
    LET p == PairFor(event)
        expected == lastObs[p][event.epoch] + 1
        omitted == Range(expected, event.value1 - 1)
    IN /\ p \in Pairs /\ ReceiverReady(event) /\ known[p]
       /\ event.epoch \in announced[p]
       /\ event.value1 \in 1..MaxSequence
       /\ event.epoch >= activeEpoch[p]
       /\ event.value1 >= expected
       /\ omitted \subseteq pendingGaps[p][event.epoch]
       /\ event.value1 \notin pendingGaps[p][event.epoch]
       /\ (event.epoch \in terminalKnown[p]
             => event.value1 <= terminalSeq[p][event.epoch])
       /\ (event.epoch > activeEpoch[p] => OlderTerminalsResolved(p, event.epoch))
       /\ activeEpoch' = [activeEpoch EXCEPT ![p] = event.epoch]
       /\ lastObs' = [lastObs EXCEPT ![p][event.epoch] = event.value1]
       /\ highEpoch' = [highEpoch EXCEPT
                            ![p] = IF event.epoch > @ THEN event.epoch ELSE @]
       /\ highSeq' = [highSeq EXCEPT ![p] =
                          IF event.epoch > highEpoch[p]
                              THEN event.value1
                          ELSE IF event.epoch = highEpoch[p] /\ event.value1 > @
                              THEN event.value1
                          ELSE @]
       /\ pendingGaps' = [pendingGaps EXCEPT
                              ![p][event.epoch] = @ \ omitted]
       /\ UNCHANGED <<viewOpen, snapshotRemaining, known, present, announced,
                       terminalKnown, terminalSeq>>

PlayerLeft(event) ==
    LET p == PairFor(event)
    IN /\ p \in Pairs /\ ReceiverReady(event) /\ known[p] /\ present[p]
       /\ event.epoch \in announced[p]
       /\ event.epoch >= activeEpoch[p]
       /\ event.value1 \in SequenceNumbers
       /\ event.value1 >= lastObs[p][event.epoch]
       /\ \A value \in pendingGaps[p][event.epoch] : value <= event.value1
       /\ present' = [present EXCEPT ![p] = FALSE]
       /\ terminalKnown' = [terminalKnown EXCEPT ![p] = @ \cup {event.epoch}]
       /\ terminalSeq' = [terminalSeq EXCEPT ![p][event.epoch] = event.value1]
       /\ highEpoch' = [highEpoch EXCEPT
                            ![p] = IF event.epoch > @ THEN event.epoch ELSE @]
       /\ highSeq' = [highSeq EXCEPT ![p] =
                          IF event.epoch > highEpoch[p]
                              THEN event.value1
                          ELSE IF event.epoch = highEpoch[p] /\ event.value1 > @
                              THEN event.value1
                          ELSE @]
       /\ UNCHANGED <<viewOpen, snapshotRemaining, known, announced,
                       activeEpoch, lastObs, pendingGaps>>

ClearReceiver(event, preserveHistory, nextOpen, nextRemaining) ==
    /\ event.receiver \in Receivers
    /\ viewOpen' = [viewOpen EXCEPT ![event.receiver] = nextOpen]
    /\ snapshotRemaining' = [snapshotRemaining EXCEPT
                                  ![event.receiver] = nextRemaining]
    /\ known' = [p \in Pairs |-> IF p[1] = event.receiver THEN FALSE ELSE known[p]]
    /\ present' = [p \in Pairs |-> IF p[1] = event.receiver THEN FALSE ELSE present[p]]
    /\ announced' = [p \in Pairs |-> IF p[1] = event.receiver THEN {} ELSE announced[p]]
    /\ activeEpoch' = [p \in Pairs |-> IF p[1] = event.receiver THEN 0 ELSE activeEpoch[p]]
    /\ lastObs' = [p \in Pairs |->
                       IF p[1] = event.receiver
                           THEN [e \in Epochs |-> 0]
                           ELSE lastObs[p]]
    /\ pendingGaps' = [p \in Pairs |->
                           IF p[1] = event.receiver
                               THEN [e \in Epochs |-> {}]
                               ELSE pendingGaps[p]]
    /\ terminalKnown' = [p \in Pairs |->
                             IF p[1] = event.receiver THEN {} ELSE terminalKnown[p]]
    /\ terminalSeq' = [p \in Pairs |->
                           IF p[1] = event.receiver
                               THEN [e \in Epochs |-> 0]
                               ELSE terminalSeq[p]]
    /\ highEpoch' = [p \in Pairs |->
                         IF p[1] = event.receiver /\ ~preserveHistory
                             THEN 0 ELSE highEpoch[p]]
    /\ highSeq' = [p \in Pairs |->
                       IF p[1] = event.receiver /\ ~preserveHistory
                           THEN 0 ELSE highSeq[p]]

ReceiverSnapshot(event) ==
    /\ event.receiver \in Receivers
    /\ ~viewOpen[event.receiver]
    /\ snapshotRemaining[event.receiver] = 0
    /\ event.value1 \in 0..Cardinality({p \in Pairs : p[1] = event.receiver})
    /\ ClearReceiver(event, FALSE, TRUE, event.value1)

ReceiverReconnect(event) ==
    /\ ReceiverReady(event)
    /\ event.value1 \in 0..Cardinality({p \in Pairs : p[1] = event.receiver})
    /\ ClearReceiver(event, TRUE, TRUE, event.value1)

ReceiverReset(event) ==
    /\ ReceiverReady(event)
    /\ ClearReceiver(event, FALSE, FALSE, 0)

Replay(event) ==
    CASE event.action = "ReceiverSnapshot" -> ReceiverSnapshot(event)
      [] event.action = "ReceiverBaseline" -> ReceiverBaseline(event)
      [] event.action = "Data" -> Data(event)
      [] event.action = "DeliveryGap" -> DeliveryGap(event)
      [] event.action = "PlayerLeft" -> PlayerLeft(event)
      [] event.action = "PlayerJoined" -> LifecycleBaseline(event, FALSE)
      [] event.action = "PlayerReconnected" -> PlayerReconnected(event)
      [] event.action = "ReceiverReconnect" -> ReceiverReconnect(event)
      [] event.action = "ReceiverReset" -> ReceiverReset(event)

TNext ==
    IF i <= Len(CurrentTrace)
    THEN /\ Replay(CurrentTrace[i])
         /\ i' = i + 1
         /\ UNCHANGED traceId
    ELSE UNCHANGED vars

TraceSpec == Init /\ [][TNext]_vars

TypeOK ==
    /\ traceId \in TraceIds /\ i \in 1..(Len(CurrentTrace) + 1)
    /\ viewOpen \in [Receivers -> BOOLEAN]
    /\ snapshotRemaining \in [Receivers -> 0..Cardinality(Pairs)]
    /\ known \in [Pairs -> BOOLEAN]
    /\ present \in [Pairs -> BOOLEAN]
    /\ announced \in [Pairs -> SUBSET Epochs]
    /\ activeEpoch \in [Pairs -> 0..MaxEpoch]
    /\ lastObs \in [Pairs -> [Epochs -> SequenceNumbers]]
    /\ highEpoch \in [Pairs -> 0..MaxEpoch]
    /\ highSeq \in [Pairs -> SequenceNumbers]
    /\ pendingGaps \in [Pairs -> [Epochs -> SUBSET (1..MaxSequence)]]
    /\ terminalKnown \in [Pairs -> SUBSET Epochs]
    /\ terminalSeq \in [Pairs -> [Epochs -> SequenceNumbers]]

(* This is the receiver-observation projection of SequencedRelay's         *)
(* GapAccountable invariant. Exact prior ranges strengthen its boolean     *)
(* `justified` witness; active epochs can only have been announced.         *)
SequencedRelayRefinement ==
    /\ \A p \in Pairs : present[p] => known[p]
    /\ \A r \in Receivers : snapshotRemaining[r] > 0 => viewOpen[r]
    /\ \A p \in Pairs : activeEpoch[p] # 0 => activeEpoch[p] \in announced[p]
    /\ \A p \in Pairs : terminalKnown[p] \subseteq announced[p]

=============================================================================

# ADR-0008: Single-Home Consistency and Durability Boundary

## Status

ADR-0008 - Accepted (2026-07-30)

## Context

Signal Fish Server is a zero-external-runtime-dependency, in-memory signaling
and relay server. Existing tests and formal models prove useful guarantees only
when one process owns a room:

- the two-process H5 experiment demonstrates duplicate public room codes,
  stranded reconnect credentials, and rejected cross-instance signals;
- `SequencedRelay.tla` and `ReconnectReplay.tla` contain seeded split-brain
  counterexamples; and
- the H2 relay experiment shows a shared-runner latency knee between the
  120 and 240 message/s/player targets for a 16-player, 1 KiB JSON workload.

The code has extension seams named `DistributedLock`, `MessageCoordinator`, and
`GameDatabase`, plus room-code prefixes and region IDs. None supplies shared
authority, route ownership, sequence ownership, reconnect-token portability, or
live-room migration. Without an explicit decision, those names can be mistaken
for partial horizontal-scaling support.

The reconnect contract also needed a quantitative statement. Gameplay data is
not replayed, so the recoverable object is identity and current control state,
not every payload accepted before or during an outage.

## Decision

The supported product boundary remains **one active home process per room for
the room's lifetime**.

Multiple Signal Fish processes are supported only as isolated routing domains.
An application-owned directory may assign a new room to one deployment and
give every participant the deployment-specific WebSocket URL before the
upgrade. The shipped server does not move a live room, redirect a connection by
room code, or recover a room after its home exits.

Within that boundary:

- successful operations commit to process memory only;
- reconnect restores identity, current control state, and a fresh delivery
  baseline, but never replays `GameData` or `Signal`;
- planned drain closes loudly and requires room rebuild instead of handoff; and
- unexpected process loss loses the room.

The exact per-operation acknowledgement, fault, and recovery semantics live in
the [consistency and durability contract](../architecture/consistency-and-durability.md).

This ADR also adopts an invariant for additional exposure caused by a
disconnect and the following outage:

```text
additional gameplay exposure
    = old queue tail
    + old post-queue, client-unobserved pipeline
    + room frames accepted while absent
```

With an enforced arrival curve `A(T) <= B + ceil(R*T)`, the conditional ceiling
is `Q + P + B + ceil(R*T)`. This does not include delivery-class omissions
already accounted before the cut. There is no default numeric promise because
the server has no room-wide gameplay ingress rate limit and the full
post-queue-to-application pipeline has no configuration-derived frame cap.
`ReconnectLossBound.tla` checks the bounded arithmetic, including the
zero-window burst edge, and its CI-pinned expected failure proves the
post-queue term is necessary.

The broad durability wording in legacy ADR-001 is clarified by this decision:
control-event replay is bounded, gameplay-state recovery belongs to the
application, and no state survives home-process loss. ADR-001's token and replay
mechanisms remain accepted.

## Alternatives considered

### Consistent hashing inside Signal Fish

Rejected as a complete solution. Consistent hashing minimizes key movement when
the server set changes, but it does not replicate room state, transfer a live
WebSocket, fence an old owner, or make `room_code` available before the
WebSocket upgrade. It can be useful inside the external directory for assigning
**new** rooms, never as evidence that live rooms are portable.

### Lease or fencing-token room ownership

Deferred unless a future distributed mode is explicitly adopted. A useful
fencing token requires a shared authority that orders ownership epochs and
every stateful consumer to reject stale epochs. That adds an external,
high-availability control plane and a new failure model. If adopted later, it
belongs on room ownership and handoff—not on each relay payload.

### CRDT room membership

Rejected for the current protocol. Convergent membership does not by itself
provide one sequence owner, atomic reconnect-token claims, ordered control
replay, or a unique session-plan/authority decision. Eventual convergence is
weaker than the current single-home room semantics. CRDTs may be appropriate for
non-authoritative directory metadata in a separate system.

### Consensus or replicated storage in the relay hot path

Rejected. It would add network coordination, external runtime dependencies, and
a distributed failure mode to the latency-sensitive fallback path. The relay
floor remains process-local. Any future replicated control plane must keep
consensus out of per-message fan-out and arrive through a new ADR, protocol,
falsification experiment, and migration plan.

### Persist only `GameDatabase`

Rejected as a durability claim. Restoring room rows without the corresponding
WebSockets, connection routes, replay cursors, reconnect claims, relay epochs,
and session plans cannot restore a live room consistently.

## Consequences

### Positive

- Operators get one explicit consistency and durability contract instead of
  inferring guarantees from implementation names.
- The relay path keeps its zero-external-dependency and low-latency design.
- Failures are honest: reconnect, drain, and process loss have distinct client
  outcomes.
- Future distributed work has a clear bar: it must replace this boundary
  coherently rather than partially replicate one map.

### Negative

- One home is an availability and durability limit. A home crash loses every
  room it hosts.
- Horizontal scale requires an application-owned directory and isolated room
  assignment before connection.
- Reconnecting clients must resynchronize gameplay state at the application
  layer.
- The quantitative disconnect-exposure ceiling needs an enforced burst/rate
  arrival curve and a bound through client observation; defaults alone cannot
  supply either.

## Evidence and references

- [Consistency and durability contract](../architecture/consistency-and-durability.md)
- [Single-instance deployment](../architecture/single-instance-deployment.md)
- [Scaling architecture](../architecture/scaling.md)
- [Formal verification](../architecture/formal-verification.md)
- [Karger et al., _Consistent Hashing and Random Trees_](https://people.csail.mit.edu/karger/Papers/web.pdf)
  (DOI: `10.1145/258533.258660`)
- [Burrows, _The Chubby Lock Service for Loosely-Coupled Distributed Systems_](https://research.google/pubs/the-chubby-lock-service-for-loosely-coupled-distributed-systems/)
- [Shapiro et al., _A Comprehensive Study of Convergent and Commutative Replicated Data Types_](https://inria.hal.science/inria-00555588)
- [Gilbert and Lynch, _Brewer's Conjecture and the Feasibility of Consistent, Available, Partition-Tolerant Web Services_](https://www.comp.nus.edu.sg/~gilbert/pubs/BrewersConjecture-SigAct.pdf)
  (DOI: `10.1145/564585.564601`)

# Protocol v3 Delivery Reliability and Lifecycle Boundaries

## Status

ADR-0006 - Accepted

## Context

Protocol v3 was still pre-release when fault experiments exposed four holes in
the original relay contract:

- a sender's `seq` reset after a new room incarnation was not self-describing;
- lossy/coalescing delivery could not be authorized by aggregate counters;
- reconnect after replay truncation lacked a complete per-sender baseline; and
- deploys and half-open sockets had no prompt, semantic lifecycle boundary.

The revision had to preserve the frozen v2 wire exactly, keep relay as the
universal floor, bound all server memory, and let a client classify every
continuing-stream gap from causally prior wire evidence.

## Decision

Revise the single, unshipped protocol v3 in place. Do not introduce a separate
protocol version for these changes.

### Incarnations and reconnect baselines

Every v3 relayed data frame carries `(epoch, seq)`. `epoch` identifies the
sender's connection/room incarnation and increases when the sender rejoins or
reconnects; `seq` starts again within that epoch. Room lifecycle snapshots
announce epochs before data, and `Reconnected.sender_watermarks` atomically
replaces the reconnecting recipient's baseline for every current sender.

This makes reset and outage boundaries explicit without replaying high-rate
game data. The composed
[`EndToEndGapAccountability.tla`](../../formal/tla/EndToEndGapAccountability.tla)
model proves the watermark is necessary to classify reconnect gaps from socket
observations alone.

### Delivery classes and exact accountability

V3 JSON `GameData` supports three recipient-side delivery classes:

- `reliable` retains bounded backpressure and fails closed;
- keyed `latest` may supersede an older queued value for the same key; and
- `volatile` may be omitted under pressure.

Raw binary data and every v2 delivery remain reliable. Any permitted v3
omission is authorized by a causally prior, bounded `DeliveryReport` carrying
exact `(sender, epoch, inclusive sequence range, reason)` evidence. Aggregate
counters are diagnostic only. A continuing same-epoch gap is valid only when
the union of prior non-overlapping ranges covers every missing sequence.

Priority lifecycle/accountability control may overtake an old data tail.
Clients therefore retain its accounting cursor while suppressing stale
application payload, require a lifecycle announcement for each future epoch,
and treat room/spectator transitions and reconnect watermarks as generation
barriers. The bounded 256-range report rollover and fail-closed queue behavior
follow the formally checked
[`DeliveryClasses.tla`](../../formal/tla/DeliveryClasses.tla) and
[`ControlPriorityDelivery.tla`](../../formal/tla/ControlPriorityDelivery.tla)
contracts.

### Server lifecycle boundaries

Planned shutdown uses a bounded drain: v3 clients may receive `GoingAway`, all
remaining sockets close with `4000 server_shutdown`, new room creation stops,
and no reconnect token is armed for the exiting in-memory instance.

The socket layer sends RFC 6455 Ping probes independently of application
queues. A missing matching Pong by the configured deadline closes with `4003
activity_timeout`; successful replies refresh liveness and produce RTT metrics.
This needs no protocol negotiation or browser/client release because compliant
WebSocket stacks answer protocol Ping automatically.

### Compatibility and future seam

All new fields and messages are negotiated-v3 only. Projection strips them for
pre-v3 recipients, and v2 JSON/MessagePack goldens remain byte-identical.
`ProtocolInfo.transports = ["websocket"]` reserves discovery for a future
server message lane without promising one today.

## Explicit cut list

- **Resume-from-sequence game-data replay:** rejected. Covering a 300-second
  reconnect window for 16 senders at 60 messages/s and 1 KiB/message is about
  281 MiB per room before overhead; a 1 MiB ring covers about 1.1 seconds.
  Clients must handle truncation and application resync anyway. A future
  latest-value-per-key snapshot may be considered only for a demonstrated use
  case. See the [reconnection contract](../concepts/reconnection.md).
- **WebTransport/QUIC relay now:** deferred. The capability seam is present,
  but adding a production lane needs a mature implementation, deployment
  evidence, and a fault/conformance matrix. The current contract advertises
  only `websocket`; see [protocol negotiation](../protocol.md#protocolinfo).
- **Multi-instance room sharding:** rejected for this revision. The shipped
  state and coordination implementations are in-memory; split-brain seeds break
  relay and replay invariants. Use the
  [single-instance deployment contract](../architecture/single-instance-deployment.md).
- **A separate `SeqReset` message:** rejected as redundant. The epoch appears
  on data and lifecycle baselines, while reconnect watermarks replace every
  sender cursor atomically; a second reset mechanism would create competing
  truth sources.

## Consequences

### Positive

- Every continuing v3 gap is either exactly authorized before observation or a
  protocol violation.
- Incarnation and reconnect resets are explicit per sender.
- Lossy classes stay bounded and observable; reliable and v2 behavior remain
  fail-closed.
- Deploys and half-open sockets have semantic, measured close boundaries.
- The frozen v2 wire remains unchanged.

### Negative

- V3 clients maintain per-sender/epoch cursors, exact-range unions, stale-tail
  suppression, and generation barriers.
- Exact reports consume priority-control capacity and can close a connection
  when bounded accountability cannot be preserved.
- Drain deliberately abandons in-memory rooms instead of handing them to a new
  process.

### Mitigations

- The native and browser reference clients and `ConformanceAuditor` implement
  the normative state machine.
- JSON/MessagePack goldens, canonical samples, AsyncAPI, fuzzing, real-socket
  tests, and formal seeded-bug models guard the wire and lifecycle rules.
- V2 projection tests prevent new v3 state from leaking across negotiation.

## Alternatives Considered

### Aggregate loss counters only

Rejected because a counter identifies neither sender, epoch, nor missing range
and cannot authorize a specific gap.

### One superseded-range scalar on the successor frame

Rejected by the `ScalarInPlaceBug` formal trace: interleaved latest keys can
make a successor-local scalar overstate a live value or require reordering.
Exact reports remain causally prior without mutating successor payloads.

### Replay all missed game data

Rejected because bounded memory cannot cover the advertised reconnect window at
game traffic rates, and applications still require state resynchronization for
truncation and process loss.

### Introduce protocol v4

Rejected because v3 was not live. Revising the pre-release version avoids
shipping two overlapping contracts and keeps v2 as the only compatibility
floor.

## References

- [Protocol reference](../protocol.md) — normative wire and client rules
- [Reconnection](../concepts/reconnection.md) — tokens, watermarks, and resync
- [Transport fallback](../architecture/transport-fallback.md) — relay floor
- [Formal verification](../architecture/formal-verification.md) — model scope
- [Single-instance deployment](../architecture/single-instance-deployment.md)
- [ADR-0001](0001-protocol-v3-two-axis.md) — original v3 topology/transport decision

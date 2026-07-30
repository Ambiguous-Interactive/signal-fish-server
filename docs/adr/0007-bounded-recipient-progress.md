# Bounded Recipient Progress and WebSocket Liveness

## Status

ADR-0007 - Accepted

This ADR supersedes only the WebSocket-probe activity rule in ADR-0006; its
delivery, accountability, and lifecycle decisions remain accepted unchanged.

## Context

Issue #217 asked whether accountable delivery should distinguish a recipient
that makes no progress from one that drains continuously below the offered
reliable rate. The distinction matters operationally, but it does not change
the reliable-delivery contract: reliable data may neither be dropped nor
buffered without a bound.

Two real-socket experiments established the boundary. A mixed-encoding
recipient draining at 32 KiB/s survived a 5,000-message burst with exact
accountability and only 2,218 report bytes. A sustained reliable load above the
same drain rate eventually exhausted its bounded delivery budget, while
`volatile` traffic remained live and exactly accounted for every omission.

That sustained experiment also exposed a separate liveness defect. The server
could successfully write application frames into a constrained connection,
then send an RFC 6455 Ping behind those frames and expire its five-second Pong
deadline before the Ping reached the client. It closed with `4003
activity_timeout` even though the outbound writer was still making progress.

## Decision

Keep `4002 slow_consumer` as the single fail-closed delivery-contract outcome.
It covers both a recipient that stops draining and sustained reliable
oversubscription that cannot remain within queue-capacity, capacity-wait, or
maximum-sojourn bounds. The code is not a physical diagnosis, and no new
oversubscription close code or negotiated wire surface is added.

Treat every successfully completed outbound application socket write as
progress for the idle WebSocket probe state. The Ping is still written so a
read-only client can return an automatic Pong and refresh inbound activity, but
the associated deadline is superseded when its Pong may be queued behind
application traffic. This does not remove reverse-path protection: a selected
write that stops progressing or an oldest reliable/control item that exceeds
`websocket.max_sojourn_ms` still closes with `4002`; after outbound progress
stops, the normal Ping/Pong deadline can close an idle connection with `4003`.
If the Ping write itself blocks after application output advanced, it inherits
the earlier `websocket.slow_consumer_timeout_ms` /
`websocket.max_sojourn_ms` `4002` boundary instead of the shorter idle-probe
write timeout. Configuration already requires the capacity-wait timeout to be
below a nonzero activity-reaper timeout, so the delivery owner wins that race.
While outbound progress continues, the separate inbound-activity reaper
(`server.ping_timeout`, 30 seconds by default) owns client-to-server
half-partition detection. Setting that reaper to `0` deliberately removes the
fixed server deadline for that case. Deployments keep a nonzero value above the
Ping interval plus measured worst-case Ping queue/write delay and operational
jitter; otherwise the reaper may legitimately expire a read-only client before
its automatic Pong arrives.

Size queues using measured encoded frame bytes and recipient drain rate:

```text
queue_drain_seconds =
    (socket_bytes_ahead + queue_capacity * encoded_frame_bytes)
    / drain_bytes_per_second
available_queue_bytes =
    max(0, drain_bytes_per_second * max_sojourn_seconds - socket_bytes_ahead)
max_capacity_at_sojourn =
    floor(available_queue_bytes / encoded_frame_bytes)
```

Use `send_queue_capacity` for data and `control_queue_capacity` for lifecycle
and accountability frames. A configured capacity above the calculated maximum
is still a valid burst/memory bound; it is not a guarantee that a completely
full queue drains before maximum sojourn. Include measured bytes already handed
to TCP; `websocket.socket_send_buffer_bytes` is a kernel request, not a portable
statement of usable payload capacity.

## Consequences

### Positive

- Constrained recipients are not falsely closed by a Pong deadline while
  application writes continue to complete.
- Genuine reverse-path stalls remain bounded by queue sojourn, selected socket
  writes, TCP failure, and the idle Ping/Pong probe.
- Existing clients retain stable close-code and protocol semantics.
- Operators can distinguish steady-state capacity from burst storage with
  explicit, lane-correct arithmetic.

### Negative

- `4002 slow_consumer` deliberately does not reveal whether the physical peer
  stopped entirely or remained below the offered reliable rate.
- Outbound application traffic can supersede protocol-Ping Pong deadlines; RTT
  samples are therefore idle-link observations, not a fixed-cadence health
  stream.
- Client-to-server half-partition detection depends on the inbound-activity
  reaper while outbound traffic continues. Deployments that disable
  `server.ping_timeout` accept no fixed server deadline for that case, and
  deployments that size it without Ping delivery headroom can reap a healthy
  read-only client before its automatic Pong arrives.
- Queue sizing requires a representative encoded frame size and measured
  egress rate; configured message slots alone are insufficient.

## Alternatives Considered

### Add a distinct oversubscribed-recipient close code

Rejected because the same bounded reliable-delivery invariant is violated in
both cases, and the server cannot infer a stable physical diagnosis from one
queue timeout. A new code would add client surface without changing recovery:
the physical connection is terminal and the application must reconnect or
reduce offered reliable load.

### Let successful writes coexist with an active Pong deadline

Rejected because a Pong can sit behind already accepted application traffic on
a constrained path. Expiring that probe misclassifies a progressing connection
as idle even though the independent delivery deadline is already responsible
for detecting a reverse-path stall.

### Increase the Pong timeout

Rejected because it only moves the false-positive threshold, couples liveness
to traffic backlog, and weakens detection for genuinely idle dead connections.

## References

- [ADR-0006](0006-protocol-v3-delivery-reliability.md) — delivery classes,
  exact accountability, and lifecycle boundaries
- [Protocol reference](../protocol.md) — delivery and close-code semantics
- [Scaling guide](../architecture/scaling.md) — worked queue geometry and
  directional fault behavior
- [Configuration recipes](../configuration-recipes.md) — operator tuning

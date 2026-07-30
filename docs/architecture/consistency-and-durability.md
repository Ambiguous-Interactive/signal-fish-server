# Consistency and Durability Contract

Signal Fish Server is an in-memory, single-home room authority. This page states
what a successful operation means, which state survives a connection failure,
and which state survives a process failure. It describes the shipped server,
not a future distributed design.

The short version is:

- stateful handlers commit only to one running process; response enqueue,
  socket-write completion, and client observation are later boundaries;
- no room, replay, token, or relay state is persisted to disk or another
  process;
- reconnect restores identity and current room control state, not missed
  gameplay payloads; and
- planned drain is a fail-loud rebuild boundary, not a room handoff.

## Terms

| Term | Meaning in Signal Fish |
| --- | --- |
| Home process | The one running server process authoritative for a room |
| Local commit | The operation's in-memory mutation and required connection-route publication completed on the home process |
| Queue commit | A server frame obtained capacity and was enqueued on a connection's bounded outbound pipeline |
| Write completion | The asynchronous WebSocket writer reported success for that frame; this is not evidence that the client application applied it |
| Connection durability | State retained by the home process after one client socket disappears |
| Process durability | State retained after the home process exits or crashes |

Process durability is **none**. The server has no write-ahead log, replicated
state, or restart recovery. A custom `GameDatabase` implementation alone does
not change that contract because live routes, relay counters, replay buffers,
reconnect claims, and session plans also belong to the process.

## Operation contract

| Operation | Local state / queue commit | Later delivery and connection-loss behavior | Home-process loss or split-home fault | Client-visible evidence |
| --- | --- | --- | --- | --- |
| Create or join room | Admission, route publication, and enqueue of the initial `RoomJoined` baseline commit together; failure to enqueue rolls the unpublished admission back | The writer attempts `RoomJoined` later. An abrupt loss can abandon it even though the room remains committed on the home and may arm reconnect | Process loss removes the room. Routing the same public code to another process can create a second room | An observed `RoomJoined`, typed error, close, or transport failure; silence is not proof of rollback |
| Relay `GameData` | No sender acknowledgement. For v3, acceptance allocates the sender's next `(epoch, seq)` and evaluates one delivery attempt per current recipient | A reliable recipient enqueue can backpressure until the slow-consumer deadline and then close loudly; latest/volatile omissions follow their reported class contract. Anything queued or later in the old pipeline is not replayed after disconnect | No payload, sequence counter, or delivery report survives | v3 stamps, `DeliveryReport`, or close `4002`; v2 has ordered live-connection delivery only |
| WebRTC `Signal` | No sender acknowledgement. After validation and rate limiting, dispatch attempts a reliable target enqueue | A full target queue backpressures dispatch for up to the slow-consumer window, then closes that target; the sender receives no success/failure acknowledgement for the dispatch. Signals are never replayed | A peer on another process is not a valid target | Target may observe `Signal`; validation failures return typed errors to the sender |
| Reconnect | Token claim, identity/route reassignment, room restoration, and enqueue of the `Reconnected` snapshot/fresh baseline commit together | The writer attempts `Reconnected` later. Identity, eligible authority, current membership, and bounded control replay can recover; `GameData` and `Signal` cannot | Reconnect records and tokens are process-local; another process rejects the real credentials | An observed `Reconnected`, `ReconnectionFailed`, close, or transport failure |
| Planned drain | Drain mode commits locally before notifications. `GoingAway` uses a nonblocking best-effort enqueue and may be skipped when the queue is full or gone | The server later requests close `4000 server_shutdown`; the close is authoritative when observed, but a broken transport may expose only failure. Drain does not arm reconnect records | The drained process transfers no room state | Optional v3 `GoingAway`, close `4000`, or transport failure |
| Unexpected process loss | None | Every connection and the room authority disappear together | Room, tokens, control replay, relay counters, and plans are lost | Transport failure only; clients rebuild application state |

Neither queue commit nor WebSocket write completion proves a disk flush,
replica quorum, client application observation, or end-to-end gameplay-state
commit.

## Additional disconnect/outage exposure bound

`Reconnected.missed_events` contains bounded **control** replay only. It never
contains `GameData`, binary game data, or `Signal`. For one reconnecting
recipient, define:

- `Q` — gameplay frames committed to the old connection's outbound data queue
  but not yet dequeued at the cut;
- `P` — dequeued gameplay frames not yet applied by the client application,
  including the server batcher, an active or partial write, kernel/network
  buffers, and the client receive pipeline;
- `A(T)` — room gameplay frames accepted during an absence of duration `T`;
  and
- `E(T)` — additional gameplay exposure introduced by this disconnect and
  absence.

The exact cut is:

```text
E(T) = Q + P + A(T)
```

This is not total historical gameplay loss. It excludes latest/volatile
supersession or other delivery-class omissions already accounted before the
cut, as well as gaps the client had already observed. The online terms contain
only frames that had committed to this recipient's old delivery pipeline.

If admission control or the workload enforces the arrival curve

```text
A(T) <= B + ceil(R * T)
```

where `B` is an instantaneous burst allowance and `R` is a sustained accepted
room-frame rate, then:

```text
E(T) <= Q + P + B + ceil(R * T)
```

This is a bounded-arrivals assumption, not a conclusion from an observed
average rate. It permits up to `B` frames at `T = 0`. For a successful
reconnect, `T` cannot exceed `server.reconnection_window`. The configured data
queue supplies the queue geometry for `Q`. A valid bound for `P` must cover
every post-dequeue stage through client application observation; the configured
socket-buffer byte request alone does not provide it. Control frames use a
separate queue and do not increase `Q`.

The server does not impose a room-wide `GameData` rate limit, so it makes no
absolute numeric disconnect-exposure promise from the default configuration
alone. Operators can use the formula only with an arrival curve and complete
post-queue bound they actually enforce or establish. When senders change
incarnation during the outage, count accepted frames across epochs;
subtracting two `seq` values from different epochs is invalid.

The exhaustive `ReconnectLossBound.tla` model checks the discrete counterpart.
`BURST` is immediately spendable, while each elapsed outage quantum releases
at most `RATE` additional admissions:

```text
reconnectExposure <= QCAP + PCAP + BURST + RATE * WINDOW
```

Its CI-pinned expected-failure configuration omits `PCAP`; TLC must find the
counterexample where a full queue and a full post-queue pipeline are both
abandoned. The zero-window configuration still exercises the immediate burst
but permits no steady-rate admission. See the
[formal verification guide](formal-verification.md#additional-disconnectoutage-exposure-bound).

## Control replay and snapshot healing

Control replay has a different contract:

- `replay: "complete"` means every replayable control event after the recorded
  cursor is present;
- `replay: "truncated"` means the bounded ring evicted at least one required
  event;
- `replay: "unavailable"` means event replay is disabled; and
- in every v3 case, the `Reconnected` membership fields and
  `sender_watermarks` are the authoritative fresh baseline.

The replay suffix is a convenience for applying changes. It is not the source
of truth after reconnect. `ReconnectReplay.tla` proves the completeness status,
and `EndToEndGapAccountability.tla` proves that snapshot replacement plus
per-sender watermark re-baselining heals a truncated or socket-lost tail.

## Client obligations

Clients must:

1. keep every room connection and reconnect on the same home process;
2. treat a reconnect snapshot as replacement state, not only as a delta;
3. perform application-level gameplay-state synchronization after reconnect;
4. treat `4000`, unexpected process loss, and a missing room as rebuild
   boundaries; and
5. use v3 delivery reports and sequence stamps for loss classification, not as
   payload recovery.

## Related documents

- [ADR-0008](../adr/0008-single-home-consistency-boundary.md) — why this is the
  supported product boundary
- [Single-instance deployment](single-instance-deployment.md) — routing and
  proven split-brain failures
- [Reconnection](../concepts/reconnection.md) — client wire flow
- [Scaling](scaling.md) — measured capacity and queue geometry
- [Protocol](../protocol.md) — exact v2/v3 wire contract

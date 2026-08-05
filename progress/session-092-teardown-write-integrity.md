# Session 092 — Teardown write integrity and TURN-lane observability (P49)

## Scope and prioritization

Remote triage found no open pull requests, nine open issues, and — unlike the
last several sessions — **`main` not green**. `TURN-only WebRTC Interop` failed
at `8efa0710`, the merge of PR #277, which is the PR that closed issue #276. Six
of the nine open issues (#204, #205, #206, #207, #213, #220) are open-ended
umbrellas and #250 still needs an owner decision, so the session's two targets
picked themselves:

1. issue #274's second non-timing item — a delivery-accounting oracle reporting
   a message that vanished across a reconnect — because silent message loss is
   the most gameplay-impacting thing open; and
2. the red lane on `main`.

Both turned out to be the same kind of problem: a real defect wearing a flake's
clothes, and a diagnosis that could not be made from what the run recorded.

## Part 1 — the unexplained sequence gap was a production defect

### The report

`Nextest (macos-latest)`, run 30899245389:

```text
thread 'reconnect_under_fire' panicked at tests/websocket_test_helpers/conformance.rs:1546:17:
Victim <- 03f48206-…: unexplained seq gap in epoch 1: expected 90, got 91
```

`seq` and `epoch` are the **server's** stamps, not the test's payload counter, so
this says: the server delivered seq 91 to that recipient and never delivered
seq 90, with no `DeliveryReport` covering it. The Watcher in the same room
required and received the complete 1..100 stream, so seq 90 existed. A hole,
not a truncation — and the conformance contract is explicit that only a prior
exact report may authorize one.

The failure took 0.050 s, which places it in the victim's post-cut tail drain:
the phase-A burst was already queued when the server closed that socket.

### Root cause

`finalize_closed_connection`'s graceful branch drained and wrote everything the
queue still held. That is correct only if nothing was lost first. It can be:

- the live writer loop runs **inside** `run_until_close`, so a close request
  cancels it wherever it is, including inside `send_queued`'s socket write;
- `send_batch` has already `pop_front`ed that payload, so the cancellation takes
  it with the future. `SendAccounting`'s `Drop` counts it — the code comment
  says "the outer close `select!` cannot create an untracked one-message hole" —
  but counting is not accounting: no exact gap is queued, and the recipient is
  told nothing;
- the close branch then wrote seq 91, 92, … onto the same socket.

The repository had already met this schedule from the other side and stopped at
the model boundary. `src/trace_validation.rs` records, on queue close:

```rust
// The send task's close-select cancelled the live socket-write
// future after WriterStart. The base model has no transition for
// a partially written frame, so reject this production schedule
// before a CloseFlushStart could look like a replay divergence.
```

The schedule was known and excluded from formal validation; its client-visible
consequence was never closed.

### Why the loss is real, and why it is ambiguous

`SinkExt::send` is `poll_ready` → `start_send` → `poll_flush`, and
tokio-tungstenite's implementation decides the outcome:

| cancelled in | `self.ready` state | payload |
| --- | --- | --- |
| `poll_flush` | frame already accepted by `start_send` (queued even on `WouldBlock`) | still emitted, in order, by the next flush |
| `poll_ready` | previous `start_send` hit `WouldBlock`, so `ready == false` and this flush blocks | never handed over — **lost** |

So a cancelled write is genuinely indeterminate. It can be neither reported as
an exact gap — that would be a false gap for a frame that did reach the wire,
and the auditor rejects a report covering a delivered sequence just as loudly —
nor assumed delivered.

That ambiguity is what shapes the fix.

### The fix

An abandoned in-flight write latches the connection's queue
(`OutboundReceiver::record_abandoned_in_flight_write`, set from
`SendAccounting`'s unresolved `Drop`). The graceful teardown branch reads the
latch and abandons the remainder — counted by class, exactly as the existing
failed-flush path does — instead of writing past it.

The result is correct in **both** sub-cases above, which is the argument for the
conservative choice rather than a clever one: if the frame was buffered it is
still flushed by the close frame's own write and the client sees `…, 90`; if it
was lost the client sees `…, 89`. Either way a gap-free prefix. Truncation at
close is always legal; a hole is not.

The practical cost is near zero. A write is only cancelled mid-flight when it
was actually pending, which means the socket was already backpressured — exactly
the case whose tail would not have flushed inside the close-write budget anyway.
The loud `SlowConsumer` branch already abandoned its queue and is untouched.

### Red-green

`close_flush_never_writes_the_queue_behind_an_abandoned_write` drives the real
`finalize_closed_connection` against a **real upgraded WebSocket** — a local
axum router hands the server-side `WebSocket` back over a oneshot — and asserts
on the bytes a real client received:

| case | client observes | dropped counter |
| --- | --- | --- |
| healthy teardown | `[1, 2, 3]` | 0 |
| abandoned in-flight write | `[]` | 4 (the payload plus the three behind it) |

With the fence removed and everything else identical, the second case fails:

```text
teardown after an abandoned in-flight write writes nothing behind it: …
  left: [1, 2, 3]
 right: []
```

`only_an_unresolved_socket_write_fences_the_queue_behind_it` pins the other
half, data-driven over all three terminal accounting states. `Written` and
`UnsupportedFormat` must **not** fence: a false positive there would make every
healthy teardown abandon its queue, which would be a worse defect than the one
being fixed.

### Sweep

The class is "sequenced payloads written after an in-flight write was
abandoned". Every other abandonment path already terminates the stream:
`complete_selected_write`'s deadline expiry and the writer's accountability
failure both request `SlowConsumer`, whose branch abandons; a socket error
breaks the loop; and a cancelled `DeliveryReport` write leaves its ranges
pending under the existing peek/write/commit rule from P17/P18. The graceful
close branch was the only continuation.

### Not reproduced locally

40 runs of `reconnect_under_fire` under `taskset -c 0` — the technique that
reproduced previous CI-only timing faults here — all passed. The window needs
the writer parked in `poll_ready` at the instant of the close, which a fast
loopback socket essentially never provides. The mechanism is proved by the
behavioural test above rather than by reproducing the schedule.

## Part 2 — `main` is red on the TURN lane, and the run cannot say why

Run 31015521223 at `8efa0710` failed with issue #276's exact shape:

```text
ERROR webrtc::…::driver: Failed to write packet to 10.254.57.2:3478 from 127.0.0.1:41151:
      io error: Invalid argument (os error 22)
ERROR webrtc::…::turn_relayer: TURN transaction timed out: …   # x2
WARN  rtc_ice::agent: [controlled]: pingAllCandidates called with no candidate pairs.
```

coturn logged no Allocate at all, so nothing reached it, and under
`--ice-transport-policy relay` a session with no reachable source gathers no
candidate whatsoever. P48's routing union did not prevent this run.

What the artifacts could not answer was **why**. The client logged the union only
when it _added_ an address, so a run where the probe contributed nothing was
byte-identical in the log to a run where it never ran; and an ICE server that
neither resolves nor routes was a `debug!` line the lane's captured stderr never
records. On the host side, whether the Docker bridge carried the expected source
address and whether its operational state was up — the exact quantity
`if_addrs::Interface::is_oper_up` reads, and the crux of P48's own root cause —
is unrecoverable from a client log after the fact.

So the first move was evidence, not a second fix:

- the client reports the complete resolved bind set on every pairing
  (`bound`, `enumerated`, `added`, `family`), and warns — not debugs — when a
  configured ICE server does not resolve or does not route;
- `scripts/run-turn-interop.sh` captures the host's `ip route get`, address and
  route tables, and the Docker bridge's link state into the failure artifacts,
  at coturn readiness and again at teardown.

### The diagnostics falsified the recorded cause on their first failing run

That head failed once in seven runs (a ~14% rate on this runner pool), and the
failing run said:

```text
routed a configured ICE server server=10.254.176.2:3478 source=10.254.176.1
resolved the ICE bind set … bound=[10.1.0.97, 10.254.176.1, 127.0.0.1, ::1]
    enumerated={10.1.0.97, 10.254.176.1, 127.0.0.1, ::1} added=[] family="any"
```

and the host agreed with it:

```text
10.254.176.2 dev br-aedb811f8bed src 10.254.176.1
5: br-aedb811f8bed: <BROADCAST,MULTICAST,UP,LOWER_UP> … state UP
    inet 10.254.176.1/24 … scope global br-aedb811f8bed
```

**The routable bridge source was already bound, the bridge was carrier-up, and
interface enumeration had it without help from the probe** (`added=[]`).
Allocates left a correctly routed source and nothing came back. Address
selection — the whole subject of P48 and of #276's recorded cause — is not the
remaining fault.

A second recorded inference also fails a control. Session 091 read "coturn
logged no Allocate at all" as proof that nothing reached it. A **successful**
local run's coturn log carries no Allocate line either: coturn does not log them
at this verbosity. That observation was never evidence.

### Measuring the path instead

The lane now measures its own environment before handing the run to the clients:
one STUN Binding request from the host over `/dev/udp`, which connects the
socket and therefore picks the same source address the client's own route probe
picks — the client's path, not an approximation. Binding needs no credentials,
so a response proves both directions. Locally it returns a full Binding Success
Response on the first attempt:

```text
turn_udp_stun_binding_response=010100142112a44273662d7475726e2d70726f62002000080001a1e02bec1041802800044b71b0e3
turn_udp_reachable_after_attempts=1
```

The gate waits up to 20 s. A path that is merely slow to come up is absorbed; a
path that never comes up fails **here**, with the measurement attached, instead
of as a thirty-second `no candidate pairs` ICE failure that says nothing about
whose fault it is. Either way the next failure is partitioned: gate red means
the environment, gate green means the client or the stack.

It also replaces a log-line proxy with a measurement. The previous readiness
signal was `grep "Relay ports initialization done"` in coturn's log, which
reports _relay port_ initialization — not that coturn's listener will answer.

### What the gate did and did not prove

| head | TURN runs | failures |
| --- | --- | --- |
| diagnostics only (`42e84ec`) | 7 | 1 |
| diagnostics + reachability gate (`4d062a5`) | 41 | 0 |

41 consecutive passes against a measured ~14% rate is about a 1-in-500
coincidence, so the gate is very likely doing something real. It is **not**
proof of mechanism: every one of the 41 answered on the first probe, so the
_wait_ absorbed nothing observable. The leading explanation is the log-line
proxy above — one successful round trip before the clients start is what the
run was missing — and that is stated as the leading explanation, not as a
finding.

### The gate's own hollow guard, found by review

Cursor Bugbot caught the gate defeating itself: `wait_for_turn_udp_reachable`
called `probe_turn_udp` bare and then read `$?`, so under `set -euo pipefail`
the first unanswered probe ended the script before `status` was ever assigned.
The twenty-attempt wait was one attempt and the "cannot measure, stand aside"
path was unreachable — and the defect was invisible in all 41 green runs,
because every probe answered immediately.

Fixed as `status=0; probe_turn_udp || status=$?`, and pinned:
`test_turn_reachability_gate_retries_until_answered` extracts the shipped
function from the script rather than copying it and drives it against a stubbed
probe that answers immediately, answers on the fifth attempt, never answers, and
cannot be run at all. Against the previous gate the answer-on-the-fifth-probe
case exits 1 after a single attempt.

That new test then failed on `Nextest (windows-latest)` — and Bugbot had already
named the reason before the lane ran: the harness interpolated
`Path::display()` into bash, so `C:\Users\…` lost its backslashes to shell
escaping and the counter file was never written (an empty `attempts` value in the
assertion). The harness already runs with the temp directory as its working
directory, so the fix removes path syntax from the shell text entirely rather
than quoting it.

`docker network inspect` confirms an `--internal` network is still assigned its
gateway address (`{"Subnet":"10.253.99.0/24","Gateway":"10.253.99.1"}`), so the
expected source exists by construction — which is consistent with what the
failing run observed.

## What was investigated and deliberately not changed

**#274's first non-timing item** (`reconnect_clears_stored_transport_status`
failing with `PlayerAlreadyConnected`). Session 091 read it as a
test-synchronization gap. Reading the teardown path this session did not confirm
that: `disconnect_client` → `unregister_client` awaits its spawned transaction,
and both the caller's transaction and the connection's own
`unregister_client_with_lifecycle` serialize on the same `ClientLifecycle` lock,
so whichever wins, one `unregister_client_locked` — which ends in
`remove_client_for_unregistration` — has completed before `disconnect_client`
returns. Under that reading `has_client` should already be false and the guard
should not fire. The observed failure therefore has a cause neither session has
identified, it reproduces neither locally nor on demand, and changing the test's
synchronization would be a change without evidence. Left open on #274 with this
analysis added.

## Validation

- Root suite `cargo test --all-features`: **2,047 passed, 0 failed**.
- `cargo fmt` + `cargo clippy --all-targets --all-features -- -D warnings`:
  clean, root and `clients/native`.
- `clients/native`: 89 unit tests green, `cargo fmt --check` and strict Clippy
  clean.
- `scripts/run-turn-interop.sh` against the pinned coturn: positive control and
  mismatched-secret control both pass with the new diagnostics recorded.
- `docs/protocol.md` now states the teardown rule it always implied, on both the
  server-behaviour side and the recipient-obligations side.

## Review

Cursor Bugbot reviewed every head and found two real defects, both in code this
session added and both invisible to the runs that had already passed: the
reachability gate's errexit collapse (41 green runs behind it) and the harness's
Windows path interpolation (which the Windows lane then confirmed
independently). Both are fixed with the replies recorded in their threads, and
Bugbot reports **no new issues** on the final head `d9f1ff3`. Copilot was
quota-blocked on every request (`the user who requested the review has reached
their quota limit`), as on every recent PR.

## Follow-ups

No new issues. Two existing ones were advanced with evidence:

- **#274** — bullet 2 item 2 is fixed here. Item 1 (`PlayerAlreadyConnected` on
  reconnect) stays open, and session 091's reading of it as a
  test-synchronization gap is recorded as falsified rather than carried
  forward. Bullets 1 and 3 (measured per-platform ceilings, PR-lane timing
  placement) are untouched.
- **#276** — reopened in substance: the lane failed again on `main`, its
  recorded cause is falsified, and the issue now carries the bind-set and
  host-routing evidence plus the reachability gate's before/after statistics.

## Publication

PR #279 from branch `agent/session-092-teardown-write-integrity`.

Final head `d9f1ff3`: **14 hosted workflows green**, the Dependabot-only
workflow skipped as designed, and one failure — `Running Copilot Code Review`,
the reviewer's own account quota, which has failed identically on every recent
PR. The `TURN-only WebRTC Interop` lane that is red on `main` is green here,
with **60 consecutive passes** across every head carrying the reachability gate
(41 + 11 + 1 + 7). Left unmerged for the owner.

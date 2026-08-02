# Session 076 — Spectator GC coherence and TURN-only interoperability

## Scope

Advance the next highest gameplay-impact work after P29: first eliminate a
production spectator lifecycle defect discovered during the audit, then close
issue #239's deterministic TURN-only interoperability gap in the same session
PR.

## Spectator lifecycle failure and fix

A failure-first database test proved both GC paths could delete a room whose
player map was empty but whose spectator map still held a connected client.
The durable room disappeared while `SpectatorService` retained the local role,
so voluntary detach failed and later admissions rejected the stranded client.
The defect is tracked as issue #241.

Room occupancy is now defined uniformly as players or spectators. Spectator
join/remove refreshes `last_activity`, spectator-only rooms use the inactive
timeout, and the existing throttled connection-activity seam resolves either a
seated room or spectator room. Inactive-room reconciliation deduplicates room
lookups, binds each detach to the room observed by the sweep, clears stale roles,
and suppresses its terminal notice once shutdown drain begins. Tests cover both
GC paths, join/detach refresh, text, binary, application-Ping, and transport-Pong
activity, a spectator-only active-room/control cleanup pair, post-detach cleanup,
same-room scaling, the leave/rejoin race, and drain-boundary behavior.

## TURN-only operability proof

The native reference client now accepts `--ice-transport-policy relay` and
emits the selected local/remote candidate types after a pair opens. A dedicated
ignored E2E suite is activated by `scripts/run-turn-interop.sh`, which starts
`coturn/coturn:4.12.0-alpine` at manifest digest
`sha256:faca4aa57efc436916c31546f3867bd1a3fb1077723291bcfba0bf814bcaf48a`
on a Docker-internal network without host publication on direct Linux hosts or
in devcontainers.

The positive control runs two real native clients through the real server's
production TURN credential-minting config, proves both selected candidates are
`relay`, checks the exact reliable/unreliable sent and received ledgers, and
checks exact WebSocket relay-floor payloads before a harness release file permits
peer-connection creation. The negative control gives the server a mismatched
secret, proves both clients received coturn allocation `401`, proves no selected
pair exists, observes `TransportStatus{webrtc,false}` plus fallback, and
completes over the same WebSocket floor. The lane performs explicit dependency
provisioning before an offline execution phase, uses no public STUN/TURN service,
and is explicitly not a production-infrastructure validation.

The runner is bounded and fail-closed: it uses a cached digest-pinned image with
`--pull=never`, validates address/port overrides, owns a per-port lock, reaps the
server/coturn processes and Docker network, and writes a fresh exact diagnostic
manifest. Coturn, server, client stderr/events, and test logs are sanitized; a
credential-pattern sentinel exercises the sanitizer before the run and the
final scan rejects static secrets or credential-shaped values.

## Failure-first and local evidence

- The new spectator GC regression failed before the production fix because
  `cleanup_empty_rooms` deleted the occupied room.
- The first coturn run exposed Docker-outside-of-Docker loopback isolation. The
  final design gives coturn a private internal address without host publication;
  a devcontainer joins that same network, while a direct Linux host reaches the
  private bridge address directly. Explicit host overrides remain validated and
  publish only to a bind address that defaults to loopback.
- A current-tree rerun exposed an opaque responder timeout after the relay-floor
  proof. The responder was discarding an early offer from its planned peer while
  its own P2P gate was still closed. Gate-held planned signals are now buffered;
  the harness observes an explicit release event from both peers, disables
  irrelevant mDNS for relay-only operation, and rejects any pair-retry signal.
- The final offline local coturn run passed both the relay-only positive and
  mismatched-secret fallback controls. Its manifest identifies two successful
  server scenarios, four client stderr logs, and four client event logs while
  retaining any absorbed server-start retry diagnostics; cleanup left no coturn
  container, Docker network, or lock.
- The first hosted Ubuntu nextest attempt exposed a real race in the shared
  chaos-proxy test helper: `pause()` could return after the pump read a chunk but
  before its write loop, allowing the paused direction to forward that chunk.
  A writer-preferring per-direction I/O barrier now linearizes pause/resume and
  terminal faults with every socket read and destination write; a post-read or
  mid-fragment pause parks the exact unwritten suffix until resume. Deterministic
  tests observe the destination-write frontier across multiple connections and
  during a mid-fragment kill instead of retrying the original test into silence.
- The final local policy gauntlet exposed that `validate-ci.sh` fed AWK syntax
  checks through anonymous stdin. That executed a valid script's `BEGIN`
  single-input contract with no filename and mislabeled the runtime rejection as
  a syntax error. Syntax checks now pass an explicit empty file, with a policy
  regression preventing the broken invocation shape.
- Final-head hosted Miri exposed a wall-clock assumption in the spectator GC
  control: its 10 ms freshness window could expire while the interpreter moved
  from the active ping to cleanup, so both rooms were reaped. The test now
  backdates both in-memory room clocks by two hours, refreshes the active room
  through the real ping path, and cleans at a one-hour cutoff. The focused test
  passes under the pinned Miri toolchain without retries.
- The same audit reported the date-pinned nightly at 182 days old. Its separate
  Miri/sanitizer/fuzz/unused-dependency upgrade and hosted revalidation are
  tracked in issue #243 rather than expanding this gameplay PR's scope.
- All eleven issue-241 tests pass, including the integrated spectator-only
  traffic/GC control. The existing embedded pause case passes in every
  integration binary, while the active frontier and pending-client regressions
  pass once in the canonical helper binary. Strict root formatting, clippy, and
  the complete locked all-features test suite pass on the final local tree.

## Publication contract

The session remains one PR. Every final push must repeat the available
Cursor/Copilot/human reviewer requests, resolve every non-trivial thread, and
finish with the complete applicable hosted workflow set green. The PR review
and check state is the authoritative publication record; this log records the
failure-first evidence and exact local validation inputs without duplicating
mutable GitHub status.

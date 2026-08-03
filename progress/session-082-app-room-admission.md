# Session 082 — Atomic application room admission

## Scope

Advance issue #249 as P39: make persisted application ownership authoritative
for every room-admission path and enforce configured per-app room/player quotas
atomically. Incorporate the open GitHub Actions dependency update from PR #253
into this single session branch and validate it on the current code head.

## Failure-first evidence

`AppInfo.max_rooms` and `max_players_per_room` had no production consumers.
Existing seated joins read a client app ID but never compared it with
`Room.application_id`; spectator and reconnect admission performed no owner
check. The warn-only ownership helper populated the process cache before
persistence, auth-disabled clients still carried a default `AppInfo`, and the
database exposed only per-game room counts. Consequently, different apps could
share a room and configured quotas did not constrain admission.

## Implementation

Room creation now treats app context as admission authority only when WebSocket
auth is enabled. Persisted application rooms are counted across all game names,
and count-plus-create is serialized under a fixed room-code → application-cap →
game-cap lock order with reverse release. Configured lock or count failures fail
closed. Requested room capacity cannot exceed the app cap, while existing rooms
use the lower of their stored capacity and the current app limit without
ejecting members.

Seated joins refresh persistence inside the room mutation lane before checking
ownership, capacity, or names. Another app receives `ROOM_NOT_FOUND`; a legacy
unowned room is claimed only by the successful seated admission. Unpublished
claims retain durable rollback provenance across detach/ownership persistence
failures, while a later published same-app seated join or reconnect cancels
only the ownership rollback and preserves the required detach. Spectators
authorize from the same persisted owner but never claim or adopt. Reconnect
re-authorizes the new socket's app
after token reservation and releases a wrong-app claim without consuming the
token. Auth-disabled room creation remains unowned. All cleanup paths reconcile
the process-local ownership cache, and missing rooms terminate pending repair.

The documentation now states the exact trust boundary: current clients send
only the public app ID. Ownership and quotas provide accounting and accidental
collision isolation, not hostile-client authentication; issue #250 retains that
separate design decision.

## Local verification

Sixteen focused tests cover cross-app seated/spectator/reconnect denial,
cache-loss authorization, reconnect-token preservation, spectator/non-published
claim behavior, deterministic concurrent legacy claims and cross-game creates,
auth-disabled ownership, player-cap boundaries and lowering, independent apps,
failed-create rollback, quota release after real empty-room cleanup, cache
pruning, and fail-closed/retry-safe lock, count, ownership persistence, detach,
and rollback errors. Cursor's first hosted review identified that reconnect
restoration still used only the room's stored capacity; reconnect now applies
the same current app-cap minimum as seated admission, and a regression proves a
full-cap denial releases rather than consumes the token. The focused suite,
formatting, strict
all-target/all-feature Clippy, and the complete locked all-feature test suite
pass. Independent adversarial code and test audits completed with zero
actionable findings. Cargo-deny, CI configuration, workflow hygiene, MSRV,
documentation, markdown, hook-readiness, policy-test, actionlint, and fast local
CI gates all pass. Hosted CI and external reviewer results remain before P39 is
complete.

Dependabot PR #253's `taiki-e/install-action` and `actions/setup-java` pin bumps
were incorporated as their own commit. The installer succeeded in the stale
PR's failing jobs. The reconnect failure was reproduced as an observer-event /
physical-teardown race and now waits for the idempotent teardown barrier; its
selector passed ten consecutive repetitions. The asymmetric-bandwidth failure
also reproduced locally: a reliable recovery marker was aging behind the
volatile FIFO backlog and could turn the observer into a third slow consumer.
The test now waits for the volatile two-recipient accounting frontier to become
terminal before enqueueing that marker. The exact ignored H10 selector passes
with all semantic and terminal-accounting assertions intact.

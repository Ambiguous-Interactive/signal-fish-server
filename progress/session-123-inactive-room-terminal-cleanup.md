# Session 123 — Terminal inactive-room cleanup convergence

## Scope and prioritization

The default branch was clean, PR #342 had merged with every hosted workflow
green, and no pull request remained open. The active hosted evidence cohorts
P53 and P56 retained four and five of 20 eligible attempts respectively. An
adversarial frontier audit found a higher-priority production correctness
defect: inactive cleanup could delete an occupied room from storage while its
seated clients remained assigned and routed locally, allowing gameplay to
continue through a deleted ghost room.

## Failure-first evidence and root cause

A production-shaped maintenance test created a two-player target room and a
fresh two-player control room, backdated only the target, and ran the real
cleanup task with the independent activity reaper disabled. Against the former
implementation, storage deleted the target but both `get_client_room` values
still returned its ID. The count-only `RoomCleanupOutcome` gave maintenance no
occupant identities, and only spectator roles plus plan/application/ready
caches were reconciled after deletion. JSON and binary gameplay trusted the
stale local assignment and coordinator route without rechecking storage.

## Implementation

Maintenance now groups connected room assignments, checks each unique room
against authoritative storage, and revalidates every missing-room candidate
under the same connection lifecycle gate used by join, leave, reconnect, and
unregister. It then uses the existing room-event lane and atomic terminal relay
watermark transition to clear assignment and routing, discards both pre-issued
and pending reconnect state, and unregisters the socket. The all-client sweep
runs after every room cleanup, so a task canceled after durable deletion can
converge the remaining local state on a later tick; transient storage errors
retain clients for retry.

Affected clients receive a best-effort `ROOM_NOT_FOUND` farewell. The stable
WebSocket close code `4005 room_inactive` is authoritative when that frame
cannot be delivered, and shutdown remains the one close reason that can win a
concurrent drain. Protocol, configuration, client-facing error-code,
conformance-auditor, README, changelog, and close-code documentation now carry
the same contract.

## Review and verification

The regression now proves the target room is absent, both seated assignments
and routes are gone, both connections terminate without reconnect records,
neither JSON nor binary ghost traffic reaches the former peer, and the fresh
control room remains stored, assigned, and able to relay. A real WebSocket test
observes the exact `4005 room_inactive` close frame. The test fails against the
former count-only cleanup path.

The first adversarial review found that the binary payload was sent but the
negative oracle rejected only `GameData`; the assertion was corrected to reject
both `GameData` and `GameDataBinary`. Its follow-up review passed with no
remaining high- or medium-severity finding. A fresh independent final audit was
then requested against the complete task and amended diff. That audit found the
binary negative check still shared a phase whose live control covered only JSON,
so the regression was split into independent JSON and binary phases. Each now
has a fresh-room positive control, and the binary control verifies exact sender,
MessagePack encoding, and payload. The fresh auditor re-reviewed the amendment
and returned a zero-finding PASS.

Local formatting, all-target/all-feature compilation and strict Clippy, the
complete locked all-feature test suite, focused lifecycle and real-socket
regressions, documentation and workflow policy checks, LLM policy checks, hook
readiness plus worktree pre-commit/pre-push checks, cargo-deny, MSRV consistency,
and close-code conformance tests pass. Exact-head hosted CI remains the final
publication gate.

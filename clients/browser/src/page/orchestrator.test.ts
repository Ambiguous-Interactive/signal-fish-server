import {
  ExchangeLedger,
  nextKeepaliveWake,
  scheduleSuccessReleasePoll,
  shouldDeferSuccessAtRunDeadline,
  StartGameGate,
} from './orchestrator.js';

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
}

assert(
  scheduleSuccessReleasePoll(null, true, 123) === null,
  'an in-flight bridge Promise must not arm a zero-delay wake',
);
assert(
  scheduleSuccessReleasePoll(null, false, 123) === 123,
  'an idle barrier must schedule an immediate probe',
);
assert(
  scheduleSuccessReleasePoll(456, false, 123) === 456,
  'an existing bounded poll deadline must be retained',
);
assert(
  nextKeepaliveWake(100, 200) === 200,
  'an overdue ping must not create a zero-delay wake while Pong grace is active',
);
assert(
  nextKeepaliveWake(100, null) === 100,
  'the ping cadence must own keepalive scheduling when no Pong is outstanding',
);
assert(
  shouldDeferSuccessAtRunDeadline(true, true, false, null),
  'a held success barrier must defer an overdue soft deadline',
);
assert(
  shouldDeferSuccessAtRunDeadline(true, true, true, 500),
  'the post-release linger must remain authoritative after the barrier opens',
);
assert(
  !shouldDeferSuccessAtRunDeadline(false, false, false, 500),
  'an ordinary run must retain its bounded soft-deadline behavior',
);
assert(
  !shouldDeferSuccessAtRunDeadline(true, true, true, null),
  'a released barrier with no pending linger must not suppress the soft deadline',
);

const departedPeer = '00000000-0000-0000-0000-000000000002';
const exchangeLedger = new ExchangeLedger();
assert(
  exchangeLedger.unmetCriteria().length === 0,
  'a never-connected peer must not create exchange debt',
);
exchangeLedger.noteConnected(departedPeer);
exchangeLedger.noteSent(departedPeer, 'reliable');
exchangeLedger.noteReceived(departedPeer, 'reliable');
assert(
  exchangeLedger.unmetCriteria().length === 2,
  'a connected departed peer must retain both missing unreliable directions',
);
exchangeLedger.noteSent(departedPeer, 'unreliable');
exchangeLedger.noteReceived(departedPeer, 'unreliable');
assert(
  exchangeLedger.unmetCriteria().length === 0,
  'a completed exchange must remain satisfied after peer departure',
);

// Pins the room creator's explicit-`StartGame` gate against the documented
// `all_ready` semantics (issue #447 F1 / issue #449) — the same scenarios as
// the native `start_game_gate_reissues_after_membership_invalidation` pin.
{
  const creator = '00000000-0000-0000-0000-000000000001';
  const joiner = '00000000-0000-0000-0000-000000000002';
  const present = (...players: string[]) => new Set(players);
  let gate = new StartGameGate([]);

  assert(
    !gate.shouldSend(true, false, present(creator)),
    'an empty readiness baseline must not send',
  );

  // Happy path: the all-ready toggle drives exactly one send, and a repeated
  // broadcast without an invalidation must not duplicate it.
  gate.snapshot([creator]);
  assert(gate.shouldSend(true, false, present(creator)), 'first all-ready snapshot must send');
  gate.noteSent();
  assert(
    !gate.shouldSend(true, false, present(creator)),
    'a repeated all-ready broadcast must not duplicate the send',
  );
  assert(!gate.shouldSend(false, false, present(creator)), 'non-creators never send');
  assert(!gate.shouldSend(true, true, present(creator)), 'a finalized room never sends');

  // Join invalidation: the latecomer is unready with no corrective broadcast;
  // the latch re-arms but the room is provably not all-ready.
  gate.memberJoined(joiner);
  assert(!gate.shouldSend(true, false, present(creator, joiner)), 'right after the join');
  // The joiner's toggle restores an authoritative all-ready snapshot and the
  // creator re-issues.
  gate.snapshot([creator, joiner]);
  assert(
    gate.shouldSend(true, false, present(creator, joiner)),
    "after the joiner's toggle the creator must re-issue",
  );
  gate.noteSent();

  // Authoritative rejection re-arms: a NotReady between snapshot and send
  // means the cached snapshot was stale. Production only queries the gate on
  // the NEXT authoritative frame (toggle or membership change), which carries
  // the refreshed snapshot.
  gate.startRejected();
  gate.snapshot([creator, joiner]);
  assert(gate.shouldSend(true, false, present(creator, joiner)), 'recovery after a rejection');
  gate.noteSent();

  // Departure restoration: the unready member leaves with NO readiness
  // broadcast; membership recomputation alone must re-issue.
  gate.memberLeft(joiner);
  assert(
    gate.shouldSend(true, false, present(creator)),
    'departure restores all-ready without a broadcast',
  );

  // RoomLeft resets the whole baseline.
  gate.noteSent();
  gate.reset();
  assert(!gate.shouldSend(true, false, present(creator)), 'a reset gate must not send');
}

console.error('ok - browser timer and latched-exchange state avoids false success');

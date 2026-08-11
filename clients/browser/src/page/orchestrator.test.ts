import {
  ExchangeLedger,
  nextKeepaliveWake,
  scheduleSuccessReleasePoll,
  shouldDeferSuccessAtRunDeadline,
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

console.error('ok - browser timer and latched-exchange state avoids false success');

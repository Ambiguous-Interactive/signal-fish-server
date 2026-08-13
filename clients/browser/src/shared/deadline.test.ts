import {
  MAX_TIMER_DELAY_MS,
  deadlineAfterSeconds,
  scheduleDeadline,
  type TimeoutHost,
} from './deadline.js';

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
}

const baseMs = 1_000;
const largestUnsaturatedSeconds = Math.floor((Number.MAX_SAFE_INTEGER - baseMs) / 1_000);
const deadlineCases = [
  [0, baseMs, 'zero duration is due at the base instant'],
  [30, 31_000, 'ordinary duration retains exact milliseconds'],
  [
    largestUnsaturatedSeconds,
    baseMs + largestUnsaturatedSeconds * 1_000,
    'largest representable absolute deadline remains exact',
  ],
  [
    largestUnsaturatedSeconds + 1,
    Number.MAX_SAFE_INTEGER,
    'absolute deadline overflow saturates in the distant future',
  ],
  [
    Number.MAX_SAFE_INTEGER,
    Number.MAX_SAFE_INTEGER,
    'largest accepted seconds value saturates in the distant future',
  ],
] as const;

for (const [seconds, expected, description] of deadlineCases) {
  assert(deadlineAfterSeconds(baseMs, seconds) === expected, description);
}

interface PendingTimer {
  callback: () => void;
  delayMs: number;
  canceled: boolean;
}

class FakeTimeoutHost implements TimeoutHost {
  nowMs = 0;
  readonly delays: number[] = [];
  private readonly pending: PendingTimer[] = [];

  now(): number {
    return this.nowMs;
  }

  setTimeout(callback: () => void, delayMs: number): unknown {
    const timer = { callback, delayMs, canceled: false };
    this.delays.push(delayMs);
    this.pending.push(timer);
    return timer;
  }

  clearTimeout(handle: unknown): void {
    (handle as PendingTimer).canceled = true;
  }

  runNext(): void {
    let timer = this.pending.shift();
    while (timer?.canceled === true) {
      timer = this.pending.shift();
    }
    if (timer === undefined) {
      throw new Error('no pending timer');
    }
    this.nowMs += timer.delayMs;
    timer.callback();
  }
}

const scheduleCases = [
  [0, [0], 'zero deadline'],
  [250, [250], 'ordinary deadline'],
  [MAX_TIMER_DELAY_MS, [MAX_TIMER_DELAY_MS], 'exact host timer ceiling'],
  [MAX_TIMER_DELAY_MS + 1, [MAX_TIMER_DELAY_MS, 1], 'one millisecond above the ceiling'],
  [
    MAX_TIMER_DELAY_MS * 2 + 17,
    [MAX_TIMER_DELAY_MS, MAX_TIMER_DELAY_MS, 17],
    'deadline above the host timer ceiling',
  ],
] as const;

for (const [deadlineMs, expectedDelays, description] of scheduleCases) {
  const host = new FakeTimeoutHost();
  let fired = false;
  scheduleDeadline(
    deadlineMs,
    () => {
      fired = true;
    },
    host,
  );
  for (let index = 0; index < expectedDelays.length; index += 1) {
    assert(!fired, `${description} must not fire before chunk ${index + 1}`);
    host.runNext();
  }
  assert(fired, `${description} must fire at its absolute deadline`);
  assert(
    host.delays.join(',') === expectedDelays.join(','),
    `${description} delay chunks must be exact`,
  );
}

const extremeHost = new FakeTimeoutHost();
let extremeFired = false;
const extreme = scheduleDeadline(
  deadlineAfterSeconds(extremeHost.now(), Number.MAX_SAFE_INTEGER),
  () => {
    extremeFired = true;
  },
  extremeHost,
);
assert(
  extremeHost.delays[0] === MAX_TIMER_DELAY_MS,
  'the largest accepted duration must start with one bounded timer chunk',
);
extremeHost.runNext();
assert(!extremeFired, 'the largest accepted duration must not fire after its first chunk');
extreme.cancel();

const canceledHost = new FakeTimeoutHost();
let canceledFired = false;
const canceled = scheduleDeadline(
  MAX_TIMER_DELAY_MS + 1,
  () => {
    canceledFired = true;
  },
  canceledHost,
);
canceled.cancel();
assert(
  canceledHost.delays[0] === MAX_TIMER_DELAY_MS,
  'a distant timer must arm one bounded chunk',
);
let canceledQueueDrained = false;
try {
  canceledHost.runNext();
} catch {
  canceledQueueDrained = true;
}
assert(canceledQueueDrained, 'cancel must remove the outstanding timer chunk');
assert(!canceledFired, 'a canceled deadline must never fire');

console.error('ok - browser deadlines remain exact and host timer delays stay bounded');

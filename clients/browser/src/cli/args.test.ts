import { parseArgs, UsageError } from './args.js';

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
}

const normal = parseArgs(['--server-url', 'ws://127.0.0.1/v3/ws', '--create-room']);
assert(normal !== null, 'normal arguments must parse');
assert(normal?.successReleaseFile === null, 'the success barrier must default off');
assert(
  normal?.config.successReleaseEnabled === false,
  'the page must not wait for a release bridge by default',
);

const held = parseArgs([
  '--server-url',
  'ws://127.0.0.1/v3/ws',
  '--create-room',
  '--success-release-file',
  '/tmp/signal-fish-release',
]);
assert(held !== null, 'success-barrier arguments must parse');
assert(
  held?.successReleaseFile === '/tmp/signal-fish-release',
  'the barrier path must be exact',
);
assert(
  held?.config.successReleaseEnabled === true,
  'the page must hold success when the CLI barrier is configured',
);

const durationFlags = [
  [
    '--p2p-timeout-secs',
    (options: NonNullable<ReturnType<typeof parseArgs>>) => options.config.p2pTimeoutSecs,
  ],
  [
    '--run-for-secs',
    (options: NonNullable<ReturnType<typeof parseArgs>>) => options.config.runForSecs,
  ],
  [
    '--max-runtime-secs',
    (options: NonNullable<ReturnType<typeof parseArgs>>) => options.maxRuntimeSecs,
  ],
] as const;

for (const [flag, read] of durationFlags) {
  for (const [value, expected, description] of [
    ['0', 0, 'zero'],
    ['30', 30, 'ordinary'],
    [String(Number.MAX_SAFE_INTEGER), Number.MAX_SAFE_INTEGER, 'largest precise integer'],
  ] as const) {
    const options = parseArgs([
      '--server-url',
      'ws://127.0.0.1/v3/ws',
      '--create-room',
      flag,
      value,
    ]);
    if (options === null) {
      throw new Error(`${flag} ${description} value must parse`);
    }
    assert(read(options) === expected, `${flag} ${description} value must remain exact`);
  }
}

const impreciseValues = [
  ['9007199254740992', 'larger than Number.MAX_SAFE_INTEGER'],
  ['9007199254740991.1', 'fraction rounded down to Number.MAX_SAFE_INTEGER'],
  ['1.0000000000000001', 'fraction rounded down to one'],
  ['1e-999', 'positive value underflowed to zero'],
] as const;

for (const [flag] of durationFlags) {
  for (const [value, description] of impreciseValues) {
    let rejected = false;
    try {
      parseArgs(['--server-url', 'ws://127.0.0.1/v3/ws', '--create-room', flag, value]);
    } catch (error) {
      rejected = error instanceof UsageError;
    }
    assert(rejected, `${flag} must reject ${description}: ${value}`);
  }
}

console.error('ok - browser CLI arguments preserve exact numeric values');

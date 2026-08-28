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

// --max-players: creator-only capacity above the ready barrier (issue #451).
// Default (absent) stays null so the page falls back to --peers.
const withoutFlag = parseArgs(['--server-url', 'ws://127.0.0.1/v3/ws', '--create-room']);
assert(withoutFlag?.config.maxPlayers === null, 'maxPlayers must default to null');
const raised = parseArgs([
  '--server-url',
  'ws://127.0.0.1/v3/ws',
  '--create-room',
  '--peers',
  '2',
  '--max-players',
  '3',
]);
assert(raised?.config.maxPlayers === 3, '--max-players must remain exact');
for (const [args, description] of [
  [
    ['--join-code', 'ABC123', '--max-players', '3'],
    'joiners adopt the joined room capacity; --max-players requires --create-room',
  ],
  [['--create-room', '--max-players', '0'], 'capacity is u8 on the wire; zero is out of range'],
  [
    ['--create-room', '--max-players', '256'],
    'capacity is u8 on the wire; 256 is out of range',
  ],
  [
    ['--create-room', '--peers', '3', '--max-players', '2'],
    'capacity below the ready barrier deadlocks the run',
  ],
] as const) {
  let rejected = false;
  try {
    parseArgs(['--server-url', 'ws://127.0.0.1/v3/ws', ...args]);
  } catch (error) {
    rejected = error instanceof UsageError;
  }
  assert(rejected, `--max-players must reject: ${description}`);
}

console.error('ok - browser CLI arguments preserve exact numeric values');

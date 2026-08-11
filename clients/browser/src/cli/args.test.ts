import { parseArgs } from './args.js';

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

console.error('ok - browser CLI success-release barrier parses and defaults off');
